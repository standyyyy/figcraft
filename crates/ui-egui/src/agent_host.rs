//! The Agent host: the interface the panel uses to run a turn, implemented by each app target.
//!
//! The UI never owns a runtime. It asks a host to start a turn from a snapshot of the document,
//! polls the events the host produced, and takes the finished turn's replay log to commit on the
//! session owner thread. That keeps one contract for desktop (a named worker thread) and the
//! browser (a local future polled by the frame), so the panel code is the same on both.
//!
//! The contract is deliberately small and synchronous-looking: `start`, `cancel`, `drain`. A host
//! that runs a turn on another thread only ever appends events; the UI thread is the only one that
//! touches a `Session`, which is why the commit is a request rather than a host call.

use figcraft_agent::AgentErrorKind;
use figcraft_agent::TurnTerminal;
use figcraft_agent::run::{AgentEvent, CancelToken, Limits};
use serde_json::Value;

/// What the host is doing, as the panel shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HostStatus {
    /// No turn is running.
    #[default]
    Idle,
    /// A turn is running.
    Running,
}

/// One turn as the host ran it: the events, and (when it committed nothing) the replay the app
/// commits.
#[derive(Clone, Debug, Default)]
pub struct TurnOutcome {
    /// The progress events, in order, up to but not including the terminal one.
    pub events: Vec<AgentEvent>,
    /// How the turn ended, once.
    ///
    /// The replay lives **inside** the successful variant (see [`TurnTerminal`]), so a failure,
    /// cancellation or limit cannot arrive with an edit to apply. The previous shape kept a
    /// `replay: Option<Vec<Value>>` beside a separate terminal event, which allowed exactly that.
    pub terminal: Option<TurnTerminal>,
}

impl TurnOutcome {
    /// Whether the turn ended.
    pub fn is_finished(&self) -> bool {
        self.terminal.is_some()
    }

    /// The envelope to apply, if the turn succeeded with edits.
    ///
    /// The only route to a replay from a host: every other ending returns `None`.
    pub fn commit(&self) -> Option<&figcraft_agent::CommitEnvelope> {
        self.terminal.as_ref().and_then(TurnTerminal::commit)
    }
}

/// Where a snapshot came from: the document, page and selection the Agent may look at.
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// The open document as the format crate writes it: the same JSON a `.figcraft` file's
    /// `document.json` holds, so a host can rebuild a `Document` from it without touching a
    /// `Session`. (Not `inspect_document`'s output: that is a report for a model, not a document.)
    pub document: Value,
    /// The document's uid, so the commit lands in the file the Agent inspected.
    pub uid: u64,
    /// The revision the snapshot was taken at; a commit is refused if the live one moved on.
    pub revision: u64,
}

/// One turn's request.
#[derive(Clone, Debug)]
pub struct TurnRequest {
    pub turn: u64,
    pub request: String,
    pub snapshot: Snapshot,
    pub limits: Limits,
    pub config: figcraft_agent::AgentConfig,
    /// Rig's continuation messages from the previous turn, so a follow-up continues the thread.
    pub history: Vec<serde_json::Value>,
    /// The token that stops this turn, so both hosts honour Stop the same way.
    pub cancel: figcraft_agent::run::CancelToken,
}

/// What a host is asked to do.
pub enum HostRequest {
    /// Run a turn for `request` against `snapshot`.
    ///
    /// Boxed: the config and snapshot together are much larger than `Cancel`, and this is created
    /// once per turn rather than per frame.
    Start(Box<TurnRequest>),
    /// Stop the running turn.
    Cancel,
}

/// A platform's way to run Agent turns.
///
/// Implementations own the model, the shadow and the transport; they never borrow the app's
/// `Session` or `FigApp`. `drain` is called once per frame on the UI thread and must not block.
pub trait AgentHost {
    /// Begin a turn. The host emits events as it runs, and the panel polls them with [`drain`].
    ///
    /// Returns the token that cancels this turn, so the panel can offer Stop.
    ///
    /// [`drain`]: AgentHost::drain
    fn start(&mut self, request: HostRequest) -> CancelToken;

    /// Take the events produced since the last call. Never blocks.
    fn drain(&mut self) -> Vec<AgentEvent>;

    /// How the turn that just finished ended, once.
    ///
    /// The replay travels inside a successful [`TurnTerminal`]; there is no separate channel for it,
    /// so a failure cannot be paired with an edit to apply.
    fn take_terminal(&mut self) -> Option<TurnTerminal>;

    /// Whether a turn is running.
    fn status(&self) -> HostStatus;

    /// A short description for diagnostics (`"desktop worker"`, `"browser"`).
    fn describe(&self) -> String;
}

/// Build a shadow `Session` from a snapshot's document JSON.
///
/// Both hosts do this, so it lives here: the desktop's worker thread and the browser's local future
/// must build the same shadow from the same snapshot, or a turn would behave differently per platform.
pub fn shadow_session_from_snapshot(snapshot: &Snapshot) -> Result<figcraft_engine::Session, String> {
    let doc: figcraft_doc::Document =
        serde_json::from_value(snapshot.document.clone()).map_err(|e| format!("the document snapshot could not be read: {e}"))?;
    Ok(figcraft_engine::Session::with_document(doc))
}

/// A host that refuses every request: what a build without an Agent runtime uses (tests, and the
/// browser until its fetch bridge is wired).
///
/// It still satisfies the contract, so the panel behaves predictably instead of the app needing an
/// `Option` at every call site.
#[derive(Clone, Debug, Default)]
pub struct NoAgent {
    /// Events reported instead of running (a failure with the reason).
    failed: Vec<AgentEvent>,
}

impl AgentHost for NoAgent {
    fn start(&mut self, request: HostRequest) -> CancelToken {
        // Report a failure with a readable cause rather than pretending to run: the panel then shows
        // "this build has no Agent runtime" instead of streaming nothing.
        if let HostRequest::Start(req) = request {
            let turn = req.turn;
            let (kind, message) = no_runtime_message();
            self.failed.push(AgentEvent::Failed { turn, kind, message: message.to_string() });
        }
        CancelToken::new()
    }

    fn drain(&mut self) -> Vec<AgentEvent> {
        std::mem::take(&mut self.failed)
    }

    fn take_terminal(&mut self) -> Option<TurnTerminal> {
        None
    }

    fn status(&self) -> HostStatus {
        HostStatus::Idle
    }

    fn describe(&self) -> String {
        "no Agent runtime".into()
    }
}

/// The message a host reports when it has no runtime.
pub fn no_runtime_message() -> (AgentErrorKind, &'static str) {
    (AgentErrorKind::Configuration, "this build has no Agent runtime; the provider settings are not used")
}
