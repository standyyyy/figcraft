//! Conversations and context compaction.
//!
//! A transcript travels with the file as `agent/conversations.json` (see
//! [`figcraft_format::agent`]), which caps and carries it but does not own its shape. This module
//! owns that shape: conversations of [`AgentRun`]s, and the compaction that keeps a long one inside
//! a model's context.
//!
//! Compaction is deliberately conservative. It uses a character estimate rather than a provider
//! tokenizer (there is no one tokenizer for an OpenAI-compatible endpoint), it keeps the system
//! contract and the most recent complete turns, and it drops the largest payloads first. A failed
//! summarization leaves the original transcript usable: losing a chat is worse than a long one.

use serde_json::{Value, json};

use crate::run::{AgentEvent, AgentRun, Limits, Outcome};

/// One conversation: a sequence of turns, oldest first.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    /// Stable id, allocated by the caller.
    pub id: String,
    /// The turns, oldest first.
    #[serde(default)]
    pub turns: Vec<AgentRun>,
    /// A summary of the turns compaction removed, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Rig's continuation messages from the last finished turn, as it returned them.
    ///
    /// Stored as plain JSON rather than a Rig type so the transcript schema stays this crate's and
    /// `figcraft-format` keeps treating the payload as opaque. A follow-up turn hands these back to
    /// the agent with `.history(...)`, which is what lets the model see the tool calls and results
    /// of the turn before it instead of only the text. An unreadable value is dropped, not fatal:
    /// the conversation continues from the visible transcript.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub continuation: Vec<Value>,
}

impl Conversation {
    /// A new, empty conversation.
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into(), turns: Vec::new(), summary: None, continuation: Vec::new() }
    }

    /// Append a turn.
    pub fn push(&mut self, run: AgentRun) {
        self.turns.push(run);
    }

    /// Record how a turn ended, on the turn it names.
    ///
    /// The terminal is the one value that says so, so the transcript cannot disagree with what the
    /// host delivered. A terminal for a turn this conversation does not have is still recorded: a
    /// reopened file may hold a turn whose start was saved and whose ending was not.
    pub fn settle(&mut self, terminal: &crate::TurnTerminal, request: &str) -> &mut AgentRun {
        let turn = terminal.turn();
        if let Some(i) = self.turns.iter().position(|r| r.turn == turn) {
            self.turns[i].settle(terminal);
            return &mut self.turns[i];
        }
        let mut run = AgentRun::new(turn, request);
        run.settle(terminal);
        self.turns.push(run);
        let last = self.turns.len() - 1;
        &mut self.turns[last]
    }

    /// Apply an event to the turn it names, creating the turn if it is new.
    pub fn apply(&mut self, event: &AgentEvent, request: &str, limits: &Limits) -> &mut AgentRun {
        let turn = event.turn();
        if let Some(i) = self.turns.iter().position(|r| r.turn == turn) {
            self.turns[i].apply(event, limits);
            return &mut self.turns[i];
        }
        let mut run = AgentRun::new(turn, request);
        run.apply(event, limits);
        self.turns.push(run);
        let last = self.turns.len() - 1;
        &mut self.turns[last]
    }

    /// Settle every unfinished turn: a conversation read from a file cannot have a running one.
    pub fn settle_on_reopen(&mut self) {
        for run in &mut self.turns {
            run.settle_on_reopen();
        }
    }

    /// Characters of text this conversation holds, by the estimator compaction uses.
    pub fn estimated_chars(&self) -> usize {
        let summary = self.summary.as_deref().map(str::len).unwrap_or(0);
        summary + self.turns.iter().map(estimated_chars).sum::<usize>()
    }
}

/// Characters one turn is estimated to cost a model.
///
/// A character count is a stand-in for tokens: it needs no provider tokenizer, it never undercounts
/// a CJK answer the way a word count would, and it is stable across providers.
pub fn estimated_chars(run: &AgentRun) -> usize {
    let tools: usize = run.tools.iter().map(|t| t.name.len() + 8).sum();
    run.request.len() + run.answer.len() + tools + 16
}

