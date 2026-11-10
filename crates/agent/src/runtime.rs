//! Running a turn through Rig's streaming agent.
//!
//! Rig owns the wire protocol: request construction, SSE decoding, tool-call parsing, the tool loop
//! and final-response assembly. This module maps Rig's stream onto FigCraft's events and enforces
//! FigCraft's limits; it does not parse an OpenAI payload or step a tool loop itself.
//!
//! Two properties come from Rig's own types rather than from our care. **Reasoning is separate from
//! text**: Rig streams `StreamEvent::Text` and `StreamEvent::Reasoning` as distinct variants, so a
//! model's scratchpad cannot reach the panel by being mistaken for an answer -- the mapping below
//! drops `Reasoning` explicitly, and there is no path that would carry it. **The run ends once**:
//! `FinalResponse` is terminal, and an error arrives as the stream's `Err`, so a turn cannot report
//! two endings.
//!
//! The stream is a futures `Stream`. A blocking worker thread drives it with
//! `futures::executor::block_on`, which the probe established works without a tokio runtime.

use figcraft_agent_tools::ToolPolicy;
use futures::StreamExt;
use rig_agent::AgentBuilder;
use rig_agent::agent::MultiTurnStreamItem;
use rig_core::providers::openai::OpenAIConfig;
use rig_core::streaming::StreamEvent;

use crate::AgentErrorKind;
use crate::config::AgentConfig;
use crate::run::{AgentEvent, CancelToken, Limits};
use crate::shadow::Shadow;
use crate::terminal::{CommitEnvelope, TurnTerminal};
use crate::tools::ShadowTools;

/// What a turn needs to run.
pub struct Turn {
    /// The turn's number, which every event carries.
    pub turn: u64,
    /// What the user asked.
    pub request: String,
    /// The document the turn was built from, for the commit envelope.
    pub document_uid: u64,
    /// Its revision when the turn started.
    pub base_revision: u64,
    /// The provider.
    pub config: AgentConfig,
    /// How long and how much.
    pub limits: Limits,
    /// Cooperative cancellation.
    pub cancel: CancelToken,
    /// The messages the previous turn ended with, so a follow-up sees its tool calls and results.
    ///
    /// Already validated by [`crate::history::continuation_messages`]: anything Rig would not accept
    /// has been dropped, so this is either usable or empty.
    pub history: Vec<serde_json::Value>,
}

/// Run one turn, reporting progress through `emit`, and return how it ended.
///
/// The returned [`TurnTerminal`] is the only thing a caller may act on, and only its success variant
/// can hold a [`CommitEnvelope`]. Every other ending drops the shadow: nothing the turn built can
/// reach the document.
/// Run one turn to completion on the calling thread, blocking.
///
/// This is the desktop shape: a worker thread has nothing else to do while the model streams, so it
/// can wait. A browser cannot block its only thread, and uses [`TurnDriver`] to advance the same
/// state machine a slice at a time instead.
pub fn run(turn: Turn, shadow: Shadow, credentials: &dyn crate::Credentials, emit: &mut dyn FnMut(AgentEvent)) -> TurnTerminal {
    let mut driver = TurnDriver::start(turn, shadow, credentials);
    loop {
        match driver.poll() {
            DriverStep::Pending => {
                for event in driver.take_events() {
                    emit(event);
                }
                // Nothing to do but wait for the next stream item.
                if !driver.drive_once() {
                    return driver.finish();
                }
            }
            DriverStep::Done => {
                for event in driver.take_events() {
                    emit(event);
                }
                return driver.finish();
            }
        }
    }
}

/// What one poll of a [`TurnDriver`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriverStep {
    /// More work is waiting on the provider.
    Pending,
    /// The turn ended; call [`TurnDriver::finish`].
    Done,
}

