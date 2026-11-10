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
}

impl Conversation {
    /// A new, empty conversation.
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into(), turns: Vec::new(), summary: None }
    }

    /// Append a turn.
    pub fn push(&mut self, run: AgentRun) {
        self.turns.push(run);
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
            Some(Outcome::Committed) => "",
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