/// The whole transcript as it is written to a file.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transcript {
    /// The format version, so a reader can refuse or migrate.
    pub version: u32,
    /// The conversations, oldest first.
    #[serde(default)]
    pub conversations: Vec<Conversation>,
}

/// The transcript format version this build writes.
pub const TRANSCRIPT_VERSION: u32 = 1;

impl Default for Transcript {
    fn default() -> Self {
        Self { version: TRANSCRIPT_VERSION, conversations: Vec::new() }
    }
}

impl Transcript {
    /// The transcript as the format layer's opaque payload.
    pub fn to_agent_data(&self) -> figcraft_format::agent::AgentData {
        // A shape this crate owns; the format layer carries it without understanding it.
        match serde_json::to_value(self) {
            Ok(json) => figcraft_format::agent::AgentData::new(json),
            // Serialization of plain data cannot fail, but an error here must not lose a document:
            // write nothing rather than a broken member.
            Err(_) => figcraft_format::agent::AgentData::new(Value::Null),
        }
    }

    /// Read a transcript from a payload, ignoring a shape this build does not know.
    ///
    /// A newer version, or a payload that is not a transcript, reads as empty: the Agent's history
    /// is not worth refusing a file for.
    pub fn from_agent_data(data: &figcraft_format::agent::AgentData) -> Self {
        if data.json.is_null() {
            return Self::default();
        }
        let mut t: Self = match serde_json::from_value(data.json.clone()) {
            Ok(t) => t,
            Err(_) => return Self::default(),
        };
        if t.version > TRANSCRIPT_VERSION {
            return Self::default();
        }
        for c in &mut t.conversations {
            c.settle_on_reopen();
        }
        t
    }

    /// Whether there is nothing to keep.
    pub fn is_empty(&self) -> bool {
        self.conversations.iter().all(|c| c.turns.is_empty())
    }

    /// The conversation with `id`, if any.
    pub fn conversation(&self, id: &str) -> Option<&Conversation> {
        self.conversations.iter().find(|c| c.id == id)
    }

    /// The conversation with `id`, creating it.
    pub fn conversation_mut(&mut self, id: &str) -> &mut Conversation {
        if let Some(i) = self.conversations.iter().position(|c| c.id == id) {
            return &mut self.conversations[i];
        }
        self.conversations.push(Conversation::new(id));
        let last = self.conversations.len() - 1;
        &mut self.conversations[last]
    }

    /// Characters the whole transcript is estimated to cost.
    pub fn estimated_chars(&self) -> usize {
        self.conversations.iter().map(Conversation::estimated_chars).sum()
    }
}

/// What compaction did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compaction {
    /// Turns removed and replaced by `summary`.
    pub removed: usize,
    /// Turns kept, most recent last.
    pub kept: usize,
}

/// Keep a conversation inside `budget` characters, dropping the oldest turns first.
///
/// The most recent `keep_recent` complete turns always survive, together with the summary, so the
/// model can still answer a follow-up. The removed turns are folded into one explicit summary line,
/// never silently discarded: a person reading the transcript can see what was elided.
///
/// Returns what changed, or `None` when the conversation already fits.
pub fn compact(conversation: &mut Conversation, budget: usize, keep_recent: usize) -> Option<Compaction> {
    if conversation.estimated_chars() <= budget || conversation.turns.len() <= keep_recent {
        return None;
    }
    let keep_from = conversation.turns.len().saturating_sub(keep_recent);
    let removed: Vec<AgentRun> = conversation.turns.drain(..keep_from).collect();
    if removed.is_empty() {
        return None;
    }
    let summary = summary_of(&conversation.summary, &removed);
    conversation.summary = Some(summary);
    Some(Compaction { removed: removed.len(), kept: conversation.turns.len() })
}

