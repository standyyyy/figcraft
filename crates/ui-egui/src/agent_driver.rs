//! Building a turn's driver from a snapshot. Compiled on every target.
//!
//! This lived inside the browser host, which meant native CI never compiled it and never saw its
//! failure path -- and that path fabricated an empty shadow and used `expect` to do it. A turn built
//! against a fabricated document could commit into the wrong file, so the code is target-independent
//! and tested natively; only the scheduling (a `spawn_local` future) stays in the browser module.

use figcraft_agent::TurnTerminal;
use figcraft_agent::runtime::TurnDriver;

use crate::agent_credentials::SessionResolver;
use crate::agent_host::TurnRequest;

/// Build the driver for `req`, or one that is already finished and reports why it could not start.
///
/// A snapshot that is not a document, and a shadow that cannot be captured from it, both end as a
/// single failed terminal. Neither fabricates a document: a turn against a made-up one could apply
/// commands to a file the Agent never inspected.
pub fn build_driver(req: TurnRequest) -> TurnDriver {
    build_driver_with(req, &crate::agent_host::shadow_session_from_snapshot)
}

/// [`build_driver`] with the shadow-construction step injectable, so a test can make it fail.
pub fn build_driver_with(
    req: TurnRequest,
    make_session: &dyn Fn(&crate::agent_host::Snapshot) -> Result<figcraft_engine::Session, String>,
) -> TurnDriver {
    let turn_no = req.turn;
    let session = match make_session(&req.snapshot) {
        Ok(s) => s,
        // A snapshot that cannot be read is a configuration problem: one terminal, no shadow at all.
        Err(message) => {
            return finished(turn_no, figcraft_agent::AgentErrorKind::Configuration, format!("the document snapshot could not be read: {message}"));
        }
    };
    let shadow = match figcraft_agent::shadow::Shadow::capture(&session) {
        Ok(s) => s,
        Err(e) => return finished(turn_no, e.kind, e.message),
    };
    TurnDriver::start(req.into_turn_with(figcraft_agent::run::CancelToken::new()), shadow, &crate::agent_host::host_credentials())
}

/// A driver that ends immediately with one failure, needing no shadow.
fn finished(turn: u64, kind: figcraft_agent::AgentErrorKind, message: String) -> TurnDriver {
    TurnDriver::failed_before_start(turn, kind, message)
}

/// Build the driver for `req` with a specific credential resolver, for the browser.
pub fn build_driver_with_credentials(req: TurnRequest, credentials: &SessionResolver) -> TurnDriver {
    let turn_no = req.turn;
    let session = match crate::agent_host::shadow_session_from_snapshot(&req.snapshot) {
        Ok(s) => s,
        Err(message) => {
            return finished(turn_no, figcraft_agent::AgentErrorKind::Configuration, format!("the document snapshot could not be read: {message}"));
        }
    };
    let shadow = match figcraft_agent::shadow::Shadow::capture(&session) {
        Ok(s) => s,
        Err(e) => return finished(turn_no, e.kind, e.message),
    };
    TurnDriver::start(req.into_turn_with(figcraft_agent::run::CancelToken::new()), shadow, credentials)
}

/// A terminal that is over before it began, for a request that cannot start.
pub fn immediate_failure(turn: u64, kind: figcraft_agent::AgentErrorKind, message: impl Into<String>) -> TurnTerminal {
    TurnTerminal::Failed { turn, kind, message: message.into() }
}
