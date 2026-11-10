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

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use figcraft_agent::run::{AgentEvent, CancelToken, Limits};
use figcraft_agent::{AgentConfig, AgentErrorKind, ModelGateway};

use crate::agent_host::{AgentHost, HostRequest, HostStatus, Snapshot};

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
    cancel: CancelToken,
}

/// A turn running on a worker thread.
///
/// The worker is started on the first turn and stopped when the host is dropped, so an idle app has
/// no Agent thread at all.
pub struct WorkerHost {
    /// The worker's inbox (`None` before the first turn, or after a stop).
    jobs: Option<Sender<Job>>,
    /// Events the worker produced, shared with the UI thread.
    events: Arc<Mutex<Vec<AgentEvent>>>,
    /// How the last finished turn ended, once.
    ///
    /// A [`TurnTerminal`], not a bare replay: only its successful variant can hold an edit, which is
    /// what makes "failed, yet with something to apply" unrepresentable here too.
    terminal: Arc<Mutex<Option<figcraft_agent::TurnTerminal>>>,
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
        Self {
            jobs: None,
            events: Arc::new(Mutex::new(Vec::new())),
            terminal: Arc::new(Mutex::new(None)),
            cancel: None,
            running: false,
            thread: None,
        }
    }

    /// Start the worker if it is not running.
    fn ensure_worker(&mut self) {
        if self.thread.is_some() {
            return;
        }
        let (tx, rx) = channel::<Job>();
        let events = Arc::clone(&self.events);
        let terminal = Arc::clone(&self.terminal);
        let handle = std::thread::Builder::new()
            .name(WORKER_NAME.into())
            .spawn(move || worker_loop(rx, events, terminal))
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
                    cancel: cancel.clone(),
                }));
                let sent = self.jobs.as_ref().is_some_and(|tx| tx.send(job).is_ok());
                if sent {
                    self.cancel = Some(cancel.clone());
                    self.running = true;
                } else {
                    // No worker: say so rather than leaving the panel streaming forever.
                    push_event(
                        &self.events,
                        AgentEvent::Failed { turn, kind: AgentErrorKind::Configuration, message: "the Agent worker could not be started".into() },
                    );
                }
                cancel
            }
        }
    }

    fn drain(&mut self) -> Vec<AgentEvent> {
        let events = match self.events.lock() {
            Ok(mut e) => std::mem::take(&mut *e),
            Err(_) => return Vec::new(),
        };
        // The worker reports the terminal event last; when it arrives the turn is over.
        if events.iter().any(AgentEvent::is_terminal) {
            self.running = false;
            self.cancel = None;
        }
        events
    }

    fn take_terminal(&mut self) -> Option<figcraft_agent::TurnTerminal> {
        self.terminal.lock().ok().and_then(|mut t| t.take())
    }

    fn status(&self) -> HostStatus {
        if self.running { HostStatus::Running } else { HostStatus::Idle }
    }

    /// A description naming the thread, for `ui.inspect` and diagnostics.
    fn describe(&self) -> String {
        self.thread.as_ref().map_or_else(|| "desktop (no worker yet)".to_string(), |t| format!("desktop worker {:?}", t.thread().name()))
    }
}

/// The worker loop: one job at a time, until Stop.
fn worker_loop(rx: Receiver<Job>, events: Arc<Mutex<Vec<AgentEvent>>>, terminal: Arc<Mutex<Option<figcraft_agent::TurnTerminal>>>) {
    while let Ok(job) = rx.recv() {
        match job {
            Job::Stop => break,
            Job::Run(job) => {
                run_turn(job.turn, &job.request, &job.snapshot, &job.limits, &job.config, &job.cancel, &events, &terminal);
            }
        }
    }
}

