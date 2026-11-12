//! The turn lifecycle: streaming events, cancellation, retry and recovery.
//!
//! The contract here is FigCraft's, not Rig's: the UI polls [`AgentEvent`]s and never learns what
//! the model transport is. A turn is one [`AgentRun`], which is also what gets persisted, so a
//! reopened file can resume or report an interrupted turn.
//!
//! Two rules shape the code. Cancellation is cooperative and idempotent: a token stops new deltas,
//! prevents any commit, drops the shadow, and emits exactly one terminal event, and a late response
//! that arrives afterwards is ignored. Failures are classified once, at this boundary, so the UI
//! chooses behaviour from a variant rather than from an error string.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Waker;

use crate::AgentErrorKind;

/// Explicit limits on one turn.
///
/// Every one of these is a bound a model can otherwise exceed, so each is checked rather than
/// assumed: a runaway answer must cost a readable error, not the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Model rounds in one turn (a tool result starts another).
    pub max_rounds: u32,
    /// Tool calls in one turn.
    pub max_tool_calls: u32,
    /// Bytes of a single tool's arguments.
    pub max_tool_args_bytes: usize,
    /// Bytes of a single tool's result, which is fed back to the model.
    pub max_tool_result_bytes: usize,
    /// Seconds a whole turn may take.
    pub max_seconds: u32,
    /// Bytes of transcript the turn may add.
    pub max_transcript_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            // Generous enough for a real screen: a model builds a complex layout one or two tools
            // per round, so a dozen rounds cut a prototype off halfway. Still finite, because a model
            // that loops on one tool must not run forever.
            max_rounds: 50,
            max_tool_calls: 120,
            max_tool_args_bytes: 256 * 1024,
            max_tool_result_bytes: 512 * 1024,
            max_seconds: 600,
            max_transcript_bytes: 8 * 1024 * 1024,
        }
    }
}

impl Limits {
    /// Whether this turn should stop before starting round `round`.
    pub fn rounds_exhausted(&self, round: u32) -> bool {
        round >= self.max_rounds
    }

    /// Whether this turn should stop before tool call number `calls`.
    pub fn tool_calls_exhausted(&self, calls: u32) -> bool {
        calls >= self.max_tool_calls
    }
}

/// What the UI learns while a turn runs. **Progress only.**
///
/// How a turn *ends* is [`TurnTerminal`](crate::TurnTerminal) and nothing else. The two were once
/// folded together -- a `Committed`/`Failed`/`Cancelled` event beside a separate terminal slot --
/// which allowed a turn to look ended in one place and unfinished in another, and made "is this
/// stale?" answerable two different ways. A progress event now carries no opinion about the outcome.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentEvent {
    /// A turn started.
    Started { turn: u64 },
    /// A piece of the model's answer, to append to the transcript.
    TextDelta { turn: u64, text: String },
    /// A tool call began.
    ToolStarted { turn: u64, name: String },
    /// A tool call finished (`ok` false: its result was a tool error, which the model still sees).
    ToolFinished { turn: u64, name: String, ok: bool },
    /// A piece of the model's reasoning, for a person who wants to see how it got there.
    ///
    /// Kept separate from [`TextDelta`](Self::TextDelta) so nothing can mistake it for the answer, and
    /// **not** recorded in the transcript: a scratchpad is working material, not part of the
    /// conversation that a later turn should continue from. The panel shows it while the turn runs and
    /// can collapse it; reopening the file shows the answers, not the thinking.
    ReasoningDelta { turn: u64, text: String },
    /// A non-fatal diagnostic: something was adjusted to let the turn continue.
    ///
    /// Not a failure and not an ending. The panel shows it so a person knows the Agent saw a
    /// shortened history, rather than wondering why it forgot an earlier turn.
    Notice { turn: u64, message: String },
}

impl AgentEvent {
    /// The turn this event belongs to.
    pub fn turn(&self) -> u64 {
        match self {
            AgentEvent::Started { turn }
            | AgentEvent::TextDelta { turn, .. }
            | AgentEvent::ToolStarted { turn, .. }
            | AgentEvent::ToolFinished { turn, .. }
            | AgentEvent::ReasoningDelta { turn, .. }
            | AgentEvent::Notice { turn, .. } => *turn,
        }
    }
}

