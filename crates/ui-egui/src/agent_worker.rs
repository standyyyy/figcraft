//! The desktop Agent host: one named worker thread that owns the model, the shadow and the turn.
//!
//! The worker never borrows `FigApp` or `Session`. It is given a snapshot (the document as JSON, its
//! uid and revision), builds its own shadow `Session` from it, runs the turn, and reports the events
//! and the replay log. The UI thread drains the events each frame and commits the replay itself, so
//! exactly one thread ever mutates the live document.
//!
//! Turning a model call into document edits therefore has three seams: the UI snapshots, the worker
//! computes, the UI commits. A failure or a cancellation at any point leaves the document untouched,
//! because the replay never leaves the worker unless the turn committed.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use figcraft_agent::run::{AgentEvent, CancelToken, Limits};
use figcraft_agent::{AgentConfig, AgentErrorKind};

use crate::agent_host::{AgentHost, HostMessage, HostRequest, HostStatus, Snapshot};

/// What the worker thread is asked to do.
enum Job {
    /// A turn to run. Boxed for the same reason as [`HostRequest::Start`]: one job per turn, and
    /// the payload is much larger than `Stop`.
    Run(Box<RunJob>),
    Stop,
}

/// One turn's job for the worker.
struct RunJob {
    turn: u64,
    request: String,
    snapshot: Snapshot,
    limits: Limits,
    config: AgentConfig,
    /// Continuation messages from the previous turn in this conversation.
    history: Vec<serde_json::Value>,
    cancel: CancelToken,
}

/// A turn running on a worker thread.
///
/// The worker is started on the first turn and stopped when the host is dropped, so an idle app has
/// no Agent thread at all.
pub struct WorkerHost {
    /// The worker's inbox (`None` before the first turn, or after a stop).
    jobs: Option<Sender<Job>>,
    /// The one queue the worker fills and the UI drains, in order.
    ///
    /// Progress and the ending share it, so the ending is by construction the last thing after its
    /// own progress. It used to be two slots, which allowed a terminal to be visible while progress
    /// was still arriving.
    queue: Arc<Mutex<VecDeque<HostMessage>>>,
    /// Wakes the UI when a message is queued, so output repaints without unrelated input.
    wake: Option<egui::Context>,
    /// The running turn's cancellation flag.
    cancel: Option<CancelToken>,
    /// Whether a turn is in flight.
    running: bool,
    /// The worker thread, joined on drop so app exit cannot hang.
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Default for WorkerHost {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for WorkerHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerHost").field("running", &self.running).finish()
    }
}

/// The name the worker thread is given, so a hang can be found in a debugger.
pub const WORKER_NAME: &str = "figcraft-agent";

impl WorkerHost {
    /// A host with no thread yet: one starts at the first turn.
    pub fn new() -> Self {
        Self { jobs: None, queue: Arc::new(Mutex::new(VecDeque::new())), wake: None, cancel: None, running: false, thread: None }
    }

    /// The same host, waking `ctx` whenever a message arrives.
    ///
    /// The desktop passes the egui context it was created with: a turn that streams while nobody
    /// moves the pointer still has to repaint, or its output appears only at the next input event.
    pub fn with_wake(mut self, ctx: egui::Context) -> Self {
        self.wake = Some(ctx);
        self
    }

    /// Queue one message and wake the UI.
    ///
    /// The host's own path for a message it produces without a worker (no runtime, no thread).
    fn enqueue(&mut self, message: HostMessage) {
        enqueue_message(&self.queue, self.wake.as_ref(), message);
    }

    /// Start the worker if it is not running.
    fn ensure_worker(&mut self) {
        if self.thread.is_some() {
            return;
        }
        let (tx, rx) = channel::<Job>();
        let queue = Arc::clone(&self.queue);
        let wake = self.wake.clone();
        let handle = std::thread::Builder::new()
            .name(WORKER_NAME.into())
            .spawn(move || worker_loop(rx, queue, wake))
            // A thread that cannot start means no Agent; the failure is reported by `start`.
            .ok();
        self.jobs = Some(tx);
        self.thread = handle;
    }

    /// Stop the worker and wait for it, so app exit cannot hang on a turn.
    pub fn shutdown(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        if let Some(jobs) = self.jobs.take() {
            let _ = jobs.send(Job::Stop);
        }
        // The receiver drops when the worker sees Stop; the join then returns promptly.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.running = false;
    }
}

impl Drop for WorkerHost {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl AgentHost for WorkerHost {
    fn start(&mut self, request: HostRequest) -> CancelToken {
        match request {
            HostRequest::Cancel => {
                if let Some(c) = self.cancel.take() {
                    c.cancel();
                }
                self.running = false;
                CancelToken::new()
            }
            HostRequest::Start(req) => {
                let turn = req.turn;
                let cancel = CancelToken::new();
                self.ensure_worker();
                let job = Job::Run(Box::new(RunJob {
                    turn,
                    request: req.request,
                    snapshot: req.snapshot,
                    limits: req.limits,
                    config: req.config,
                    history: req.history,
                    cancel: cancel.clone(),
                }));
                let sent = self.jobs.as_ref().is_some_and(|tx| tx.send(job).is_ok());
                if sent {
                    self.cancel = Some(cancel.clone());
                    self.running = true;
                } else {
                    // No worker: say so rather than leaving the panel streaming forever.
                    let (kind, message) = crate::agent_host::no_runtime_message();
                    self.enqueue(HostMessage::Terminal(figcraft_agent::TurnTerminal::Failed { turn, kind, message: message.to_string() }));
                }
                cancel
            }
        }
    }

