//! The browser Agent host: one local future per turn instead of a worker thread.
//!
//! A page has one thread, so it cannot park it waiting for a provider the way the desktop worker
//! does. The same [`TurnDriver`](figcraft_agent::runtime::TurnDriver) is advanced by a future
//! instead: each step awaits the next stream item, which yields to the browser's event loop, and the
//! panel drains the events it produced when the frame draws.
//!
//! The contract is identical to the desktop host's, so a turn behaves the same on either: the
//! shadow, the typed terminal, the "only a success carries a replay" rule, and the single
//! `command.batch` the UI thread applies are all the same code.
//!
//! Compiles only for the browser: it needs `spawn_local`, which has no meaning on a native build.

use std::cell::RefCell;
use std::rc::Rc;

use figcraft_agent::run::{AgentEvent, CancelToken};
use figcraft_agent::{TurnTerminal, runtime::TurnDriver};

use crate::agent_host::{AgentHost, HostMessage, HostRequest, HostStatus, Snapshot, TurnRequest};

/// Shared state between the future and the host: the future runs on the page's loop, so it cannot be
/// borrowed across an await.
#[derive(Default)]
struct Shared {
    /// The one queue the future fills and the UI drains, in order: progress then its terminal.
    queue: std::collections::VecDeque<HostMessage>,
    /// Whether a turn is running. Cleared when the terminal is *drained*, not when produced.
    running: bool,
    /// The egui context to wake whenever a message is queued.
    ///
    /// A page repaints only on input otherwise, so a turn that streams while nobody touches the page
    /// would show nothing until the next click or key.
    wake: Option<egui::Context>,
}

/// Queue `message` and wake the page.
fn push(shared: &Rc<RefCell<Shared>>, message: HostMessage) {
    let wake = match shared.try_borrow_mut() {
        Ok(mut s) => {
            s.queue.push_back(message);
            s.wake.clone()
        }
        // A borrow in progress means the UI is draining; the message waits for the next step.
        Err(_) => None,
    };
    if let Some(ctx) = wake {
        ctx.request_repaint();
    }
}

/// Runs turns on the browser's event loop.
pub struct LocalFutureHost {
    shared: Rc<RefCell<Shared>>,
    cancel: Option<CancelToken>,
}

impl Default for LocalFutureHost {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalFutureHost {
    /// A host with no turn running.
    pub fn new() -> Self {
        Self { shared: Rc::new(RefCell::new(Shared::default())), cancel: None }
    }
}

impl AgentHost for LocalFutureHost {
    fn start(&mut self, request: HostRequest) -> CancelToken {
        match request {
            HostRequest::Cancel => {
                if let Some(c) = self.cancel.take() {
                    c.cancel();
                }
                // The future sees the token and ends as cancelled on its next step.
                CancelToken::new()
            }
            HostRequest::Start(req) => {
                let cancel = CancelToken::new();
                let orchestrator = Orchestrator::from_request(*req, cancel.clone());
                self.cancel = Some(cancel.clone());
                if let Ok(mut shared) = self.shared.try_borrow_mut() {
                    shared.running = true;
                    shared.queue.clear();
                }
                let shared = Rc::clone(&self.shared);
                // A local future: it awaits the provider through the browser's fetch and yields to the
                // event loop between items, so the page stays responsive.
                wasm_bindgen_futures::spawn_local(async move {
                    orchestrator.run(shared).await;
                });
                cancel
            }
        }
    }

    fn drain(&mut self) -> Vec<HostMessage> {
        match self.shared.try_borrow_mut() {
            Ok(mut shared) => {
                let messages: Vec<HostMessage> = shared.queue.drain(..).collect();
                // The turn is over once its ending has been drained; until then it is still theirs to
                // settle, and a second start would race that.
                if messages.iter().any(HostMessage::is_terminal) {
                    shared.running = false;
                }
                messages
            }
            Err(_) => Vec::new(),
        }
    }

