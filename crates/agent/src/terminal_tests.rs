//! The terminal contract: only success can carry a replay, and only a matching document accepts it.

use serde_json::json;

use crate::AgentErrorKind;
use crate::shadow::ReplayStep;
use crate::terminal::{CommitEnvelope, TurnProgress, TurnTerminal};

fn step(command: &str) -> ReplayStep {
    ReplayStep { command: command.into(), params: json!({"type": "rectangle"}) }
}

fn envelope(turn: u64, uid: u64, revision: u64) -> CommitEnvelope {
    CommitEnvelope::new(turn, uid, revision, vec![step("node.create")], "AI: draw")
}

#[test]
fn only_success_exposes_a_commit() {
    let success = TurnTerminal::Success { turn: 1, answer: "done".into(), commit: Some(envelope(1, 7, 3)), continuation: Vec::new() };
    assert!(success.commit().is_some(), "a success carries its envelope");
    assert!(success.is_success());
    assert_eq!(success.message(), None, "a success has an answer, not a message");

    for terminal in [
        TurnTerminal::Failed { turn: 1, kind: AgentErrorKind::Protocol, message: "bad".into() },
        TurnTerminal::Cancelled { turn: 1 },
        TurnTerminal::LimitReached { turn: 1, kind: AgentErrorKind::Model, message: "rounds".into() },
    ] {
        assert!(terminal.commit().is_none(), "{terminal:?} must carry no replay");
        assert!(!terminal.is_success());
        assert!(terminal.message().is_some(), "{terminal:?} says what happened");
    }
}

#[test]
fn every_unsuccessful_end_is_a_different_variant() {
    // The type is the guarantee: there is no field on these variants a replay could sit in, so
    // "failed but with a replay" cannot be constructed, and no review comment is needed to prevent
    // it. This test documents the shape rather than a runtime check.
    let failed = TurnTerminal::Failed { turn: 2, kind: AgentErrorKind::Unreachable, message: "offline".into() };
    assert_eq!(failed.turn(), 2);
    assert!(failed.commit().is_none());

    let cancelled = TurnTerminal::Cancelled { turn: 3 };
    assert_eq!(cancelled.message(), Some("Stopped."));

    let limited = TurnTerminal::LimitReached { turn: 4, kind: AgentErrorKind::Timeout, message: "too slow".into() };
    assert_eq!(limited.turn(), 4);
    assert!(limited.commit().is_none());
}

#[test]
fn a_success_with_no_edits_has_no_envelope() {
    // A turn that only reads the document succeeds and changes nothing; there is nothing to commit.
    let progress = TurnProgress::new();
    let terminal = progress.finish(1, 7, 3, "AI: look");
    assert!(terminal.is_success());
    assert!(terminal.commit().is_none(), "no replay, no envelope");
}

#[test]
fn abandoning_a_turn_drops_its_work() {
    // The shadow is consumed by `abandon` and never read again: whatever the turn built stays in the
    // dropped shadow, which is what makes a failed turn change nothing.
    let mut progress = TurnProgress::new();
    progress.text("I was about to draw");
    let terminal = progress.abandon(1, AgentErrorKind::Tool, "a tool failed");
    assert!(terminal.commit().is_none());
    assert!(matches!(terminal, TurnTerminal::Failed { .. }));
}

#[test]
fn an_envelope_accepts_only_its_own_document_and_exact_revision() {
    let e = envelope(1, 7, 3);
    assert!(e.accepts(7, 3), "same document, same revision");
    assert!(!e.accepts(8, 3), "another document");
    assert!(!e.accepts(7, 4), "the document moved on");
    assert!(!e.accepts(7, 2), "even backwards: the turn was built against 3");
}

#[test]
fn a_refusal_says_which_check_failed() {
    let e = envelope(1, 7, 3);
    assert_eq!(e.refusal(7, 3), None);
    let wrong_file = e.refusal(8, 3).expect("a reason");
    assert!(wrong_file.contains("different file"), "got {wrong_file}");
    let stale = e.refusal(7, 4).expect("a reason");
    assert!(stale.contains("changed"), "got {stale}");
}

#[test]
fn the_replay_becomes_the_batch_commands() {
    let e = CommitEnvelope::new(1, 7, 3, vec![step("node.create"), step("node.set")], "AI: two");
    let batch = e.batch_commands();
    assert_eq!(batch.len(), 2);
    assert_eq!(batch[0]["command"], "node.create");
    assert_eq!(batch[1]["command"], "node.set");
    assert_eq!(batch[0]["params"]["type"], "rectangle");
}

#[test]
fn an_empty_envelope_reports_itself_empty() {
    let e = CommitEnvelope::new(1, 7, 3, Vec::new(), "AI: nothing");
    assert!(e.is_empty());
    assert!(e.batch_commands().is_empty());
}

#[test]
fn progress_records_what_the_turn_did() {
    let mut p = TurnProgress::new();
    p.text("Drawing ");
    p.text("a card");
    p.tool("create_node", true);
    p.tool("set_properties", false);
    assert_eq!(p.answer(), "Drawing a card");
    assert_eq!(p.tools(), [("create_node".to_string(), true), ("set_properties".to_string(), false)]);
}