    fn drain(&mut self) -> Vec<HostMessage> {
        let messages = match self.queue.lock() {
            Ok(mut q) => q.drain(..).collect::<Vec<_>>(),
            Err(_) => return Vec::new(),
        };
        // The turn is over only once its terminal has been *drained*: while the ending is still
        // queued the UI has not settled it yet, and a second start would race that settlement.
        if messages.iter().any(HostMessage::is_terminal) {
            self.running = false;
            self.cancel = None;
        }
        messages
    }

    fn status(&self) -> HostStatus {
        if self.running { HostStatus::Running } else { HostStatus::Idle }
    }

    /// Keep the app's context, so a queued message repaints without waiting for input.
    fn set_wake(&mut self, ctx: egui::Context) {
        self.wake = Some(ctx);
    }

    /// A description naming the thread, for `ui.inspect` and diagnostics.
    fn describe(&self) -> String {
        self.thread.as_ref().map_or_else(|| "desktop (no worker yet)".to_string(), |t| format!("desktop worker {:?}", t.thread().name()))
    }
}

/// The worker loop: one job at a time, until Stop.
fn worker_loop(rx: Receiver<Job>, queue: Arc<Mutex<VecDeque<HostMessage>>>, wake: Option<egui::Context>) {
    while let Ok(job) = rx.recv() {
        match job {
            Job::Stop => break,
            Job::Run(job) => {
                run_turn(job.turn, &job.request, &job.snapshot, &job.limits, &job.config, &job.history, &job.cancel, &queue, wake.as_ref());
            }
        }
    }
}

/// Run one turn on the worker thread.
///
/// The shadow is built from the snapshot, so the worker never needs the live `Session`. The runtime
/// returns one `TurnTerminal`; only its success variant can carry a replay, which is what keeps a
/// failed, cancelled or limited turn from touching the document.
fn run_turn(
    turn: u64,
    request: &str,
    snapshot: &Snapshot,
    limits: &Limits,
    config: &AgentConfig,
    history: &[serde_json::Value],
    cancel: &CancelToken,
    queue: &Arc<Mutex<VecDeque<HostMessage>>>,
    wake: Option<&egui::Context>,
) {
    // Every message goes through here, so the UI is woken once per message and the order is the order
    // it happened in.
    let enqueue = |message: HostMessage| enqueue_message(queue, wake, message);

    // The shadow first: a document that does not parse is a configuration problem the user can act
    // on, not a crash.
    let session = match crate::agent_host::shadow_session_from_snapshot(snapshot) {
        Ok(s) => s,
        Err(message) => {
            enqueue(HostMessage::Terminal(figcraft_agent::TurnTerminal::Failed { turn, kind: AgentErrorKind::Configuration, message }));
            return;
        }
    };
    let shadow = match figcraft_agent::shadow::Shadow::capture(&session) {
        Ok(s) => s,
        Err(e) => {
            enqueue(HostMessage::Terminal(figcraft_agent::TurnTerminal::Failed { turn, kind: e.kind, message: e.message }));
            return;
        }
    };

    // The key is resolved on this thread and dropped with the client. On the desktop the persisted
    // reference is an environment-variable name; nothing here stores or forwards a key.
    let credentials = figcraft_agent::EnvironmentCredentials;
    let mut emit = |event: AgentEvent| enqueue(HostMessage::Progress(event));
    let terminal = figcraft_agent::runtime::run(
        figcraft_agent::runtime::Turn {
            turn,
            request: request.to_string(),
            document_uid: snapshot.uid,
            base_revision: snapshot.revision,
            config: config.clone(),
            limits: *limits,
            cancel: cancel.clone(),
            history: history.to_vec(),
        },
        shadow,
        &credentials,
        &mut emit,
    );

    // Exactly one terminal, after all of its progress: the ending is the last message of its turn by
    // construction, which is what one queue buys over two slots.
    enqueue(HostMessage::Terminal(terminal));
}

/// Append `message` to `queue` and wake the UI.
///
/// The wake is what makes a turn visible while nothing else happens: without it, output from a turn
/// that streams sits in the queue until the pointer moves or a key is pressed.
fn enqueue_message(queue: &Arc<Mutex<VecDeque<HostMessage>>>, wake: Option<&egui::Context>, message: HostMessage) {
    match queue.lock() {
        Ok(mut q) => q.push_back(message),
        // A poisoned queue still holds the messages ordered before the panic; the turn is over.
        Err(poisoned) => poisoned.into_inner().push_back(message),
    }
    if let Some(ctx) = wake {
        ctx.request_repaint();
    }
}
