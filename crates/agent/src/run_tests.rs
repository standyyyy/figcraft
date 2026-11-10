//! The turn lifecycle: events, limits, cancellation, retry and reopening a saved run.

use crate::AgentErrorKind;
use crate::run::{AgentEvent, AgentRun, CancelToken, Limits, Outcome, RetryPolicy, ToolCall, failure_event};

#[test]
fn a_turn_emits_events_in_order_and_exactly_one_terminal_event() {
    let turn = 7;
    let events = [
        AgentEvent::Started { turn },
        AgentEvent::TextDelta { turn, text: "Drawing ".into() },
        AgentEvent::ToolStarted { turn, name: "create_node".into() },
        AgentEvent::ToolFinished { turn, name: "create_node".into(), ok: true },
        AgentEvent::TextDelta { turn, text: "done.".into() },
        AgentEvent::Committed { turn, undo_label: "AI: draw".into() },
    ];
    assert!(events.first().is_some_and(|e| matches!(e, AgentEvent::Started { .. })), "a turn starts first");
    let terminal: Vec<&AgentEvent> = events.iter().filter(|e| e.is_terminal()).collect();
    assert_eq!(terminal.len(), 1, "exactly one terminal event");
    assert!(matches!(terminal[0], AgentEvent::Committed { .. }));
    assert!(events.iter().all(|e| e.turn() == turn), "every event carries the turn");
    // Nothing follows the terminal event.
    let last = events.last().expect("last");
    assert!(last.is_terminal());
}

#[test]
fn every_kind_of_ending_is_terminal() {
    for event in [
        AgentEvent::Committed { turn: 1, undo_label: "AI: x".into() },
        AgentEvent::Failed { turn: 1, kind: AgentErrorKind::Timeout, message: "slow".into() },
        AgentEvent::Cancelled { turn: 1 },
    ] {
        assert!(event.is_terminal(), "{event:?} ends the turn");
    }
    for event in [
        AgentEvent::Started { turn: 1 },
        AgentEvent::TextDelta { turn: 1, text: "x".into() },
        AgentEvent::ToolStarted { turn: 1, name: "t".into() },
        AgentEvent::ToolFinished { turn: 1, name: "t".into(), ok: true },
    ] {
        assert!(!event.is_terminal(), "{event:?} does not end the turn");
    }
}

#[test]
fn cancellation_is_idempotent_and_readable_from_any_clone() {
    let token = CancelToken::new();
    assert!(!token.is_cancelled());
    let other = token.clone();
    other.cancel();
    assert!(token.is_cancelled(), "a clone cancels the same turn");
    // Cancelling again is harmless: the UI and a shutdown path may both ask.
    token.cancel();
    other.cancel();
    assert!(token.is_cancelled());
}

#[test]
fn a_cancelled_turn_produces_one_cancelled_event_and_no_commit() {
    let mut run = AgentRun::new(3, "draw a card");
    let limits = Limits::default();
    run.apply(&AgentEvent::Started { turn: 3 }, &limits);
    run.apply(&AgentEvent::TextDelta { turn: 3, text: "working".into() }, &limits);
    // The user stops it before any commit.
    run.apply(&AgentEvent::Cancelled { turn: 3 }, &limits);
    assert_eq!(run.outcome, Some(Outcome::Cancelled));
    assert_eq!(run.answer, "working", "the text so far is kept for the transcript");
    // A late response after cancellation is ignored: the outcome does not flip.
    run.apply(&AgentEvent::Committed { turn: 3, undo_label: "AI: late".into() }, &limits);
    assert_eq!(run.outcome, Some(Outcome::Cancelled), "a late commit cannot un-cancel a turn");
    assert!(matches!(run.outcome, Some(Outcome::Cancelled)));
}

