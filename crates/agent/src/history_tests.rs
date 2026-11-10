//! Conversations, transcript persistence and context compaction.

use crate::history::{Conversation, TRANSCRIPT_VERSION, Transcript, compact, context_for_model, estimated_chars};
use crate::run::{AgentEvent, AgentRun, Limits, Outcome};
use serde_json::json;

fn limits() -> Limits {
    Limits::default()
}

/// A success envelope with one replay step, for tests that need a committed turn.
fn committed_envelope(turn: u64) -> crate::CommitEnvelope {
    crate::CommitEnvelope::new(
        turn,
        1,
        1,
        vec![crate::shadow::ReplayStep { command: "node.create".into(), params: serde_json::json!({"type": "rectangle"}) }],
        "AI: q",
    )
}

fn run_with(turn: u64, request: &str, answer: &str) -> AgentRun {
    let limits = limits();
    let mut run = AgentRun::new(turn, request);
    run.apply(&AgentEvent::TextDelta { turn, text: answer.into() }, &limits);
    run.settle(&crate::TurnTerminal::Success { turn, answer: answer.into(), commit: Some(committed_envelope(turn)), continuation: Vec::new() });
    run
}

#[test]
fn a_conversation_records_turns_in_order() {
    let mut c = Conversation::new("c1");
    let limits = limits();
    for turn in 1..=3 {
        c.apply(&AgentEvent::Started { turn }, "q", &limits);
        c.apply(&AgentEvent::TextDelta { turn, text: format!("a{turn}") }, "q", &limits);
        c.settle(
            &crate::TurnTerminal::Success { turn, answer: format!("a{turn}"), commit: Some(committed_envelope(turn)), continuation: Vec::new() },
            "q",
        );
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
    let mut failed = AgentRun::new(1, "broke");
    failed.settle(&crate::TurnTerminal::Failed { turn: 1, kind: crate::AgentErrorKind::Timeout, message: "slow".into() });
    c.push(failed);
    let mut cancelled = AgentRun::new(2, "stopped");
    cancelled.settle(&crate::TurnTerminal::Cancelled { turn: 2 });
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
        .filter(|m| m["content"].as_str().is_some_and(|c| !c.starts_with("Earlier in this conversation") && c != "Understood."))
        .filter_map(|m| m["content"].as_str())
        .collect();
    assert!(!turn_contents.iter().any(|t| t.trim_end() == "request 0"), "a removed request is only in the summary, not a turn: {turn_contents:?}");
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

// ---------- Rig continuation across turns ----------

#[test]
fn a_conversation_carries_the_continuation_it_was_stored_with() {
    // The messages Rig returns from a run are what lets a follow-up see the turn before it. They are
    // stored as plain JSON so the schema stays ours and `figcraft-format` never learns about Rig.
    let mut convo = Conversation::new("doc-1");
    convo.continuation = vec![
        json!({"role": "user", "content": [{"type": "text", "text": "draw a card"}]}),
        json!({"role": "assistant", "content": [{"type": "text", "text": "done"}]}),
    ];
    let mut transcript = Transcript::default();
    transcript.conversations.push(convo);

    // Through the stored form and back.
    let stored = transcript.to_agent_data();
    let back = Transcript::from_agent_data(&stored);
    assert_eq!(back.conversations[0].continuation.len(), 2, "the continuation round-tripped");
}

#[test]
fn an_unreadable_continuation_message_is_dropped_not_fatal() {
    // A stored value an older or future schema wrote must not refuse the turn: the conversation
    // continues from the visible transcript with whatever is still readable.
    let stored =
        vec![json!({"role": "user", "content": [{"type": "text", "text": "keep me"}]}), json!({"role": "not-a-real-role", "content": "drop me"})];
    let messages = crate::history::continuation_messages(&stored);
    assert_eq!(messages.len(), 1, "the good message survived, the bad one was dropped");
    assert!(!crate::history::continuation_is_intact(&stored), "and the reader can say it was not intact");
}

#[test]
fn a_wholly_unreadable_continuation_yields_an_empty_history() {
    // Nothing readable: an empty history is the safe answer, not an error.
    let stored = vec![json!({"nonsense": true})];
    assert!(crate::history::continuation_messages(&stored).is_empty());
    assert!(!crate::history::continuation_is_intact(&stored));
}

#[test]
fn an_empty_continuation_is_intact() {
    // A conversation that has not run anything yet has nothing to continue, and that is not damage.
    assert!(crate::history::continuation_messages(&[]).is_empty());
    assert!(crate::history::continuation_is_intact(&[]));
}

#[test]
fn a_turn_with_no_continuation_stores_none() {
    // The stored form omits the field rather than writing an empty array, so a transcript from a
    // build without continuation reads back with an empty one.
    let transcript = Transcript::default();
    let stored = transcript.to_agent_data();
    let text = serde_json::to_string(&stored.json).expect("serialize");
    assert!(!text.contains("continuation"), "an empty continuation is not written: {text}");
}

// ---------- compaction of the messages actually sent ----------

/// A continuation history with `turns` complete turns, each a user ask, a tool call and result, and
/// an answer. Matches the shape Rig sends.
fn history_of(turns: usize, result_chars: usize) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for i in 0..turns {
        out.push(json!({"role": "user", "content": [{"type": "text", "text": format!("ask {i}")}]}));
        out.push(json!({"role": "assistant", "content": [
            {"type": "toolcall", "id": {"provider": format!("c{i}")},
             "function": {"name": "create_node", "arguments": {"type": "rectangle"}}}
        ]}));
        out.push(json!({"role": "user", "content": [
            {"type": "toolresult", "call": {"provider": format!("c{i}")}, "name": "create_node",
             "content": [{"type": "text", "text": "x".repeat(result_chars)}]}
        ]}));
        out.push(json!({"role": "assistant", "content": [{"type": "text", "text": format!("done {i}")}]}));
    }
    out
}

#[test]
fn a_history_just_below_the_budget_is_sent_unchanged() {
    let history = history_of(3, 100);
    let total: usize = crate::history::history_turns(&history).iter().map(|t| t.estimated_chars()).sum();
    let (out, report) = crate::history::compact_request(&history, total + 10, 2);
    assert_eq!(out, history, "nothing to do");
    assert!(report.is_empty(), "and nothing reported: {report:?}");
}

#[test]
fn a_history_just_above_the_budget_has_its_oldest_turns_summarized() {
    let history = history_of(6, 100);
    let total: usize = crate::history::history_turns(&history).iter().map(|t| t.estimated_chars()).sum();
    // A budget under the total but above what the recent turns need.
    let (out, report) = crate::history::compact_request(&history, total / 2, 2);
    assert!(report.removed_turns > 0, "old turns were folded: {report:?}");
    assert!(report.summarized, "a summary was prepended");
    assert!(out.len() < history.len(), "the request is smaller");
    // The summary is the first message and says it is a summary.
    assert_eq!(out[0]["role"], "user");
    assert!(text_of(&out[0]).contains("Earlier in this conversation"), "got {:?}", text_of(&out[0]));
}

#[test]
fn the_recent_complete_turns_always_survive() {
    // Keeping the last few whole turns is what lets a follow-up make sense.
    let history = history_of(8, 100);
    let total: usize = crate::history::history_turns(&history).iter().map(|t| t.estimated_chars()).sum();
    let out = crate::history::history_turns(&crate::history::compact_request(&history, total / 3, 3).0);
    // The kept turns plus the two messages of the summary: the last three asks are all still there.
    let asks: Vec<String> = out.iter().map(|t| text_of(&t.messages[0])).collect();
    for recent in (5..8).map(|i| format!("ask {i}")) {
        assert!(asks.iter().any(|a| a.contains(&recent)), "`{recent}` must survive: {asks:?}");
    }
}

#[test]
fn a_large_tool_result_is_elided_before_a_whole_turn_is_removed() {
    // The biggest payloads cost the most and mean the least once their turn is over, so they go
    // first: that keeps more turns' worth of meaning than dropping turns would.
    let history = history_of(2, 50_000);
    let total: usize = crate::history::history_turns(&history).iter().map(|t| t.estimated_chars()).sum();
    let (out, report) = crate::history::compact_request(&history, total / 2, 5);
    assert!(report.elided_results > 0, "tool results were elided: {report:?}");
    assert_eq!(report.removed_turns, 0, "no turn was removed while eliding sufficed: {report:?}");
    let text = serde_json::to_string(&out).expect("serialize");
    assert!(text.contains("omitted to fit the request"), "the elision says so");
}

#[test]
fn tool_call_and_result_pairs_stay_together() {
    // A provider rejects a tool result whose call is gone, so turns are removed whole.
    let history = history_of(10, 500);
    let total: usize = crate::history::history_turns(&history).iter().map(|t| t.estimated_chars()).sum();
    let (out, _report) = crate::history::compact_request(&history, total / 4, 2);
    // Every remaining toolcall has its result, and every result its call.
    let mut calls: Vec<String> = Vec::new();
    let mut results: Vec<String> = Vec::new();
    for m in &out {
        for part in m.get("content").and_then(serde_json::Value::as_array).into_iter().flatten() {
            match part.get("type").and_then(serde_json::Value::as_str) {
                Some("toolcall") => {
                    if let Some(id) = part.get("id").and_then(|i| i.get("provider")).and_then(serde_json::Value::as_str) {
                        calls.push(id.to_string());
                    }
                }
                Some("toolresult") => {
                    if let Some(id) = part.get("call").and_then(|i| i.get("provider")).and_then(serde_json::Value::as_str) {
                        results.push(id.to_string());
                    }
                }
                _ => {}
            }
        }
    }
    for r in &results {
        assert!(calls.contains(r), "a result without its call survived: {r}");
    }
    for c in &calls {
        assert!(results.contains(c), "a call without its result survived: {c}");
    }
}

#[test]
fn repeated_compaction_does_not_grow_the_summary() {
    // Compacting an already-compacted history must not append a second summary or grow without bound.
    let history = history_of(12, 200);
    let total: usize = crate::history::history_turns(&history).iter().map(|t| t.estimated_chars()).sum();
    let (once, _) = crate::history::compact_request(&history, total / 4, 2);
    let (twice, report) = crate::history::compact_request(&once, 100, 1);
    let summaries = twice.iter().filter(|m| text_of(m).contains("Earlier in this conversation")).count();
    assert!(summaries <= 2, "at most one summary per compaction: {summaries}");
    assert!(report.removed_turns > 0 || report.elided_results > 0, "the second pass did something");
    // The result is bounded, not growing.
    assert!(twice.len() <= once.len() + 2, "bounded: {} then {}", once.len(), twice.len());
}

#[test]
fn a_cjk_history_is_estimated_by_characters_not_bytes() {
    // CJK is three bytes per character in JSON: a byte estimate would compact far too eagerly.
    let mut history = Vec::new();
    for i in 0..4 {
        history.push(json!({"role": "user", "content": [{"type": "text", "text": format!("画一个卡片 {i}")}]}));
        history.push(json!({"role": "assistant", "content": [{"type": "text", "text": "好的"}]}));
    }
    let turns = crate::history::history_turns(&history);
    let estimated: usize = turns.iter().map(|t| t.estimated_chars()).sum();
    let bytes = serde_json::to_string(&history).expect("serialize").len();
    assert!(estimated < bytes, "the estimate counts characters ({estimated}), not bytes ({bytes})");
    // A generous budget keeps it whole.
    let (out, report) = crate::history::compact_request(&history, estimated, 2);
    assert_eq!(out, history);
    assert!(report.is_empty());
}

#[test]
fn the_system_contract_is_never_part_of_the_summarized_history() {
    // The history passed here is only the previous turns; the preamble is the caller's and is not in
    // it. This pins that compaction does not invent a system message of its own.
    let history = history_of(6, 300);
    let (out, _) = crate::history::compact_request(&history, 200, 1);
    assert!(!out.iter().any(|m| m.get("role").and_then(serde_json::Value::as_str) == Some("system")), "compaction adds no system message");
}

#[test]
fn an_empty_history_compacts_to_nothing() {
    let (out, report) = crate::history::compact_request(&[], 100, 3);
    assert!(out.is_empty());
    assert!(report.is_empty());
}

/// The text in a message, for assertions.
fn text_of(message: &serde_json::Value) -> String {
    message
        .get("content")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| p.get("text").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}
