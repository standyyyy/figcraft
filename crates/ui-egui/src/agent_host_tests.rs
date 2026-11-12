//! The Agent host contract: one interface, tested against every implementation.

use figcraft_agent::AgentErrorKind;
use figcraft_agent::run::{AgentEvent, Limits};

use crate::agent_host::{AgentHost, HostMessage, HostRequest, HostStatus, NoAgent, Snapshot};

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
        history: Vec::new(),
        cancel: figcraft_agent::run::CancelToken::new(),
    })))
}

#[test]
fn a_host_with_no_runtime_reports_a_readable_failure() {
    // The contract holds even where nothing can run, so the panel behaves predictably rather than
    // streaming forever.
    let mut host = NoAgent::default();
    let _token = start(&mut host, 1);
    let messages = host.drain();
    assert_eq!(messages.len(), 1, "one message: the failure");
    match &messages[0] {
        HostMessage::Terminal(figcraft_agent::TurnTerminal::Failed { turn, kind, message, .. }) => {
            assert_eq!(*turn, 1);
            assert_eq!(*kind, AgentErrorKind::Configuration, "a missing runtime is a configuration matter");
            assert!(message.contains("no Agent runtime"), "the message says what is missing: {message}");
        }
        other => panic!("expected a failed terminal, got {other:?}"),
    }
    assert!(messages[0].is_terminal(), "and the turn is over");
    assert!(messages[0].turn() == 1);
    assert_eq!(host.status(), HostStatus::Idle);
}

#[test]
fn draining_empties_the_queue() {
    let mut host = NoAgent::default();
    let _ = start(&mut host, 1);
    assert_eq!(host.drain().len(), 1);
    assert!(host.drain().is_empty(), "the messages are taken once");
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
        history: Vec::new(),
        cancel: figcraft_agent::run::CancelToken::new(),
    })));

    let mut messages = Vec::new();
    for _ in 0..400 {
        let batch = worker.drain();
        let ended = batch.iter().any(HostMessage::is_terminal);
        messages.extend(batch);
        if ended {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(messages.iter().any(|m| matches!(m, HostMessage::Progress(AgentEvent::Started { turn: 1 }))), "the turn started: {messages:?}");
    let terminal = messages.iter().find_map(|m| match m {
        HostMessage::Terminal(t) => Some(t),
        _ => None,
    });
    match terminal.expect("the turn ended") {
        figcraft_agent::TurnTerminal::Failed { kind, .. } => {
            assert!(
                matches!(kind, AgentErrorKind::Unreachable | AgentErrorKind::Timeout),
                "an unreachable provider is classified as such, got {kind:?}"
            );
        }
        other => panic!("expected a failure, got {other:?}"),
    }
    // The ending came through the same queue as the progress, and carries no envelope.
    if let Some(HostMessage::Terminal(t)) = messages.last() {
        assert!(t.commit().is_none(), "a failed turn commits nothing");
    } else {
        panic!("the turn must end with a terminal: {messages:?}");
    }
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
fn one_queue_carries_progress_and_its_terminal_in_order() {
    // The contract this pins: progress and the ending share one FIFO, so the ending is the last
    // message of its turn by construction. Two deliveries -- a list plus a terminal slot -- allowed
    // the UI to see an ending while progress was still arriving.
    let mut host = NoAgent::default();
    let _ = start(&mut host, 1);
    let messages = host.drain();
    let last = messages.last().expect("the turn produced something");
    assert!(last.is_terminal(), "the ending is last: {messages:?}");
    assert!(messages[..messages.len() - 1].iter().all(|m| !m.is_terminal()), "and it is the only one");
    assert!(messages.iter().all(|m| m.turn() == 1), "every message names its turn");
}

#[test]
fn only_a_successful_terminal_exposes_a_commit() {
    // A replay is reachable only from a success, so no caller can apply one after a failure.
    for terminal in [
        figcraft_agent::TurnTerminal::Failed { turn: 1, kind: AgentErrorKind::Protocol, message: "bad".into() },
        figcraft_agent::TurnTerminal::Cancelled { turn: 1 },
        figcraft_agent::TurnTerminal::LimitReached { turn: 1, kind: AgentErrorKind::Timeout, message: "slow".into() },
    ] {
        let message = HostMessage::Terminal(terminal);
        if let HostMessage::Terminal(t) = &message {
            assert!(t.commit().is_none(), "an unsuccessful ending carries no envelope");
        }
    }

    let envelope = figcraft_agent::CommitEnvelope::new(
        1,
        1,
        1,
        vec![figcraft_agent::shadow::ReplayStep { command: "node.create".into(), params: serde_json::json!({}) }],
        "AI: x",
    );
    let success = HostMessage::Terminal(figcraft_agent::TurnTerminal::Success {
        turn: 1,
        answer: "d".into(),
        commit: Some(envelope),
        continuation: Vec::new(),
    });
    if let HostMessage::Terminal(t) = &success {
        assert!(t.commit().is_some(), "a success exposes its envelope");
    }
}

#[test]
fn a_read_only_success_exposes_no_commit() {
    // A turn that only looked at the document succeeds and changes nothing; there is nothing to apply.
    let terminal = figcraft_agent::TurnTerminal::Success { turn: 1, answer: "looked".into(), commit: None, continuation: Vec::new() };
    assert!(terminal.commit().is_none());
    assert_eq!(terminal.outcome(), figcraft_agent::run::Outcome::Completed, "and it is not 'committed'");
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
    let mut messages = Vec::new();
    for _ in 0..200 {
        let batch = worker.drain();
        let ended = batch.iter().any(HostMessage::is_terminal);
        messages.extend(batch);
        if ended {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    match messages.iter().find_map(|m| match m {
        HostMessage::Terminal(t) => Some(t),
        _ => None,
    }) {
        Some(t) => {
            assert!(
                matches!(t, figcraft_agent::TurnTerminal::Cancelled { .. } | figcraft_agent::TurnTerminal::Failed { .. }),
                "a cancelled turn ends cancelled or failed, not committed: {t:?}"
            );
            assert!(t.commit().is_none(), "a cancelled turn commits nothing");
        }
        None => panic!("the turn must end: {messages:?}"),
    }
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