/// A turn's cancellation flag, shared with the thread running it.
///
/// Cheap to clone: the UI holds one and the runner holds one, and either can stop the turn. Setting
/// it twice is harmless, which is what makes cancellation idempotent.
#[derive(Clone, Debug)]
pub struct CancelToken {
    /// The flag, and the one task waiting on it.
    ///
    /// A bare `AtomicBool` cannot wake a task parked in `stream.next().await`, so a browser Stop did
    /// nothing until the provider next sent something. The waker is what makes cancellation *land*.
    state: Arc<CancelState>,
}

/// The cancellation flag plus the waker of whoever is awaiting it.
#[derive(Debug, Default)]
struct CancelState {
    cancelled: AtomicBool,
    /// The waiting task, if any. A `Mutex` rather than `AtomicWaker` keeps this off a `#[doc(hidden)]`
    /// futures API; contention is one cancel and one register.
    waker: Mutex<Option<Waker>>,
}

impl Default for CancelToken {
    fn default() -> Self {
        Self { state: Arc::new(CancelState::default()) }
    }
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the turn to stop. Safe to call more than once, and from any thread.
    ///
    /// Sets the flag first, then wakes the waiter: a waiter that registers between the two sees the
    /// flag on its next poll, so the order is what makes the race safe.
    pub fn cancel(&self) {
        self.state.cancelled.store(true, Ordering::SeqCst);
        let waker = match self.state.waker.lock() {
            Ok(mut w) => w.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Whether the turn was asked to stop.
    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::SeqCst)
    }

    /// Resolves when the turn is asked to stop, waking a parked task when it happens.
    ///
    /// The flag is re-checked **after** the waker is registered: a cancel landing between the first
    /// check and the registration would otherwise be lost, and the task would park forever.
    pub fn cancelled(&self) -> Cancelled<'_> {
        Cancelled { token: self }
    }
}

/// The future [`CancelToken::cancelled`] returns.
///
/// Borrowed rather than owned so a token stays cheap to clone and no allocation is needed per wait.
pub struct Cancelled<'a> {
    token: &'a CancelToken,
}

impl std::future::Future for Cancelled<'_> {
    type Output = ();

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        // Checked first: a cancel before this poll needs no waker at all.
        if self.token.is_cancelled() {
            return std::task::Poll::Ready(());
        }
        // Register, then check again. A cancel in between set the flag and found no waker (or this
        // one), so the second check is what keeps it from being lost.
        match self.token.state.waker.lock() {
            Ok(mut w) => *w = Some(cx.waker().clone()),
            Err(poisoned) => *poisoned.into_inner() = Some(cx.waker().clone()),
        }
        if self.token.is_cancelled() {
            // Take the waker back: nobody will wake this task, and leaving it would keep it alive.
            if let Ok(mut w) = self.token.state.waker.lock() {
                *w = None;
            }
            return std::task::Poll::Ready(());
        }
        std::task::Poll::Pending
    }
}

/// Where a turn is, in a form that survives being written to a file and read back.
///
/// A turn saved mid-flight reopens as [`Interrupted`](Self::Interrupted): its edits were never
/// committed, so it cannot be resumed as if nothing happened, and saying so is more useful than
/// pretending it finished.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRun {
    /// The turn's number, which its events carry.
    pub turn: u64,
    /// What the user asked, for the transcript and the undo label.
    pub request: String,
    /// The answer so far.
    pub answer: String,
    /// The tools that ran, in order, with whether each succeeded.
    pub tools: Vec<ToolCall>,
    /// How the turn ended (`None`: still running when it was saved).
    pub outcome: Option<Outcome>,
}

/// One tool call in a turn's record.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub name: String,
    pub ok: bool,
}

