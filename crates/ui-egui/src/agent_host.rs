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

impl TurnRequest {
    /// The turn this request describes, with `cancel` as its cancellation.
    ///
    /// One helper so both hosts build the same turn: a field added to `Turn` then reaches both, and the
    /// cancellation token is always the one the host returned to the UI.
    pub fn into_turn_with(self, cancel: figcraft_agent::run::CancelToken) -> figcraft_agent::runtime::Turn {
        figcraft_agent::runtime::Turn {
            turn: self.turn,
            request: self.request,
            document_uid: self.snapshot.uid,
            base_revision: self.snapshot.revision,
            config: self.config,
            limits: self.limits,
            cancel,
            history: self.history,
        }
    }
}

/// The credentials a native host uses, for a driver built without one.
pub fn host_credentials() -> figcraft_agent::EnvironmentCredentials {
    figcraft_agent::EnvironmentCredentials
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

/// One thing the host has to tell the UI, in the order it happened.
///
/// Progress and the ending share **one** queue. They used to be two deliveries -- a list of events
/// and a separate terminal slot -- so the UI could observe a terminal while progress was still
/// arriving, or read an ending that did not match the events before it. One ordered queue makes the
/// ending the last thing after its own progress, by construction.
#[derive(Debug, Clone, PartialEq)]
pub enum HostMessage {
    /// Something that happened while the turn ran.
    Progress(AgentEvent),
    /// How the turn ended. Always the last message of its turn.
    Terminal(TurnTerminal),
    /// A connection test finished.
    ///
    /// Carries the request's id so a result that arrives after Preferences closed, or after a newer
    /// test started, is discarded: showing a stale "ready" for an endpoint the person has since
    /// edited would be worse than showing nothing.
    ConnectionFinished { request: u64, status: figcraft_agent::runtime::ConnectionStatus },
}

impl HostMessage {
    /// The turn this message belongs to, or 0 for a message that is not about a turn.
    pub fn turn(&self) -> u64 {
        match self {
            HostMessage::Progress(e) => e.turn(),
            HostMessage::Terminal(t) => t.turn(),
            // A connection test is not a turn: it has a request id instead.
            HostMessage::ConnectionFinished { .. } => 0,
        }
    }

    /// Whether this message ends its turn.
    pub fn is_terminal(&self) -> bool {
        matches!(self, HostMessage::Terminal(_))
    }
}

/// A platform's way to run Agent turns.
///
/// Implementations own the model, the shadow and the transport; they never borrow the app's
/// `Session` or `FigApp`. `drain` is called once per frame on the UI thread and must not block.
pub trait AgentHost {
    /// Begin a turn. The host emits messages as it runs, and the panel polls them with [`drain`].
    ///
    /// Returns the token that cancels this turn, so the panel can offer Stop.
    ///
    /// [`drain`]: AgentHost::drain
    fn start(&mut self, request: HostRequest) -> CancelToken;

    /// Take the messages produced since the last call, in order. Never blocks.
    ///
    /// Every enqueue also wakes the UI, so a turn that produces output while the pointer is still
    /// repaints without waiting for unrelated input.
    fn drain(&mut self) -> Vec<HostMessage>;

    /// Whether a turn is running.
    ///
    /// Stays `Running` until the terminal message has been **drained**, not merely produced: while the
    /// ending is still queued the turn is not yet finished from the UI's point of view.
    fn status(&self) -> HostStatus;

    /// Begin a connection test, performed off the drawing thread.
    ///
    /// A settings check, not a turn: it builds no shadow, registers no tools and changes no document.
    /// The result arrives as [`HostMessage::ConnectionFinished`], so the window keeps repainting while
    /// the provider is asked.
    ///
    /// Returns the token that stops this test, so closing Preferences can abandon it.
    fn start_connection_test(&mut self, request: u64, config: figcraft_agent::AgentConfig) -> CancelToken {
        // A host with no runtime says so instead of leaving the button spinning.
        let _ = config;
        self.queue_connection_failure(request);
        CancelToken::new()
    }

    /// Report a connection test that could not start, through the same queue as any other result.
    ///
    /// A host that cannot run the test at all still has to settle the UI's persistent state.
    fn queue_connection_failure(&mut self, request: u64) {
        let _ = request;
    }

    /// The egui context to wake whenever a message is queued.
    ///
    /// A turn that streams while the pointer is still must repaint, or its output appears only at the
    /// next input event. Called once with the app's context; a host with no wake yet keeps working.
    fn set_wake(&mut self, ctx: egui::Context) {
        let _ = ctx;
    }

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
    /// What to report instead of running: a failure with the reason.
    queue: Vec<HostMessage>,
}

impl AgentHost for NoAgent {
    fn start(&mut self, request: HostRequest) -> CancelToken {
        // Report a failure with a readable cause rather than pretending to run: the panel then shows
        // "this build has no Agent runtime" instead of streaming nothing. One terminal, in order.
        if let HostRequest::Start(req) = request {
            let turn = req.turn;
            let (kind, message) = no_runtime_message();
            self.queue.push(HostMessage::Terminal(TurnTerminal::Failed { turn, kind, message: message.to_string() }));
        }
        CancelToken::new()
    }

    fn drain(&mut self) -> Vec<HostMessage> {
        std::mem::take(&mut self.queue)
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