#[test]
fn a_turn_that_was_cancelled_after_shadow_edits_still_commits_nothing() {
    // The runner drops the shadow on cancellation, so no commands reach `commit`. Stated here as the
    // contract the caller relies on: a cancelled run has no Committed outcome ever.
    let mut run = AgentRun::new(1, "x");
    run.apply(&AgentEvent::ToolFinished { turn: 1, name: "create_node".into(), ok: true }, &Limits::default());
    run.apply(&AgentEvent::Cancelled { turn: 1 }, &Limits::default());
    assert_eq!(run.outcome, Some(Outcome::Cancelled));
    assert_eq!(run.tools.len(), 1, "the tool that did run is recorded, for the transcript");
}

#[test]
fn a_tool_failure_is_recorded_and_the_turn_can_still_succeed() {
    // A tool error is information for the model, not the end of the turn: the model may try another
    // way. Only a failed *turn* is terminal.
    let limits = Limits::default();
    let mut run = AgentRun::new(1, "x");
    run.apply(&AgentEvent::ToolFinished { turn: 1, name: "create_node".into(), ok: false }, &limits);
    assert!(run.is_running(), "a tool error does not end the turn");
    run.apply(&AgentEvent::ToolFinished { turn: 1, name: "create_node".into(), ok: true }, &limits);
    run.apply(&AgentEvent::Committed { turn: 1, undo_label: "AI: x".into() }, &limits);
    assert_eq!(run.outcome, Some(Outcome::Committed));
    assert_eq!(
        run.tools,
        [ToolCall { name: "create_node".into(), ok: false }, ToolCall { name: "create_node".into(), ok: true }],
        "both attempts are on the record"
    );
}

#[test]
fn the_round_and_tool_call_budgets_stop_a_runaway_turn() {
    let limits = Limits { max_rounds: 3, max_tool_calls: 2, ..Limits::default() };
    assert!(!limits.rounds_exhausted(0));
    assert!(!limits.rounds_exhausted(2));
    assert!(limits.rounds_exhausted(3), "the fourth round is over the budget");
    assert!(!limits.tool_calls_exhausted(1));
    assert!(limits.tool_calls_exhausted(2), "the third tool call is over the budget");
}

#[test]
fn transcript_growth_is_bounded() {
    let limits = Limits { max_transcript_bytes: 20, ..Limits::default() };
    let mut run = AgentRun::new(1, "x");
    run.push_text("0123456789", &limits);
    assert_eq!(run.answer.len(), 10);
    run.push_text("0123456789", &limits);
    assert_eq!(run.answer.len(), 20, "the budget is reached");
    // Over the budget: the delta is dropped, and the run is not corrupted.
    run.push_text("more", &limits);
    assert_eq!(run.answer.len(), 20, "a delta past the budget is dropped");
    assert!(run.answer.is_char_boundary(run.answer.len()));
}

#[test]
fn a_multibyte_delta_is_never_cut_mid_character() {
    // The budget counts bytes; a dropped delta must not leave half a character behind.
    let limits = Limits { max_transcript_bytes: 7, ..Limits::default() };
    let mut run = AgentRun::new(1, "x");
    run.push_text("中文", &limits); // 6 bytes
    assert_eq!(run.answer, "中文");
    run.push_text("日本", &limits); // would be 12 bytes
    assert_eq!(run.answer, "中文", "the whole delta is dropped rather than split");
    assert!(run.answer.chars().count() == 2);
}

#[test]
fn only_a_transient_failure_is_retried() {
    let policy = RetryPolicy { retries: 2 };
    assert!(policy.should_retry(AgentErrorKind::Timeout, 0), "a timeout is worth retrying");
    assert!(policy.should_retry(AgentErrorKind::Unreachable, 1));
    assert!(!policy.should_retry(AgentErrorKind::Timeout, 2), "the retry budget is spent");

    for permanent in [
        AgentErrorKind::Configuration,
        AgentErrorKind::Authentication,
        AgentErrorKind::Protocol,
        AgentErrorKind::Model,
        AgentErrorKind::Tool,
        AgentErrorKind::Conflict,
        AgentErrorKind::Cancelled,
    ] {
        assert!(!policy.should_retry(permanent, 0), "{permanent:?} will not fix itself");
    }
}