/// The summary line: what earlier turns were about, kept short.
///
/// A failed summarization leaves the previous summary alone: an unusable summary is worse than an
/// old one.
fn summary_of(previous: &Option<String>, removed: &[AgentRun]) -> String {
    const MAX: usize = 2000;
    let mut parts: Vec<String> = Vec::new();
    if let Some(p) = previous {
        // The earlier summary is carried forward, shortened, so repeated compaction does not grow it
        // without bound.
        parts.push(clip(p, MAX / 3));
    }
    // What each removed turn asked for, in order, clipped: this is what a follow-up needs.
    for run in removed {
        let request = run.request.split_whitespace().collect::<Vec<_>>().join(" ");
        let outcome = match run.outcome {
            // A turn that completed or committed did what it was asked; the summary does not need to
            // distinguish them, and saying "committed" of a read-only turn would be wrong.
            Some(Outcome::Completed | Outcome::Committed) => "",
            Some(Outcome::Conflict) => " (not applied: the document changed)",
            Some(Outcome::Failed) => " (failed)",
            Some(Outcome::Cancelled) => " (cancelled)",
            Some(Outcome::Interrupted) => " (interrupted)",
            None => " (unfinished)",
        };
        parts.push(format!("- {}{}", clip(&request, 160), outcome));
    }
    clip(&parts.join("\n"), MAX)
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// The turns a model should see for a follow-up: the summary as a preamble, then the kept turns.
///
/// A provider integration sends this instead of the whole conversation, so compaction actually
/// affects the request rather than only the file.
pub fn context_for_model(conversation: &Conversation) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(summary) = &conversation.summary {
        out.push(json!({"role": "user", "content": format!("Earlier in this conversation:\n{summary}")}));
        out.push(json!({"role": "assistant", "content": "Understood."}));
    }
    for run in &conversation.turns {
        out.push(json!({"role": "user", "content": run.request}));
        if !run.answer.is_empty() {
            out.push(json!({"role": "assistant", "content": run.answer}));
        }
    }
    out
}

/// Rig's continuation messages from stored JSON, dropping anything Rig would reject.
///
/// The transcript stores these as plain JSON so the schema stays this crate's and `figcraft-format`
/// keeps treating the payload as opaque. On the way back they must be Rig's `Message` again, and a
/// stored value that no longer parses (an older schema, a hand-edited file, a future field Rig does
/// not know) must not fail the turn: the conversation continues from the visible transcript with an
/// empty history rather than refusing to run.
///
/// Messages are validated one at a time so one bad entry does not discard the whole run.
pub fn continuation_messages(stored: &[Value]) -> Vec<rig_core::completion::Message> {
    stored.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect()
}

/// Whether a stored continuation is wholly readable, for a reader that wants to warn.
///
/// `continuation_messages` is deliberately lenient; this says whether anything had to be dropped.
pub fn continuation_is_intact(stored: &[Value]) -> bool {
    stored.iter().all(|v| serde_json::from_value::<rig_core::completion::Message>(v.clone()).is_ok())
}

// ---------- compaction of the messages actually sent ----------

/// Most characters of continuation history one request may carry.
///
/// The estimate counts JSON bytes, which over-estimates token cost for ASCII and under-estimates for
/// CJK; it is a bound, not a token count, and it exists so a long conversation cannot grow a request
/// without limit.
pub const MAX_REQUEST_CHARS: usize = 96 * 1024;
/// How many recent complete turns always survive compaction.
pub const KEEP_RECENT_TURNS: usize = 3;
/// Most characters one tool result may carry before it is elided.
const MAX_TOOL_RESULT_CHARS: usize = 2 * 1024;

/// What compacting the request history did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestCompaction {
    /// Turns removed from the front and folded into the summary.
    pub removed_turns: usize,
    /// Tool results shortened in place, oldest first.
    pub elided_results: usize,
    /// Whether a summary line was prepended.
    pub summarized: bool,
}

impl RequestCompaction {
    /// Whether anything changed.
    pub fn is_empty(&self) -> bool {
        self.removed_turns == 0 && self.elided_results == 0 && !self.summarized
    }
}

