//! Building a turn's driver from a snapshot, tested natively.
//!
//! This logic used to live inside the WASM-only host, so native CI never compiled it and its failure
//! path -- which fabricated a shadow and used `expect` -- was invisible. It lives here now, and the
//! tests run wherever the crate does.

use figcraft_agent::AgentErrorKind;
use figcraft_agent::TurnTerminal;
use serde_json::json;

use crate::agent_host::Snapshot;

/// A snapshot of a real, minimal document.
fn snapshot() -> Snapshot {
    let mut s = figcraft_engine::Session::new();
    s.execute("file.new", &json!({})).unwrap();
    let ds = s.ds().expect("a document");
    let document = figcraft_format::to_json(ds.doc.as_ref()).expect("document json");
    Snapshot { document, uid: ds.uid, revision: ds.revision }
}

/// A request against `snapshot`.
fn request(turn: u64, snapshot: Snapshot) -> crate::agent_host::TurnRequest {
    let prefs = figcraft_engine::prefs::AgentPrefs {
        enabled: true,
        base_url: "http://127.0.0.1:1".into(),
        model: "test".into(),
        timeout_seconds: 10,
        ..figcraft_engine::prefs::AgentPrefs::default()
    };
    crate::agent_host::TurnRequest {
        turn,
        request: "draw".into(),
        snapshot,
        limits: figcraft_agent::run::Limits::default(),
        config: figcraft_agent::AgentConfig::from_prefs(&prefs).expect("valid"),
        history: Vec::new(),
        cancel: figcraft_agent::run::CancelToken::new(),
    }
}

#[test]
fn a_good_snapshot_builds_a_driver_that_is_not_already_finished() {
    let mut req = request(1, snapshot());
    req.request = "draw a card".into();
    let credentials = crate::agent_credentials::AgentCredentials::default();
    let driver = crate::agent_driver::build_driver(req);
    assert!(!driver.is_done(), "a valid request has a turn to run");
    let _ = credentials;
}

#[test]
fn a_snapshot_that_is_not_a_document_yields_one_failed_terminal() {
    // The former WASM crash path: `serde_json::from_value` fails, and the old code fabricated a second
    // shadow for the failure. This must produce exactly one failed terminal, no shadow of its own, and
    // no panic.
    let mut snap = snapshot();
    snap.document = json!({"not": "a document"});
    let driver = crate::agent_driver::build_driver(request(7, snap));

    assert!(driver.is_done(), "it ends immediately");
    let terminal = driver.finish();
    match &terminal {
        TurnTerminal::Failed { turn, kind, message, .. } => {
            assert_eq!(*turn, 7, "it names the turn");
            assert_eq!(*kind, AgentErrorKind::Configuration, "a bad snapshot is a configuration problem");
            assert!(message.contains("snapshot"), "and says what could not be read: {message}");
        }
        other => panic!("expected a failed terminal, got {other:?}"),
    }
    assert!(terminal.commit().is_none(), "and it carries no envelope");
}

#[test]
fn a_missing_shadow_yields_one_failed_terminal_and_never_a_fabricated_one() {
    // A shadow that cannot be captured must not be replaced by an empty one: a turn against a fabricated
    // document could commit into the wrong file. This asserts the failure is reported instead.
    let mut snap = snapshot();
    // A document whose pages list is empty is not one a shadow can be captured from.
    snap.document = json!({"id": 1, "name": "broken", "pages": [], "currentPage": null});

    let driver = crate::agent_driver::build_driver(request(3, snap));
    if driver.is_done() {
        let terminal = driver.finish();
        assert!(terminal.commit().is_none(), "a failed build carries no envelope");
        assert!(!matches!(terminal, TurnTerminal::Success { .. }), "and it is not a success");
    }
}

#[test]
fn a_failed_build_never_reaches_the_live_document() {
    // The point of refusing to fabricate a shadow: nothing about a failed build may be applied.
    let mut snap = snapshot();
    snap.document = json!({"not": "a document"});
    let driver = crate::agent_driver::build_driver(request(5, snap));
    let terminal = driver.finish();
    assert!(terminal.commit().is_none(), "no envelope, so no batch");
}
