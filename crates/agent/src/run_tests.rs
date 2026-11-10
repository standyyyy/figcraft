//! The turn lifecycle: events, limits, cancellation, retry and reopening a saved run.

use crate::AgentErrorKind;
use crate::run::{AgentEvent, AgentRun, CancelToken, Limits, Outcome, RetryPolicy, ToolCall, failure_terminal};

/// A success that committed one replay step.
fn committed_terminal(turn: u64) -> crate::TurnTerminal {
    let envelope = crate::CommitEnvelope::new(
        turn,
        1,
        1,
        vec![crate::shadow::ReplayStep { command: "node.create".into(), params: serde_json::json!({"type": "rectangle"}) }],
        "AI: x",
    );
    crate::TurnTerminal::Success { turn, answer: String::new(), commit: Some(envelope), continuation: Vec::new() }
}

#[test]
fn progress_events_carry_no_ending_of_their_own() {
    // The defect this pins: a `Committed`/`Failed`/`Cancelled` event beside a separate terminal slot
    // let a turn look ended in one place and unfinished in another. Progress now has no opinion about
    // the outcome, so every event that exists is progress and the turn has exactly one ending.
    let turn = 7;
    let events = [
        AgentEvent::Started { turn },
        AgentEvent::TextDelta { turn, text: "Drawing ".into() },
        AgentEvent::ToolStarted { turn, name: "create_node".into() },
        AgentEvent::ToolFinished { turn, name: "create_node".into(), ok: true },
        AgentEvent::TextDelta { turn, text: "done.".into() },
        AgentEvent::Notice { turn, message: "history shortened".into() },
    ];
    assert!(events.first().is_some_and(|e| matches!(e, AgentEvent::Started { .. })), "a turn starts first");
    assert!(events.iter().all(|e| e.turn() == turn), "every event carries the turn");
    // `AgentEvent` has no `is_terminal`: the type cannot express an ending, which is the guarantee.
}

#[test]
fn every_kind_of_ending_is_a_terminal_and_only_one_can_carry_an_envelope() {
    use crate::TurnTerminal;
    let endings = [
        TurnTerminal::Success { turn: 1, answer: "x".into(), commit: None, continuation: Vec::new() },
        TurnTerminal::Failed { turn: 1, kind: AgentErrorKind::Timeout, message: "slow".into() },
        TurnTerminal::Cancelled { turn: 1 },
        TurnTerminal::LimitReached { turn: 1, kind: AgentErrorKind::Model, message: "rounds".into() },
    ];
    assert_eq!(endings.len(), 4, "every ending has a variant");
    assert_eq!(endings.iter().filter(|t| t.commit().is_some()).count(), 0, "none of these carries an envelope");
    for terminal in &endings {
        assert_eq!(terminal.turn(), 1, "each names its turn");
    }
    // The transcript outcome comes from the terminal, so the two cannot disagree.
    assert_eq!(endings[0].outcome(), Outcome::Completed, "a read-only success completed");
    assert_eq!(endings[1].outcome(), Outcome::Failed);
    assert_eq!(endings[2].outcome(), Outcome::Cancelled);
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
fn a_cancelled_turn_records_cancelled_and_a_late_success_cannot_change_it() {
    let mut run = AgentRun::new(3, "draw a card");
    let limits = Limits::default();
    run.apply(&AgentEvent::Started { turn: 3 }, &limits);
    run.apply(&AgentEvent::TextDelta { turn: 3, text: "working".into() }, &limits);
    // The user stops it before any commit.
    run.settle(&crate::TurnTerminal::Cancelled { turn: 3 });
    assert_eq!(run.outcome, Some(Outcome::Cancelled));
    assert_eq!(run.answer, "working", "the text so far is kept for the transcript");
    // A late response after cancellation is ignored: the outcome does not flip.
    run.settle(&crate::TurnTerminal::Success { turn: 3, answer: "late".into(), commit: None, continuation: Vec::new() });
    assert_eq!(run.outcome, Some(Outcome::Cancelled), "a late commit cannot un-cancel a turn");
    assert!(matches!(run.outcome, Some(Outcome::Cancelled)));
}

#[test]
fn a_turn_that_was_cancelled_after_shadow_edits_still_commits_nothing() {
    // The runner drops the shadow on cancellation, so no commands reach `commit`. Stated here as the
    // contract the caller relies on: a cancelled run has no Committed outcome ever.
    let mut run = AgentRun::new(1, "x");
    run.apply(&AgentEvent::ToolFinished { turn: 1, name: "create_node".into(), ok: true }, &Limits::default());
    run.settle(&crate::TurnTerminal::Cancelled { turn: 1 });
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
    run.settle(&committed_terminal(1));
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
    run.settle(&committed_terminal(4));

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
fn a_failure_terminal_carries_the_classified_kind_and_message() {
    let e = crate::AgentError::new(AgentErrorKind::Timeout, "the provider did not answer in 120 s");
    let terminal = failure_terminal(9, &e);
    match &terminal {
        crate::TurnTerminal::Failed { turn, kind, message, .. } => {
            assert_eq!(*turn, 9);
            assert_eq!(*kind, AgentErrorKind::Timeout);
            assert!(message.contains("120 s"));
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(terminal.commit().is_none(), "a failure carries no envelope");
    assert_eq!(terminal.outcome(), Outcome::Failed, "and the transcript agrees");
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

#[test]
fn a_turn_stopped_by_a_limit_keeps_its_record_but_applies_nothing() {
    // A limit is not a failure of the model's work so far, and it is not a finished turn either. The
    // transcript keeps what the turn did -- so a person can see the work and the reason -- while the
    // document is untouched, because a turn cut short is not one the user agreed to.
    use crate::run::{AgentEvent, AgentRun, Limits, Outcome};
    let limits = Limits::default();
    let mut run = AgentRun::new(1, "build the sidebar");
    run.apply(&AgentEvent::ToolFinished { turn: 1, name: "batch".into(), ok: true }, &limits);
    let terminal = crate::TurnTerminal::LimitReached {
        turn: 1,
        kind: crate::AgentErrorKind::Model,
        message: "the turn used its rounds".into(),
    };
    run.settle(&terminal);
    assert_eq!(run.outcome, Some(Outcome::Failed), "a cut-short turn records that it did not finish");
    assert_eq!(run.tools.len(), 1, "what it built is on the record");
    assert!(terminal.commit().is_none(), "and nothing may be applied");
}

#[test]
fn the_default_turn_budget_fits_a_real_screen() {
    // 12 rounds cut a prototype off halfway: a model spends rounds creating and a few inspecting.
    let l = Limits::default();
    let (rounds, calls) = (l.max_rounds, l.max_tool_calls);
    assert!(rounds >= 40, "rounds: {rounds}");
    assert!(calls >= 100, "tool calls: {calls}");
}