/// A turn the caller advances itself.
///
/// The browser cannot block its thread, so a desktop `block_on` is not available there. This exposes
/// the same state machine: `poll` reports whether the turn is still waiting, `drive_once` advances it
/// by at most one stream item, and `take_events` hands over what happened. Both hosts run this code,
/// so a turn behaves the same in either.
pub struct TurnDriver {
    turn: u64,
    document_uid: u64,
    base_revision: u64,
    request: String,
    limits: Limits,
    cancel: CancelToken,
    tools: ShadowTools,
    /// Rig's stream, driven one item at a time.
    stream: rig_agent::agent::StreamingResult,
    started: std::time::Instant,
    answer: String,
    tool_calls: u32,
    rounds: u32,
    pending_tools: std::collections::VecDeque<String>,
    continuation: Vec<serde_json::Value>,
    /// Events produced by the last step, not yet handed over.
    events: Vec<AgentEvent>,
    /// Set when the turn ended, so `finish` can build the terminal.
    outcome: Option<StreamOutcome>,
}

impl TurnDriver {
    /// Prepare a turn: resolve the key, fit the history, build the agent. No provider call yet.
    pub fn start(turn: Turn, shadow: Shadow, credentials: &dyn crate::Credentials) -> Self {
        let Turn { turn: turn_no, request, document_uid, base_revision, config, limits, cancel, history: stored_history } = turn;
        // A turn cancelled before it starts reports nothing at all: no `Started`, because nothing
        // began, and the caller has the cancellation already.
        if cancel.is_cancelled() {
            return Self::cancelled(turn_no, document_uid, base_revision, request, limits, cancel, shadow);
        }
        let mut events = vec![AgentEvent::Started { turn: turn_no }];

        // The key is resolved here, at the one place a request is built, and dropped with the client.
        let credential = match credentials.resolve(config.credential()) {
            Ok(s) => s,
            Err(e) => return Self::failed(turn_no, document_uid, base_revision, request, limits, cancel, shadow, e.kind, e.message),
        };
        if credential.is_none() && config.credential().is_some() {
            let name = config.credential().unwrap_or_default();
            return Self::failed(
                turn_no,
                document_uid,
                base_revision,
                request,
                limits,
                cancel,
                shadow,
                AgentErrorKind::Authentication,
                format!("no credential named `{name}` is set"),
            );
        }

        // Fit the previous turns into the request budget before they are sent. The system contract is
        // not part of this history, so it is never summarised.
        let (fitted, compaction) =
            crate::history::compact_request(&stored_history, crate::history::MAX_REQUEST_CHARS, crate::history::KEEP_RECENT_TURNS);
        if !compaction.is_empty() {
            events.push(AgentEvent::Notice {
                turn: turn_no,
                message: format!(
                    "earlier turns were shortened to fit the request: {} removed, {} tool results elided",
                    compaction.removed_turns, compaction.elided_results
                ),
            });
        }

        let history = crate::history::continuation_messages(&fitted);
        let openai = OpenAIConfig::new(credential.as_ref().map_or("", crate::Secret::expose)).with_base_url(config.base_url());
        let model = openai.client().chat(config.model().to_string());
        let tools = ShadowTools::new(shadow, ToolPolicy::EmbeddedAgent);
        let dynamic = tools.definitions();
        let agent = AgentBuilder::new(model).preamble(crate::runtime_prompt::SYSTEM_PROMPT).dynamic_tools(dynamic).build();

        let mut runner = agent.prompt(request.clone()).max_turns(limits.max_rounds as usize);
        if !history.is_empty() {
            runner = runner.history(history);
        }
        let stream = runner.stream();

        Self {
            turn: turn_no,
            document_uid,
            base_revision,
            request,
            limits,
            cancel,
            tools,
            stream,
            started: std::time::Instant::now(),
            answer: String::new(),
            tool_calls: 0,
            rounds: 0,
            pending_tools: std::collections::VecDeque::new(),
            continuation: Vec::new(),
            events,
            outcome: None,
        }
    }