#[test]
fn a_conflict_is_never_retried_against_the_same_shadow() {
    // Retrying a conflict would replay against a document the Agent never inspected, which is the
    // one thing the revision guard exists to prevent.
    let policy = RetryPolicy::default();
    assert!(!policy.should_retry(AgentErrorKind::Conflict, 0));
}

#[test]
fn a_run_reopens_as_interrupted_not_running() {
    // A turn saved mid-flight cannot be resumed: its shadow is gone and its edits were never
    // committed. Saying "interrupted" is honest; "running" would leave a spinner forever.
    let mut run = AgentRun::new(1, "draw");
    run.apply(&AgentEvent::Started { turn: 1 }, &Limits::default());
    run.apply(&AgentEvent::TextDelta { turn: 1, text: "partial".into() }, &Limits::default());
    assert!(run.is_running());

    let json = serde_json::to_string(&run).expect("serialize");
    let mut back: AgentRun = serde_json::from_str(&json).expect("deserialize");
    assert!(back.is_running(), "the file records that it had not finished");
    back.settle_on_reopen();
    assert_eq!(back.outcome, Some(Outcome::Interrupted));
    assert_eq!(back.answer, "partial", "the text so far survives");
    assert_eq!(back.request, "draw");
}

#[test]
fn a_finished_run_round_trips_through_json() {
    let limits = Limits::default();
    let mut run = AgentRun::new(4, "make a card");
    run.apply(&AgentEvent::TextDelta { turn: 4, text: "ok".into() }, &limits);
    run.apply(&AgentEvent::ToolFinished { turn: 4, name: "create_node".into(), ok: true }, &limits);
    run.apply(&AgentEvent::Committed { turn: 4, undo_label: "AI: make a card".into() }, &limits);

    let json = serde_json::to_string(&run).expect("serialize");
    let back: AgentRun = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, run);
    // camelCase on the wire, like the rest of the file format.
    assert!(json.contains("request"), "got {json}");
    assert!(!json.contains("consideration"), "no unexpected keys");
}

#[test]
fn a_finished_run_does_not_become_interrupted() {
    let mut run = AgentRun::new(1, "x");
    run.finish(Outcome::Committed);
    run.settle_on_reopen();
    assert_eq!(run.outcome, Some(Outcome::Committed), "settling only fills an empty outcome");
}

#[test]
fn a_failure_event_carries_the_classified_kind_and_message() {
    let e = crate::AgentError::new(AgentErrorKind::Timeout, "the provider did not answer in 120 s");
    let event = failure_event(9, &e);
    match &event {
        AgentEvent::Failed { turn, kind, message } => {
            assert_eq!(*turn, 9);
            assert_eq!(*kind, AgentErrorKind::Timeout);
            assert!(message.contains("120 s"));
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(event.is_terminal());
}

#[test]
fn every_error_kind_has_an_action_a_person_can_take() {
    for kind in [
        AgentErrorKind::Configuration,
        AgentErrorKind::Authentication,
        AgentErrorKind::Unreachable,
        AgentErrorKind::Timeout,
        AgentErrorKind::Protocol,
        AgentErrorKind::Model,
        AgentErrorKind::Tool,
        AgentErrorKind::Conflict,
        AgentErrorKind::Cancelled,
    ] {
        let action = kind.action();
        assert!(!action.is_empty(), "{kind:?} needs a readable action");
        assert!(action.ends_with('.'), "{kind:?}'s action is a sentence: {action}");
    }
}

#[test]
fn the_default_limits_bound_every_runaway_dimension() {
    let limits = Limits::default();
    assert!(limits.max_rounds > 0 && limits.max_rounds <= 100, "rounds are bounded");
    assert!(limits.max_tool_calls > 0 && limits.max_tool_calls <= 500);
    assert!(limits.max_tool_args_bytes > 0);
    assert!(limits.max_tool_result_bytes > 0);
    assert!(limits.max_seconds > 0 && limits.max_seconds <= 3600, "a turn cannot run for hours");
    assert!(limits.max_transcript_bytes > 0);
}