/// How a turn ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    /// The model answered and changed nothing: a read-only turn.
    ///
    /// Distinct from [`Committed`](Self::Committed) because the two are different facts. Reporting a
    /// read-only turn as committed would claim an undo entry that does not exist.
    Completed,
    /// Its edits became one undo step.
    Committed,
    /// The model answered but its edits were not applied: the document moved on, it was the wrong
    /// file, or the batch rolled back. The transcript records this so the next turn does not assume
    /// the rejected edits landed.
    Conflict,
    /// It failed.
    Failed,
    /// It was cancelled.
    Cancelled,
    /// It was saved while running, so it never finished.
    Interrupted,
}

impl AgentRun {
    /// A new run for `request`.
    pub fn new(turn: u64, request: impl Into<String>) -> Self {
        Self { turn, request: request.into(), answer: String::new(), tools: Vec::new(), outcome: None }
    }

    /// Whether the run is still going.
    pub fn is_running(&self) -> bool {
        self.outcome.is_none()
    }

    /// Record a delta, keeping [`Limits::max_transcript_bytes`] from growing without bound.
    pub fn push_text(&mut self, text: &str, limits: &Limits) {
        if self.answer.len() + text.len() > limits.max_transcript_bytes {
            return;
        }
        self.answer.push_str(text);
    }

    /// Record a finished tool call.
    pub fn push_tool(&mut self, name: &str, ok: bool) {
        self.tools.push(ToolCall { name: name.to_string(), ok });
    }

    /// Finish the run.
    pub fn finish(&mut self, outcome: Outcome) {
        if self.outcome.is_none() {
            self.outcome = Some(outcome);
        }
    }

    /// A run read back from a file that never finished becomes [`Outcome::Interrupted`].
    ///
    /// An in-flight turn cannot be resumed (its shadow session is gone and its edits were never
    /// committed), so the honest state is "interrupted", not "running".
    pub fn settle_on_reopen(&mut self) {
        if self.outcome.is_none() {
            self.outcome = Some(Outcome::Interrupted);
        }
    }

    /// Apply one progress event, so the persisted run and the UI cannot disagree.
    ///
    /// Progress only: how the turn ended comes from [`settle`](Self::settle), because a run that
    /// records an outcome from a progress event could disagree with the terminal the host delivers.
    pub fn apply(&mut self, event: &AgentEvent, limits: &Limits) {
        match event {
            AgentEvent::Started { .. } => {}
            AgentEvent::TextDelta { text, .. } => self.push_text(text, limits),
            AgentEvent::ToolStarted { .. } => {}
            AgentEvent::ToolFinished { name, ok, .. } => self.push_tool(name, *ok),
            // Reasoning is working material, not part of the conversation: the transcript records what
            // the model concluded, not how it got there. A later turn continuing from a scratchpad would
            // be re-reading someone else's notes.
            AgentEvent::ReasoningDelta { .. } => {}
            // A diagnostic is not progress and not an ending: the run's record is unchanged.
            AgentEvent::Notice { .. } => {}
        }
    }

    /// Record how the turn ended, from the one value that says so.
    pub fn settle(&mut self, terminal: &crate::TurnTerminal) {
        self.finish(terminal.outcome());
    }
}

/// Whether a failed request should be retried, and how many times.
///
/// Only a failure that could plausibly succeed unchanged is retried: an unreachable provider or a
/// timeout. A configuration, authentication, conflict or cancellation failure will not fix itself,
/// and retrying a conflict would replay against a document the Agent never inspected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts after the first.
    pub retries: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self { retries: 2 }
    }
}

impl RetryPolicy {
    /// Whether `kind` is worth another attempt with `attempt` attempts already made (`0`: none).
    pub fn should_retry(&self, kind: AgentErrorKind, attempt: u32) -> bool {
        kind.is_retryable() && attempt < self.retries
    }
}

/// Why a turn stopped early, as the terminal a host delivers.
pub fn failure_terminal(turn: u64, error: &crate::AgentError) -> crate::TurnTerminal {
    crate::TurnTerminal::Failed { turn, kind: error.kind, message: error.message.clone() }
}