    /// An already-finished driver for a turn cancelled before it began: no events, no provider call.
    #[allow(clippy::too_many_arguments)]
    fn cancelled(turn: u64, document_uid: u64, base_revision: u64, request: String, limits: Limits, cancel: CancelToken, shadow: Shadow) -> Self {
        Self {
            turn,
            document_uid,
            base_revision,
            request,
            limits,
            cancel,
            tools: ShadowTools::new(shadow, ToolPolicy::EmbeddedAgent),
            stream: Box::pin(futures::stream::empty()),
            started: std::time::Instant::now(),
            answer: String::new(),
            tool_calls: 0,
            rounds: 0,
            pending_tools: std::collections::VecDeque::new(),
            continuation: Vec::new(),
            events: Vec::new(),
            outcome: Some(StreamOutcome::Cancelled),
        }
    }

    /// A driver that is already finished, for a turn that could not start.
    ///
    /// The browser host builds one when the snapshot cannot be read, so it can report a terminal
    /// through the same path as any other turn instead of needing a special case.
    pub fn from_terminal(terminal: TurnTerminal, shadow: Shadow) -> Self {
        let turn = terminal.turn();
        Self {
            turn,
            document_uid: 0,
            base_revision: 0,
            request: String::new(),
            limits: Limits::default(),
            cancel: CancelToken::new(),
            tools: ShadowTools::new(shadow, ToolPolicy::EmbeddedAgent),
            stream: Box::pin(futures::stream::empty()),
            started: std::time::Instant::now(),
            answer: String::new(),
            tool_calls: 0,
            rounds: 0,
            pending_tools: std::collections::VecDeque::new(),
            continuation: Vec::new(),
            events: Vec::new(),
            outcome: Some(match terminal {
                TurnTerminal::Success { .. } => StreamOutcome::Answered,
                TurnTerminal::Cancelled { .. } => StreamOutcome::Cancelled,
                TurnTerminal::LimitReached { kind, message, .. } => StreamOutcome::Limited(kind, message),
                TurnTerminal::Failed { kind, message, .. } => StreamOutcome::Failed(kind, message),
            }),
        }
    }

    /// A driver for a turn that ends before any provider call.
    #[allow(clippy::too_many_arguments)]
    fn failed(
        turn: u64,
        document_uid: u64,
        base_revision: u64,
        request: String,
        limits: Limits,
        cancel: CancelToken,
        shadow: Shadow,
        kind: AgentErrorKind,
        message: String,
    ) -> Self {
        let event = AgentEvent::Failed { turn, kind, message: message.clone() };
        let outcome = StreamOutcome::Failed(kind, message);
        Self {
            turn,
            document_uid,
            base_revision,
            request,
            limits,
            cancel,
            tools: ShadowTools::new(shadow, ToolPolicy::EmbeddedAgent),
            // An empty stream: nothing will be polled because the outcome is already set.
            stream: Box::pin(futures::stream::empty()),
            started: std::time::Instant::now(),
            answer: String::new(),
            tool_calls: 0,
            rounds: 0,
            pending_tools: std::collections::VecDeque::new(),
            continuation: Vec::new(),
            events: vec![event],
            outcome: Some(outcome),
        }
    }

    /// Whether the turn has ended.
    pub fn is_done(&self) -> bool {
        self.outcome.is_some()
    }

    /// Take the events produced since the last call.
    pub fn take_events(&mut self) -> Vec<AgentEvent> {
        std::mem::take(&mut self.events)
    }

    /// Poll once: whether the turn is waiting or done.
    pub fn poll(&self) -> DriverStep {
        if self.outcome.is_some() { DriverStep::Done } else { DriverStep::Pending }
    }

