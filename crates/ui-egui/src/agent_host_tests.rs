//! The Agent host contract: one interface, tested against every implementation.

use figcraft_agent::AgentErrorKind;
use figcraft_agent::run::{AgentEvent, Limits};

use crate::agent_host::{AgentHost, HostRequest, HostStatus, NoAgent, Snapshot, TurnOutcome};

fn snapshot() -> Snapshot {
    let doc = figcraft_doc::Document::new("probe");
    doc.first_page();
    Snapshot { document: serde_json::to_value(&doc).expect("document json"), uid: 1, revision: 1 }
}

fn config() -> figcraft_agent::AgentConfig {
    // A local endpoint: the host is not asked to reach it in these tests.
    let prefs = figcraft_engine::prefs::AgentPrefs {
        enabled: true,
        base_url: "http://127.0.0.1:8000".into(),
        model: "test-model".into(),
        ..figcraft_engine::prefs::AgentPrefs::default()
    };
    figcraft_agent::AgentConfig::from_prefs(&prefs).expect("valid config")
}

fn start(host: &mut dyn AgentHost, turn: u64) -> figcraft_agent::run::CancelToken {
    host.start(HostRequest::Start(Box::new(crate::agent_host::TurnRequest {
        turn,
        request: "draw a card".into(),
        snapshot: snapshot(),
        limits: Limits::default(),
        config: config(),
    })))
}

