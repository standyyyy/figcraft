//! Conversations, transcript persistence and context compaction.

use crate::history::{Conversation, TRANSCRIPT_VERSION, Transcript, compact, context_for_model, estimated_chars};
use crate::run::{AgentEvent, AgentRun, Limits, Outcome};

fn limits() -> Limits {
    Limits::default()
}

fn run_with(turn: u64, request: &str, answer: &str) -> AgentRun {
    let limits = limits();
    let mut run = AgentRun::new(turn, request);
    run.apply(&AgentEvent::TextDelta { turn, text: answer.into() }, &limits);
    run.apply(&AgentEvent::Committed { turn, undo_label: "AI: x".into() }, &limits);
    run
}

#[test]
fn a_conversation_records_turns_in_order() {
    let mut c = Conversation::new("c1");
    let limits = limits();
    for turn in 1..=3 {
        c.apply(&AgentEvent::Started { turn }, "q", &limits);
        c.apply(&AgentEvent::TextDelta { turn, text: format!("a{turn}") }, "q", &limits);
        c.apply(&AgentEvent::Committed { turn, undo_label: "AI: q".into() }, "q", &limits);
    }
    assert_eq!(c.turns.len(), 3);
    assert_eq!(c.turns.iter().map(|r| r.turn).collect::<Vec<_>>(), [1, 2, 3]);
    assert!(c.turns.iter().all(|r| r.outcome == Some(Outcome::Committed)));
}

#[test]
fn an_event_for_a_known_turn_appends_to_it_not_a_new_one() {
    let mut c = Conversation::new("c1");
    let limits = limits();
    c.apply(&AgentEvent::Started { turn: 5 }, "q", &limits);
    c.apply(&AgentEvent::TextDelta { turn: 5, text: "a".into() }, "q", &limits);
    c.apply(&AgentEvent::TextDelta { turn: 5, text: "b".into() }, "q", &limits);
    assert_eq!(c.turns.len(), 1, "one turn, not three");
    assert_eq!(c.turns[0].answer, "ab");
}

#[test]
fn reopening_settles_every_unfinished_turn() {
    let mut c = Conversation::new("c1");
    let limits = limits();
    c.apply(&AgentEvent::Started { turn: 1 }, "q", &limits);
    c.turns.push(run_with(2, "done", "ok"));
    c.settle_on_reopen();
    assert_eq!(c.turns[0].outcome, Some(Outcome::Interrupted), "a running turn becomes interrupted");
    assert_eq!(c.turns[1].outcome, Some(Outcome::Committed), "a finished one is left alone");
}

#[test]
fn a_transcript_round_trips_through_the_agent_data_payload() {
    let mut t = Transcript::default();
    let c = t.conversation_mut("c1");
    c.push(run_with(1, "make a card", "done"));
    let data = t.to_agent_data();
    let back = Transcript::from_agent_data(&data);
    assert_eq!(back, t);
    assert_eq!(back.version, TRANSCRIPT_VERSION);
}

#[test]
fn a_newer_version_reads_as_empty_rather_than_failing() {
    // A future build may change the shape. Losing the chat is preferable to refusing the file.
    let json = serde_json::json!({"version": TRANSCRIPT_VERSION + 1, "conversations": [{"id": "c", "turns": []}]});
    let data = figcraft_format::agent::AgentData::new(json);
    let t = Transcript::from_agent_data(&data);
    assert!(t.conversations.is_empty(), "an unknown version is not read");
    assert!(t.is_empty());
}

#[test]
fn a_payload_that_is_not_a_transcript_reads_as_empty() {
    let data = figcraft_format::agent::AgentData::new(serde_json::json!({"nonsense": true}));
    let t = Transcript::from_agent_data(&data);
    assert!(t.is_empty());
    // And a null payload (what a failed serialization writes) does too.
    let t = Transcript::from_agent_data(&figcraft_format::agent::AgentData::new(serde_json::Value::Null));
    assert!(t.is_empty());
}

#[test]
fn a_transcript_that_has_only_empty_conversations_is_empty() {
    let mut t = Transcript::default();
    t.conversation_mut("a");
    t.conversation_mut("b");
    assert!(t.is_empty(), "conversations with no turns are not worth writing");
}

#[test]
fn the_estimate_counts_the_answer_and_the_request() {
    let run = run_with(1, "make a card", "here it is");
    let estimate = estimated_chars(&run);
    assert!(estimate >= "make a card".len() + "here it is".len(), "the estimate covers both texts: {estimate}");
}

#[test]
fn a_conversation_within_budget_is_left_alone() {
    let mut c = Conversation::new("c1");
    for turn in 0..3 {
        c.push(run_with(turn, "q", "a"));
    }
    let before = c.clone();
    assert_eq!(compact(&mut c, 100_000, 2), None, "nothing to compact");
    assert_eq!(c, before, "the conversation is unchanged");
}

#[test]
fn compaction_drops_the_oldest_turns_and_keeps_the_recent_ones() {
    let mut c = Conversation::new("c1");
    for turn in 0..10 {
        c.push(run_with(turn, &format!("request {turn}"), &"x".repeat(100)));
    }
    let done = compact(&mut c, 500, 3).expect("compaction ran");
    assert_eq!(done.removed, 7, "the oldest seven turns were removed");
    assert_eq!(done.kept, 3, "the three most recent survive");
    assert_eq!(c.turns.len(), 3);
    let turns: Vec<u64> = c.turns.iter().map(|r| r.turn).collect();
    assert_eq!(turns, [7, 8, 9], "the most recent turns, in order");
}