/// One complete turn of a request history: a user message, the assistant/tool exchange it caused, and
/// the final assistant answer.
///
/// Grouping matters because a provider rejects a `toolresult` whose `toolcall` is missing, so turns
/// are removed whole rather than message by message.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryTurn {
    /// The messages making up this turn, in order.
    pub messages: Vec<Value>,
}

impl HistoryTurn {
    /// Characters this turn is estimated to cost.
    pub fn estimated_chars(&self) -> usize {
        self.messages.iter().map(json_chars).sum()
    }

    /// Whether any message in the turn names a tool call or result.
    pub fn tool_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for m in &self.messages {
            for c in m.get("content").and_then(Value::as_array).into_iter().flatten() {
                match c.get("type").and_then(Value::as_str) {
                    Some("toolcall") => {
                        if let Some(n) = c.get("function").and_then(|f| f.get("name")).and_then(Value::as_str) {
                            names.push(n.to_string());
                        }
                    }
                    Some("toolresult") => {
                        if let Some(n) = c.get("name").and_then(Value::as_str) {
                            names.push(format!("{n}→"));
                        }
                    }
                    _ => {}
                }
            }
        }
        names
    }

    /// The text a summary should mention: the user's ask and the assistant's answer.
    pub fn gist(&self) -> String {
        let mut ask = String::new();
        let mut answer = String::new();
        for m in &self.messages {
            let role = m.get("role").and_then(Value::as_str).unwrap_or("");
            let text = text_of(m);
            if text.is_empty() {
                continue;
            }
            match role {
                "user" => {
                    if ask.is_empty() {
                        ask = text;
                    }
                }
                "assistant" => answer = text,
                _ => {}
            }
        }
        if answer.is_empty() { clip(&ask, 200) } else { format!("{} → {}", clip(&ask, 160), clip(&answer, 160)) }
    }
}

/// Split a continuation history into whole turns.
///
/// A turn starts at a `user` message that is not a tool result and runs until the next such message.
/// A leading fragment (a history that begins mid-turn) becomes its own turn so nothing is dropped
/// silently.
pub fn history_turns(history: &[Value]) -> Vec<HistoryTurn> {
    let mut turns: Vec<HistoryTurn> = Vec::new();
    let mut current: Vec<Value> = Vec::new();
    for m in history {
        let starts_turn = m.get("role").and_then(Value::as_str) == Some("user") && !is_tool_result(m);
        if starts_turn && !current.is_empty() {
            turns.push(HistoryTurn { messages: std::mem::take(&mut current) });
        }
        current.push(m.clone());
    }
    if !current.is_empty() {
        turns.push(HistoryTurn { messages: current });
    }
    turns
}

/// Whether a `user` message is a tool result the provider expects after a tool call.
fn is_tool_result(message: &Value) -> bool {
    message.get("content").and_then(Value::as_array).is_some_and(|c| c.iter().any(|p| p.get("type").and_then(Value::as_str) == Some("toolresult")))
}

/// The text in a message, joined across its text parts.
fn text_of(message: &Value) -> String {
    let mut out = String::new();
    for part in message.get("content").and_then(Value::as_array).into_iter().flatten() {
        if part.get("type").and_then(Value::as_str) == Some("text")
            && let Some(t) = part.get("text").and_then(Value::as_str)
        {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(t);
        }
    }
    out
}

/// Characters a JSON value costs, by its serialized length.
fn json_chars(v: &Value) -> usize {
    // A cheap estimate: strings dominate, and `to_string` on a large value is the thing being avoided.
    fn walk(v: &Value, n: &mut usize) {
        match v {
            Value::String(s) => *n += s.chars().count(),
            Value::Array(a) => a.iter().for_each(|x| walk(x, n)),
            Value::Object(o) => {
                for (k, x) in o {
                    *n += k.chars().count();
                    walk(x, n);
                }
            }
            Value::Null => {}
            _ => *n += 8,
        }
    }
    let mut n = 0;
    walk(v, &mut n);
    n
}