    /// Advance the stream by one item, emitting what it produced. False when the turn has ended.
    ///
    /// A browser calls this from a local future; a desktop worker from a blocking loop. One item per
    /// call keeps cancellation and the deadline responsive without a second thread.
    pub fn drive_once(&mut self) -> bool {
        if self.outcome.is_some() {
            return false;
        }
        if self.cancel.is_cancelled() {
            self.outcome = Some(StreamOutcome::Cancelled);
            return false;
        }
        if self.started.elapsed() >= std::time::Duration::from_secs(u64::from(self.limits.max_seconds)) {
            self.outcome =
                Some(StreamOutcome::Limited(AgentErrorKind::Timeout, format!("the turn ran past its {} second limit", self.limits.max_seconds)));
            return false;
        }

        let item = match futures::executor::block_on(self.stream.next()) {
            Some(Ok(i)) => i,
            Some(Err(e)) => {
                let message = e.to_string();
                let kind = classify(&message);
                self.outcome = Some(StreamOutcome::Failed(kind, summarize(&message)));
                return false;
            }
            None => {
                self.outcome = Some(StreamOutcome::Answered);
                return false;
            }
        };
        self.handle(item);
        self.outcome.is_none()
    }

    /// Advance the stream by one item without blocking, for a host that awaits.
    ///
    /// This is the browser's shape: a local future awaits the next stream item instead of parking
    /// the only thread the page has. The desktop's [`drive_once`](Self::drive_once) calls this under
    /// `block_on`, so both hosts run the same code.
    pub async fn drive_once_async(&mut self) -> bool {
        if self.outcome.is_some() {
            return false;
        }
        if self.cancel.is_cancelled() {
            self.outcome = Some(StreamOutcome::Cancelled);
            return false;
        }
        if self.started.elapsed() >= std::time::Duration::from_secs(u64::from(self.limits.max_seconds)) {
            self.outcome =
                Some(StreamOutcome::Limited(AgentErrorKind::Timeout, format!("the turn ran past its {} second limit", self.limits.max_seconds)));
            return false;
        }
        let item = match self.stream.next().await {
            Some(Ok(i)) => i,
            Some(Err(e)) => {
                let message = e.to_string();
                let kind = classify(&message);
                self.outcome = Some(StreamOutcome::Failed(kind, summarize(&message)));
                return false;
            }
            None => {
                self.outcome = Some(StreamOutcome::Answered);
                return false;
            }
        };
        self.handle(item);
        self.outcome.is_none()
    }

    /// Apply one stream item.
    fn handle(&mut self, item: MultiTurnStreamItem) {
        match item {
            MultiTurnStreamItem::StreamAssistantItem(rig_core::streaming::Item::Event(e)) => match e {
                // The answer.
                StreamEvent::Text { text, .. } => {
                    self.answer.push_str(&text);
                    self.events.push(AgentEvent::TextDelta { turn: self.turn, text });
                }
                // A model's scratchpad. Explicitly dropped: Rig keeps it in its own variant, so this
                // arm exists to say it is not an answer rather than to filter a string.
                StreamEvent::Reasoning { .. } => {}
                _ => {}
            },
            MultiTurnStreamItem::ToolCall { tool_call } => {
                let name = tool_call.function.name.to_string();
                self.pending_tools.push_back(name.clone());
                self.events.push(AgentEvent::ToolStarted { turn: self.turn, name });
            }
            MultiTurnStreamItem::ToolResult { tool_result: _ } => {
                self.tool_calls += 1;
                if self.limits.tool_calls_exhausted(self.tool_calls) {
                    self.outcome =
                        Some(StreamOutcome::Limited(AgentErrorKind::Tool, format!("the turn used its {} tool calls", self.limits.max_tool_calls)));
                    return;
                }
                let name = self.pending_tools.pop_front().unwrap_or_else(|| "tool".to_string());
                self.events.push(AgentEvent::ToolFinished { turn: self.turn, name, ok: true });
            }
            MultiTurnStreamItem::CompletionCall(_) => {
                self.rounds += 1;
                if self.limits.rounds_exhausted(self.rounds) {
                    self.outcome = Some(StreamOutcome::Limited(
                        AgentErrorKind::Model,
                        format!("the turn used its {} rounds without an answer", self.limits.max_rounds),
                    ));
                }
            }
            MultiTurnStreamItem::FinalResponse(response) => {
                // Rig's transcript of the run: the prompt, every assistant turn and every committed
                // tool result. Keeping it is what lets a follow-up continue the conversation.
                self.continuation = serde_json::to_value(&response.messages).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
                self.outcome = Some(StreamOutcome::Answered);
            }
            _ => {}
        }
    }

