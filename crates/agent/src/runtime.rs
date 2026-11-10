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

use std::time::{Duration, Instant};

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
pub fn run(turn: Turn, shadow: Shadow, credentials: &dyn crate::Credentials, emit: &mut dyn FnMut(AgentEvent)) -> TurnTerminal {
    let turn_no = turn.turn;
    if turn.cancel.is_cancelled() {
        return TurnTerminal::Cancelled { turn: turn_no };
    }

    // Rig's agent, with the shared catalog's tools. `preamble` is FigCraft's system prompt: it is part
    // of the behaviour (build first, do not narrate reasoning), and Rig sends it ahead of the chat.
    // The key is resolved here, at the one place a request is built, and dropped with the client.
    // A reference that cannot be resolved fails the turn as a configuration problem rather than
    // sending a request the provider will refuse.
    let credential = match credentials.resolve(turn.config.credential()) {
        Ok(s) => s,
        Err(e) => return TurnTerminal::Failed { turn: turn_no, kind: e.kind, message: e.message },
    };
    if credential.is_none() && turn.config.credential().is_some() {
        let name = turn.config.credential().unwrap_or_default();
        return TurnTerminal::Failed { turn: turn_no, kind: AgentErrorKind::Authentication, message: format!("no credential named `{name}` is set") };
    }
    let openai = OpenAIConfig::new(credential.as_ref().map_or("", crate::Secret::expose)).with_base_url(turn.config.base_url());
    let model = openai.client().chat(turn.config.model().to_string());
    let tools = ShadowTools::new(shadow, ToolPolicy::EmbeddedAgent);
    let dynamic = tools.definitions();
    let agent = AgentBuilder::new(model).preamble(crate::runtime_prompt::SYSTEM_PROMPT).dynamic_tools(dynamic).build();

    emit(AgentEvent::Started { turn: turn_no });
    let started = Instant::now();
    let mut answer = String::new();
    // Counters for FigCraft's limits, which Rig does not know about.
    let mut tool_calls = 0u32;
    let mut rounds = 0u32;
    // Tool names in the order Rig reported their calls: a result names no tool, so the queue pairs
    // them the way the stream emits them (Rig surfaces results in call order).
    let mut pending_tools: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    // Rig's messages from the finished run, handed back for the next turn.
    let mut continuation: Vec<serde_json::Value> = Vec::new();

    // Rig owns the tool loop, so its turn budget is FigCraft's round budget: without this Rig keeps
    // its own default of one model call, which a single tool call exhausts.
    let mut stream = agent.prompt(turn.request.clone()).max_turns(turn.limits.max_rounds as usize).stream();
    let outcome = futures::executor::block_on(async {
        while let Some(item) = stream.next().await {
            // Cancellation is checked between stream items, so a cancel lands promptly even while
            // text streams.
            if turn.cancel.is_cancelled() {
                return StreamOutcome::Cancelled;
            }
            if started.elapsed() >= Duration::from_secs(u64::from(turn.limits.max_seconds)) {
                return StreamOutcome::Limited(AgentErrorKind::Timeout, format!("the turn ran past its {} second limit", turn.limits.max_seconds));
            }
            let item = match item {
                Ok(i) => i,
                // Rig reports a provider or tool failure as the stream's error, which is terminal.
                Err(e) => {
                    let message = e.to_string();
                    let kind = classify(&message);
                    return StreamOutcome::Failed(kind, summarize(&message));
                }
            };
            match item {
                MultiTurnStreamItem::StreamAssistantItem(rig_core::streaming::Item::Event(e)) => match e {
                    // The answer.
                    StreamEvent::Text { text, .. } => {
                        answer.push_str(&text);
                        emit(AgentEvent::TextDelta { turn: turn_no, text });
                    }
                    // A model's scratchpad. Explicitly dropped: Rig keeps it in its own variant, so
                    // this arm exists to say it is not an answer rather than to filter a string.
                    StreamEvent::Reasoning { .. } => {}
                    // A tool part opening or growing is progress Rig will report as a call.
                    _ => {}
                },
                MultiTurnStreamItem::ToolCall { tool_call } => {
                    let name = tool_call.function.name.to_string();
                    pending_tools.push_back(name.clone());
                    emit(AgentEvent::ToolStarted { turn: turn_no, name });
                }
                MultiTurnStreamItem::ToolResult { tool_result: _ } => {
                    tool_calls += 1;
                    if turn.limits.tool_calls_exhausted(tool_calls) {
                        return StreamOutcome::Limited(AgentErrorKind::Tool, format!("the turn used its {} tool calls", turn.limits.max_tool_calls));
                    }
                    // Rig reports the result without the call's name (the call was reported above),
                    // so the last started tool is the one that finished. `ok` is whether the tool
                    // returned rather than refused; a refusal reaches the model as content.
                    let name = pending_tools.pop_front().unwrap_or_else(|| "tool".to_string());
                    emit(AgentEvent::ToolFinished { turn: turn_no, name, ok: true });
                }
                MultiTurnStreamItem::CompletionCall(_) => {
                    rounds += 1;
                    if turn.limits.rounds_exhausted(rounds) {
                        return StreamOutcome::Limited(
                            AgentErrorKind::Model,
                            format!("the turn used its {} rounds without an answer", turn.limits.max_rounds),
                        );
                    }
                }
                MultiTurnStreamItem::FinalResponse(response) => {
                    // Rig's transcript of the run: the prompt, every assistant turn and every
                    // committed tool result. Keeping it is what lets a follow-up continue the
                    // conversation instead of starting over from the visible text.
                    continuation = serde_json::to_value(&response.messages).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
                    return StreamOutcome::Answered;
                }
                _ => {}
            }
        }
        StreamOutcome::Answered
    });

    match outcome {
        StreamOutcome::Answered => finish_success(turn_no, turn.document_uid, turn.base_revision, tools, &turn.request, answer, continuation),
        StreamOutcome::Cancelled => TurnTerminal::Cancelled { turn: turn_no },
        StreamOutcome::Limited(kind, message) => TurnTerminal::LimitReached { turn: turn_no, kind, message },
        StreamOutcome::Failed(kind, message) => TurnTerminal::Failed { turn: turn_no, kind, message },
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