/// Run one turn on the worker thread.
///
/// The shadow is built from the snapshot, so the worker never needs the live `Session`. A cancelled,
/// failed or stale turn records no replay, which is what keeps the document untouched.
fn run_turn(
    turn: u64,
    request: &str,
    snapshot: &Snapshot,
    limits: &Limits,
    config: &AgentConfig,
    cancel: &CancelToken,
    events: &Arc<Mutex<Vec<AgentEvent>>>,
    terminal_slot: &Arc<Mutex<Option<figcraft_agent::TurnTerminal>>>,
) {
    push_event(events, AgentEvent::Started { turn });

    // The model, built here on the worker thread: the credential is read, used and dropped here.
    let gateway = match ModelGateway::build(config, &figcraft_agent::Environment) {
        Ok(g) => g,
        Err(e) => {
            push_event(events, AgentEvent::Failed { turn, kind: e.kind, message: e.message });
            return;
        }
    };
    let _ = gateway;

    // Build the shadow from the snapshot JSON. A document that does not parse is a configuration
    // problem the user can act on, not a crash.
    let session = match shadow_from_snapshot(snapshot) {
        Ok(s) => s,
        Err(message) => {
            push_event(events, AgentEvent::Failed { turn, kind: AgentErrorKind::Configuration, message });
            return;
        }
    };
    // The shadow owns the copy; tools run against it, never against the live document.
    let shadow = match figcraft_agent::shadow::Shadow::capture(&session) {
        Ok(s) => s,
        Err(e) => {
            push_event(events, AgentEvent::Failed { turn, kind: e.kind, message: e.message });
            return;
        }
    };
    let tools = figcraft_agent::tools::ShadowTools::new(shadow, figcraft_agent_tools::ToolPolicy::EmbeddedAgent);

    if cancel.is_cancelled() {
        push_event(events, AgentEvent::Cancelled { turn });
        return;
    }

    // Drive the turn: ask the model, run the tools it asks for, repeat until it answers or a limit
    // stops it. The loop lives here rather than in the panel so the UI thread never blocks.
    let credential = credential_for(config);
    let transport = figcraft_agent::transport::Transport::new(config.timeout_seconds());
    let mut emit = |event: AgentEvent| push_event(events, event);
    let end = figcraft_agent::transport::run_turn(config, credential.as_deref(), &transport, &tools, turn, request, limits, cancel, &mut emit);

    // One value says how the turn ended, and only its success variant can hold an edit. The shadow is
    // consumed on every path: applying the commands is only ever reachable from `Succeeded`.
    let terminal = match end {
        figcraft_agent::transport::TurnEnd::Answered(answer) => finish_success(tools, turn, snapshot, request, answer),
        figcraft_agent::transport::TurnEnd::Cancelled => {
            drop(tools);
            figcraft_agent::TurnTerminal::Cancelled { turn }
        }
        figcraft_agent::transport::TurnEnd::Stopped(kind, message) => {
            // A turn cut short by a limit or a timeout is not a finished turn: its edits are dropped,
            // because applying half a screen would leave a document nobody agreed to. The message says
            // which limit stopped it.
            drop(tools);
            figcraft_agent::TurnTerminal::LimitReached { turn, kind, message }
        }
    };
    // The panel learns how it ended from the terminal event; the replay travels inside the success.
    match &terminal {
        figcraft_agent::TurnTerminal::Success { .. } => {
            push_event(events, AgentEvent::Committed { turn, undo_label: figcraft_agent::shadow::undo_label(request) })
        }
        figcraft_agent::TurnTerminal::Failed { kind, message, .. } => {
            push_event(events, AgentEvent::Failed { turn, kind: *kind, message: message.clone() })
        }
        figcraft_agent::TurnTerminal::Cancelled { .. } => push_event(events, AgentEvent::Cancelled { turn }),
        figcraft_agent::TurnTerminal::LimitReached { kind, message, .. } => {
            push_event(events, AgentEvent::Failed { turn, kind: *kind, message: message.clone() })
        }
    }
    if let Ok(mut slot) = terminal_slot.lock() {
        *slot = Some(terminal);
    }
}

/// Build the success terminal, moving the shadow's replay into the envelope.
///
/// This is the only function that reads a shadow's replay for committing, and it is only called when
/// the model answered.
fn finish_success(
    tools: figcraft_agent::tools::ShadowTools,
    turn: u64,
    snapshot: &Snapshot,
    request: &str,
    answer: String,
) -> figcraft_agent::TurnTerminal {
    let replay = tools.into_shadow().map(|sh| sh.replay().to_vec()).unwrap_or_default();
    let commit = if replay.is_empty() {
        None
    } else {
        Some(figcraft_agent::CommitEnvelope::new(turn, snapshot.uid, snapshot.revision, replay, figcraft_agent::shadow::undo_label(request)))
    };
    figcraft_agent::TurnTerminal::Success { turn, answer, commit }
}

/// The credential a turn may use, resolved from the environment by the name in the config.
fn credential_for(config: &AgentConfig) -> Option<String> {
    config.credential().and_then(|name| std::env::var(name).ok()).filter(|v| !v.is_empty())
}

/// Build a shadow `Session` from a snapshot's document JSON.
fn shadow_from_snapshot(snapshot: &Snapshot) -> Result<figcraft_engine::Session, String> {
    // The snapshot is what `inspect_document` reported; a document round-trips through the same JSON
    // the format writes.
    let doc: figcraft_doc::Document =
        serde_json::from_value(snapshot.document.clone()).map_err(|e| format!("the document snapshot could not be read: {e}"))?;
    let session = figcraft_engine::Session::with_document(doc);
    let _ = snapshot.uid;
    Ok(session)
}

fn push_event(events: &Arc<Mutex<Vec<AgentEvent>>>, event: AgentEvent) {
    if let Ok(mut e) = events.lock() {
        e.push(event);
    }
}