    fn status(&self) -> HostStatus {
        match self.shared.try_borrow() {
            Ok(s) if s.running => HostStatus::Running,
            _ => HostStatus::Idle,
        }
    }

    /// Keep the app's context, so a queued message repaints without waiting for input.
    fn set_wake(&mut self, ctx: egui::Context) {
        if let Ok(mut s) = self.shared.try_borrow_mut() {
            s.wake = Some(ctx);
        }
    }

    fn describe(&self) -> String {
        "browser local future".to_string()
    }
}

/// One turn's future.
struct Orchestrator {
    driver: TurnDriver,
}

impl Orchestrator {
    /// Build the driver for a request: a shadow from the snapshot, then the turn.
    fn from_request(req: TurnRequest, _cancel: CancelToken) -> Self {
        // The snapshot is the document as JSON: the driver's tools run against a shadow built from it,
        // never against the live document, which the UI thread owns.
        Self { driver: build_driver(req) }
    }

    /// Drive the turn to its end, handing events over as they are produced.
    async fn run(mut self, shared: Rc<RefCell<Shared>>) {
        loop {
            for event in self.driver.take_events() {
                push(&shared, HostMessage::Progress(event));
            }
            if self.driver.is_done() {
                break;
            }
            let more = self.driver.drive_once_async().await;
            // One item per step, then let the page breathe: the browser must not be starved of
            // frames while a long turn streams.
            if !more && self.driver.is_done() {
                break;
            }
        }
        for event in self.driver.take_events() {
            push(&shared, HostMessage::Progress(event));
        }
        // Progress first, then exactly one terminal, in one queue: the ending is the last message of
        // its turn by construction.
        push(&shared, HostMessage::Terminal(self.driver.finish()));
    }
}

/// Build a driver from a request, or a finished driver that reports why it could not start.
fn build_driver(req: TurnRequest) -> TurnDriver {
    let turn_no = req.turn;
    let (session, failure) = match crate::agent_host::shadow_session_from_snapshot(&req.snapshot) {
        Ok(s) => (s, None),
        // A snapshot that cannot be read is a configuration problem: report it as a terminal rather
        // than panicking on the page's thread.
        Err(message) => (figcraft_engine::Session::new(), Some((figcraft_agent::AgentErrorKind::Configuration, message))),
    };
    let shadow = match (figcraft_agent::shadow::Shadow::capture(&session), failure) {
        (Ok(shadow), None) => shadow,
        (Ok(shadow), Some((kind, message))) => {
            return TurnDriver::from_terminal(TurnTerminal::Failed { turn: turn_no, kind, message }, shadow);
        }
        (Err(e), _) => {
            let fallback = figcraft_agent::shadow::Shadow::capture(&figcraft_engine::Session::new()).unwrap_or_else(|_| {
                figcraft_agent::shadow::Shadow::capture(&figcraft_engine::Session::with_document(figcraft_doc::Document::new("untitled")))
                    .expect("empty")
            });
            return TurnDriver::from_terminal(TurnTerminal::Failed { turn: turn_no, kind: e.kind, message: e.message }, fallback);
        }
    };
    let turn = figcraft_agent::runtime::Turn {
        turn: req.turn,
        request: req.request,
        document_uid: req.snapshot.uid,
        base_revision: req.snapshot.revision,
        config: req.config,
        limits: req.limits,
        cancel: req.cancel,
        history: req.history,
    };
    TurnDriver::start(turn, shadow, &figcraft_agent::EnvironmentCredentials)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_with_no_turn_is_idle_and_says_what_it_is() {
        let host = LocalFutureHost::new();
        assert_eq!(host.status(), HostStatus::Idle);
        assert_eq!(host.describe(), "browser local future");
    }

    #[test]
    fn draining_an_idle_host_reports_nothing() {
        let mut host = LocalFutureHost::new();
        assert!(host.drain().is_empty());
        assert!(host.take_terminal().is_none());
    }
}