/// Shorten the largest tool results first, so the shape of the conversation survives.
///
/// A tool result is what the model does not need verbatim once the turn is over: the document holds
/// the outcome. Eliding the biggest first keeps more turns' worth of meaning than removing whole
/// turns would.
fn elide_tool_results(turns: &mut [HistoryTurn], max_chars: usize) -> usize {
    // (turn index, message index, part index, size) for every oversized tool result, biggest first.
    let mut big: Vec<(usize, usize, usize, usize)> = Vec::new();
    for (ti, turn) in turns.iter().enumerate() {
        for (mi, message) in turn.messages.iter().enumerate() {
            for (pi, part) in message.get("content").and_then(Value::as_array).into_iter().flatten().enumerate() {
                if part.get("type").and_then(Value::as_str) == Some("toolresult") {
                    let size = json_chars(part);
                    if size > max_chars {
                        big.push((ti, mi, pi, size));
                    }
                }
            }
        }
    }
    big.sort_by_key(|entry| std::cmp::Reverse(entry.3));
    let mut elided = 0;
    for (ti, mi, pi, size) in big {
        if let Some(parts) = turns[ti].messages[mi].get_mut("content").and_then(Value::as_array_mut)
            && let Some(part) = parts.get_mut(pi)
        {
            let note = format!("[{size} characters of tool output omitted to fit the request]");
            part["content"] = json!([{"type": "text", "text": note}]);
            elided += 1;
        }
    }
    elided
}

/// Fit a continuation history into `budget` characters before it is sent.
///
/// The system contract is not part of `history` and is never summarised. Three things happen, in the
/// order that loses the least meaning: the largest tool results are elided, the oldest turns are
/// folded into one explicit summary line, and the most recent [`KEEP_RECENT_TURNS`] complete turns
/// always survive. Tool call and result pairs stay in the same turn, so the provider never sees a
/// result whose call is gone.
///
/// Returns the compacted history and what was done; the caller replaces the stored history only after
/// this succeeds.
pub fn compact_request(history: &[Value], budget: usize, keep_recent: usize) -> (Vec<Value>, RequestCompaction) {
    let mut report = RequestCompaction::default();
    let mut turns = history_turns(history);
    if turns.is_empty() {
        return (Vec::new(), report);
    }
    let total: usize = turns.iter().map(HistoryTurn::estimated_chars).sum();
    if total <= budget {
        return (history.to_vec(), report);
    }

    // Large payloads first: they cost the most and mean the least once their turn is over.
    report.elided_results = elide_tool_results(&mut turns, MAX_TOOL_RESULT_CHARS);
    let after_elide: usize = turns.iter().map(HistoryTurn::estimated_chars).sum();
    if after_elide <= budget {
        return (turns.into_iter().flat_map(|t| t.messages).collect(), report);
    }

    // Then whole old turns, keeping the most recent ones.
    if turns.len() > keep_recent {
        let keep_from = turns.len() - keep_recent;
        let removed: Vec<HistoryTurn> = turns.drain(..keep_from).collect();
        report.removed_turns = removed.len();
        let mut lines: Vec<String> = Vec::new();
        for t in &removed {
            let tools = t.tool_names();
            let tool_note = if tools.is_empty() { String::new() } else { format!(" [{}]", tools.join(" ")) };
            lines.push(format!("- {}{tool_note}", t.gist()));
        }
        let summary = clip(&format!("Earlier in this conversation:\n{}", lines.join("\n")), 2000);
        let mut out: Vec<Value> = vec![
            json!({"role": "user", "content": [{"type": "text", "text": summary}]}),
            json!({"role": "assistant", "content": [{"type": "text", "text": "Understood."}]}),
        ];
        out.extend(turns.into_iter().flat_map(|t| t.messages));
        report.summarized = true;
        return (out, report);
    }

    (turns.into_iter().flat_map(|t| t.messages).collect(), report)
}