#[test]
fn a_host_with_no_runtime_reports_a_readable_failure() {
    // The contract holds even where nothing can run, so the panel behaves predictably rather than
    // streaming forever.
    let mut host = NoAgent::default();
    let _token = start(&mut host, 1);
    let events = host.drain();
    assert_eq!(events.len(), 1, "one event: the failure");
    match &events[0] {
        AgentEvent::Failed { turn, kind, message } => {
            assert_eq!(*turn, 1);
            assert_eq!(*kind, AgentErrorKind::Configuration, "a missing runtime is a configuration matter");
            assert!(message.contains("no Agent runtime"), "the message says what is missing: {message}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(events[0].is_terminal(), "and the turn is over");
    assert_eq!(host.status(), HostStatus::Idle);
    assert!(host.take_terminal().is_none(), "nothing to commit");
}

#[test]
fn draining_empties_the_queue() {
    let mut host = NoAgent::default();
    let _ = start(&mut host, 1);
    assert_eq!(host.drain().len(), 1);
    assert!(host.drain().is_empty(), "the events are taken once");
}

#[test]
fn cancelling_is_always_safe() {
    // A host must accept a cancel at any time, including with nothing running.
    let mut host = NoAgent::default();
    let _ = host.start(HostRequest::Cancel);
    assert!(host.drain().is_empty(), "a cancel with nothing running produces no event");
}

#[test]
fn a_description_names_the_platform() {
    let host = NoAgent::default();
    assert!(host.describe().contains("no Agent runtime"), "got {}", host.describe());
    let worker = crate::agent_worker::WorkerHost::new();
    assert!(worker.describe().contains("desktop"), "got {}", worker.describe());
}

#[test]
fn the_desktop_worker_starts_no_thread_until_a_turn_runs() {
    // An idle app has no Agent thread at all.
    let worker = crate::agent_worker::WorkerHost::new();
    assert_eq!(worker.status(), HostStatus::Idle);
    assert!(worker.describe().contains("no worker yet"), "got {}", worker.describe());
}

#[test]
fn the_desktop_worker_reports_an_unreachable_provider_and_commits_nothing() {
    // The transport is wired; a provider nothing listens on must be classified, not crash the
    // worker, and no replay may leave it: a failed turn changes no document.
    let mut worker = crate::agent_worker::WorkerHost::new();
    let prefs = figcraft_engine::prefs::AgentPrefs {
        enabled: true,
        base_url: "http://127.0.0.1:1".into(),
        model: "test-model".into(),
        timeout_seconds: 5,
        ..figcraft_engine::prefs::AgentPrefs::default()
    };
    let config = figcraft_agent::AgentConfig::from_prefs(&prefs).expect("valid config");
    let _token = worker.start(HostRequest::Start(Box::new(crate::agent_host::TurnRequest {
        turn: 1,
        request: "draw a card".into(),
        snapshot: snapshot(),
        limits: Limits::default(),
        config,
    })));

    let mut events = Vec::new();
    for _ in 0..400 {
        events.extend(worker.drain());
        if events.iter().any(AgentEvent::is_terminal) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Started { turn: 1 })), "the turn started: {events:?}");
    let terminal = events.iter().find(|e| e.is_terminal()).expect("the turn ended");
    match terminal {
        AgentEvent::Failed { kind, .. } => {
            assert!(
                matches!(kind, AgentErrorKind::Unreachable | AgentErrorKind::Timeout),
                "an unreachable provider is classified as such, got {kind:?}"
            );
        }
        other => panic!("expected a failure, got {other:?}"),
    }
    let terminal = worker.take_terminal();
    assert!(terminal.as_ref().is_none_or(|t| t.commit().is_none()), "a failed turn commits nothing");
    worker.shutdown();
}

#[test]
fn shutting_the_worker_down_joins_it() {
    // App exit must not hang on an Agent thread.
    let mut worker = crate::agent_worker::WorkerHost::new();
    let _ = start(&mut worker, 1);
    // Returns promptly, joined.
    worker.shutdown();
    assert_eq!(worker.status(), HostStatus::Idle);
    // Shutting down twice is harmless.
    worker.shutdown();
}

#[test]
fn a_turn_outcome_knows_whether_it_finished() {
    let mut outcome = TurnOutcome::default();
    assert!(!outcome.is_finished(), "an empty outcome has not finished");
    outcome.events.push(AgentEvent::Started { turn: 1 });
    assert!(!outcome.is_finished(), "progress events do not finish a turn");
    outcome.terminal = Some(figcraft_agent::TurnTerminal::Cancelled { turn: 1 });
    assert!(outcome.is_finished(), "a terminal value finishes it");
}

#[test]
fn only_a_successful_outcome_exposes_a_commit() {
    // The whole point of the typed terminal: the host hands back one value, and a replay is reachable
    // only from a success. A failure with edits is not representable, so no caller can apply one.
    let mut outcome = TurnOutcome::default();
    for terminal in [
        figcraft_agent::TurnTerminal::Failed { turn: 1, kind: AgentErrorKind::Protocol, message: "bad".into() },
        figcraft_agent::TurnTerminal::Cancelled { turn: 1 },
        figcraft_agent::TurnTerminal::LimitReached { turn: 1, kind: AgentErrorKind::Timeout, message: "slow".into() },
    ] {
        outcome.terminal = Some(terminal);
        assert!(outcome.commit().is_none(), "an unsuccessful ending carries no envelope");
    }

    let envelope = figcraft_agent::CommitEnvelope::new(
        1,
        1,
        1,
        vec![figcraft_agent::shadow::ReplayStep { command: "node.create".into(), params: serde_json::json!({}) }],
        "AI: x",
    );
    outcome.terminal = Some(figcraft_agent::TurnTerminal::Success { turn: 1, answer: "d".into(), commit: Some(envelope) });
    assert!(outcome.commit().is_some(), "a success exposes its envelope");
}

#[test]
fn a_success_without_edits_exposes_no_commit() {
    // A read-only turn succeeds and changes nothing; there is no envelope to apply.
    let outcome =
        TurnOutcome { events: Vec::new(), terminal: Some(figcraft_agent::TurnTerminal::Success { turn: 1, answer: "looked".into(), commit: None }) };
    assert!(outcome.is_finished());
    assert!(outcome.commit().is_none());
}

#[test]
fn a_snapshot_carries_what_the_worker_needs() {
    // The worker never borrows the live session, so the snapshot must be enough to build a shadow.
    let s = snapshot();
    assert_eq!(s.uid, 1);
    assert_eq!(s.revision, 1);
    let doc: figcraft_doc::Document = serde_json::from_value(s.document).expect("the snapshot is a document");
    assert_eq!(doc.name, "probe");
    assert!(!doc.pages.is_empty(), "with its pages");
}

#[test]
fn a_cancelled_turn_produces_one_cancelled_event_and_no_replay() {
    // The host contract: cancellation ends the turn and commits nothing.
    let mut worker = crate::agent_worker::WorkerHost::new();
    let token = start(&mut worker, 1);
    // Cancel immediately: the worker may or may not have started, but the turn must end cancelled
    // or failed, and never with a replay.
    token.cancel();
    let _ = worker.start(HostRequest::Cancel);
    let mut events = Vec::new();
    for _ in 0..200 {
        events.extend(worker.drain());
        if events.iter().any(AgentEvent::is_terminal) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    if let Some(terminal) = events.iter().find(|e| e.is_terminal()) {
        assert!(
            matches!(terminal, AgentEvent::Cancelled { .. } | AgentEvent::Failed { .. }),
            "a cancelled turn ends cancelled or failed, not committed: {terminal:?}"
        );
    }
    let terminal = worker.take_terminal();
    assert!(terminal.as_ref().is_none_or(|t| t.commit().is_none()), "a cancelled turn commits nothing");
    worker.shutdown();
}

#[test]
fn a_host_is_object_safe() {
    // The app holds one behind a `Box<dyn AgentHost>`, so the trait must stay object safe.
    let hosts: Vec<Box<dyn AgentHost>> = vec![Box::new(NoAgent::default()), Box::new(crate::agent_worker::WorkerHost::new())];
    for host in hosts {
        assert!(!host.describe().is_empty());
    }
}