    /// Build the terminal for an ended turn, taking the shadow with it.
    pub fn finish(self) -> TurnTerminal {
        let turn = self.turn;
        let outcome = self.outcome.unwrap_or(StreamOutcome::Failed(AgentErrorKind::Protocol, "the turn ended without an outcome".to_string()));
        match outcome {
            StreamOutcome::Answered => {
                finish_success(turn, self.document_uid, self.base_revision, self.tools, &self.request, self.answer, self.continuation)
            }
            StreamOutcome::Cancelled => TurnTerminal::Cancelled { turn },
            StreamOutcome::Limited(kind, message) => TurnTerminal::LimitReached { turn, kind, message },
            StreamOutcome::Failed(kind, message) => TurnTerminal::Failed { turn, kind, message },
        }
    }

    /// The events produced so far, for a driver that finishes in one go.
    pub fn into_parts(self) -> (Vec<AgentEvent>, TurnTerminal) {
        let events = self.events.clone();
        (events, self.finish())
    }

    /// The turn number, which every event carries.
    pub fn turn(&self) -> u64 {
        self.turn
    }
}

/// How the stream ended, before the shadow is taken.
enum StreamOutcome {
    Answered,
    Cancelled,
    Limited(AgentErrorKind, String),
    Failed(AgentErrorKind, String),
}

fn finish_success(
    turn: u64,
    uid: u64,
    revision: u64,
    tools: ShadowTools,
    request: &str,
    answer: String,
    continuation: Vec<serde_json::Value>,
) -> TurnTerminal {
    let replay = tools.into_shadow().map(|s| s.replay().to_vec()).unwrap_or_default();
    let commit = if replay.is_empty() { None } else { Some(CommitEnvelope::new(turn, uid, revision, replay, crate::shadow::undo_label(request))) };
    TurnTerminal::Success { turn, answer, commit, continuation }
}

/// Classify a Rig error by what a person can do about it.
///
/// The message is Rig's own: it is summarized, never dumped, because a provider body can hold the
/// request.
fn classify(message: &str) -> AgentErrorKind {
    let m = message.to_lowercase();
    // Rig wraps transport failures as `HttpError: Http client error: error sending request for url
    // (...)`; a refused connection, a bad host and a DNS failure all read that way, and the message a
    // person can act on is "the provider could not be reached".
    if m.contains("401") || m.contains("403") || m.contains("unauthor") || m.contains("invalid api key") || m.contains("auth") {
        AgentErrorKind::Authentication
    } else if m.contains("timed out") || m.contains("timeout") || m.contains("deadline") {
        AgentErrorKind::Timeout
    } else if m.contains("error sending request")
        || m.contains("connect")
        || m.contains("dns")
        || m.contains("network")
        || m.contains("unreachable")
        || m.contains("connection refused")
        || m.contains("failed to lookup")
    {
        AgentErrorKind::Unreachable
    } else if m.contains("tool") {
        AgentErrorKind::Tool
    } else {
        AgentErrorKind::Protocol
    }
}

fn summarize(s: &str) -> String {
    const MAX: usize = 240;
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let mut out: String = line.chars().take(MAX).collect();
    if line.chars().count() > MAX {
        out.push('…');
    }
    out
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