#[test]
fn compaction_writes_an_explicit_summary_never_a_silent_discard() {
    // A person reading the transcript can see what was elided.
    let mut c = Conversation::new("c1");
    for turn in 0..6 {
        c.push(run_with(turn, &format!("request {turn}"), "a"));
    }
    compact(&mut c, 100, 2).expect("compaction ran");
    let summary = c.summary.expect("a summary was written");
    assert!(summary.contains("request 0"), "the removed turns are named: {summary}");
    assert!(summary.contains("request 3"));
    assert!(!summary.contains("request 4"), "a kept turn is not summarized: {summary}");
}

#[test]
fn the_summary_records_how_each_turn_ended() {
    let mut c = Conversation::new("c1");
    let limits = limits();
    let mut failed = AgentRun::new(1, "broke");
    failed.apply(&AgentEvent::Failed { turn: 1, kind: crate::AgentErrorKind::Timeout, message: "slow".into() }, &limits);
    c.push(failed);
    let mut cancelled = AgentRun::new(2, "stopped");
    cancelled.apply(&AgentEvent::Cancelled { turn: 2 }, &limits);
    c.push(cancelled);
    c.push(run_with(3, "kept", "a"));

    compact(&mut c, 10, 1).expect("compaction ran");
    let summary = c.summary.expect("summary");
    assert!(summary.contains("(failed)"), "a failed turn is marked: {summary}");
    assert!(summary.contains("(cancelled)"), "a cancelled turn is marked: {summary}");
}

#[test]
fn compacting_twice_does_not_grow_the_summary_without_bound() {
    let mut c = Conversation::new("c1");
    for turn in 0..30 {
        c.push(run_with(turn, &format!("request {turn} {}", "x".repeat(20)), "a"));
    }
    // Compact repeatedly with a small budget.
    for _ in 0..5 {
        for turn in 100..110 {
            c.push(run_with(turn, &format!("later {turn} {}", "y".repeat(20)), "a"));
        }
        compact(&mut c, 300, 2);
    }
    let summary = c.summary.expect("summary");
    assert!(summary.chars().count() <= 2000, "the summary stays bounded: {} chars", summary.chars().count());
}

#[test]
fn a_short_conversation_is_never_compacted_below_its_recent_turns() {
    let mut c = Conversation::new("c1");
    for turn in 0..2 {
        c.push(run_with(turn, &"x".repeat(500), "a"));
    }
    assert_eq!(compact(&mut c, 10, 2), None, "the recent turns are not removed");
    assert_eq!(c.turns.len(), 2);
}

#[test]
fn the_model_sees_the_summary_then_the_kept_turns() {
    let mut c = Conversation::new("c1");
    for turn in 0..6 {
        c.push(run_with(turn, &format!("request {turn}"), &format!("answer {turn}")));
    }
    compact(&mut c, 100, 2).expect("compaction ran");
    let context = context_for_model(&c);
    assert!(!context.is_empty());
    let first = context[0]["content"].as_str().expect("content");
    assert!(first.contains("Earlier in this conversation"), "the summary leads: {first}");
    // Then the kept turns, request and answer alternating, most recent last.
    let texts: Vec<&str> = context.iter().filter_map(|m| m["content"].as_str()).collect();
    assert!(texts.iter().any(|t| t.contains("request 4")), "a kept request is present: {texts:?}");
    assert!(texts.iter().any(|t| t.contains("answer 5")));
    // A removed turn's request appears only inside the summary, never as a turn of its own.
    let turn_contents: Vec<&str> = context
        .iter()
        .filter(|m| {
            m["content"]
                .as_str()
                .is_some_and(|c| !c.starts_with("Earlier in this conversation") && c != "Understood.")
        })
        .filter_map(|m| m["content"].as_str())
        .collect();
    assert!(
        !turn_contents.iter().any(|t| t.trim_end() == "request 0"),
        "a removed request is only in the summary, not a turn: {turn_contents:?}"
    );
}

#[test]
fn a_transcript_with_no_summary_has_no_preamble() {
    let mut c = Conversation::new("c1");
    c.push(run_with(1, "q", "a"));
    let context = context_for_model(&c);
    assert_eq!(context[0]["role"], "user");
    assert_eq!(context[0]["content"], "q", "no summary preamble when nothing was elided");
}

#[test]
fn a_conversation_lookup_creates_once() {
    let mut t = Transcript::default();
    t.conversation_mut("a").push(run_with(1, "q", "a"));
    t.conversation_mut("a").push(run_with(2, "q", "a"));
    t.conversation_mut("b");
    assert_eq!(t.conversations.len(), 2, "`a` was created once");
    assert_eq!(t.conversation("a").map(|c| c.turns.len()), Some(2));
    assert!(t.conversation("missing").is_none());
}

#[test]
fn the_transcript_estimate_adds_up_its_conversations() {
    let mut t = Transcript::default();
    t.conversation_mut("a").push(run_with(1, "q", "a"));
    t.conversation_mut("b").push(run_with(2, "q", "a"));
    let each: usize = t.conversations.iter().map(Conversation::estimated_chars).sum();
    assert_eq!(t.estimated_chars(), each);
    assert!(t.estimated_chars() > 0);
}
