//! The Agent's conversation transcript as an archive member: `agent/conversations.json`.
//!
//! The format crate keeps transcripts **opaque**: it reads and writes capped bytes, and knows
//! nothing about Rig or the Agent's types. That keeps Rig out of the file format (a dependency rule)
//! while still letting a transcript travel with the file.
//!
//! A transcript is file metadata, not document data, so it never enters undo history or a live
//! collaboration session: only the `.figcraft` archive carries it.

use serde_json::Value;

/// The archive member holding the transcript.
pub const AGENT_MEMBER: &str = "agent/conversations.json";
/// Most bytes the member may hold. A transcript is context, not a document: past this it is skipped
/// with an issue rather than read, so an oversized member cannot be allocated.
pub const MAX_AGENT_BYTES: usize = 8 * 1024 * 1024;
/// Most conversations a file keeps.
pub const MAX_CONVERSATIONS: usize = 32;
/// Most turns in one conversation.
pub const MAX_TURNS: usize = 200;
/// Most characters of decoded text across the whole transcript.
pub const MAX_AGENT_TEXT: usize = 4 * 1024 * 1024;

/// A transcript read from a file, or to be written to one.
///
/// The payload is a versioned JSON value whose shape this crate does not own: the Agent decides it,
/// and a future version can add fields an older reader ignores.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentData {
    /// The versioned transcript JSON.
    pub json: Value,
}

impl AgentData {
    /// Wrap `json` as a transcript.
    pub fn new(json: Value) -> Self {
        Self { json }
    }

    /// Whether there is nothing to write.
    pub fn is_empty(&self) -> bool {
        match &self.json {
            Value::Null => true,
            Value::Array(a) => a.is_empty(),
            Value::Object(o) => o.get("conversations").and_then(Value::as_array).is_none_or(|c| c.is_empty()),
            _ => false,
        }
    }

    /// The bytes written to the archive.
    pub fn to_bytes(&self) -> Result<Vec<u8>, FormatError> {
        let bytes = serde_json::to_vec(&self.json).map_err(|e| FormatError::Corrupt(format!("the transcript could not be written: {e}")))?;
        if bytes.len() > MAX_AGENT_BYTES {
            return Err(FormatError::Corrupt(format!("the transcript is {} bytes, over the {MAX_AGENT_BYTES} limit", bytes.len())));
        }
        Ok(bytes)
    }

    /// Read a transcript from `bytes`, or explain why it was skipped.
    ///
    /// A malformed or oversized member is **skipped with an issue**, never fatal: a file whose
    /// transcript is damaged still opens, because the document is what matters.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_AGENT_BYTES {
            return Err(format!("{} bytes, over the {MAX_AGENT_BYTES} limit", bytes.len()));
        }
        let text = std::str::from_utf8(bytes).map_err(|e| format!("not UTF-8: {e}"))?;
        let json: Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
        let data = Self { json };
        data.counts().map_err(|e| e.to_string())?;
        Ok(data)
    }

    /// The conversations in the transcript.
    fn conversations(&self) -> &[Value] {
        match &self.json {
            Value::Array(a) => a,
            Value::Object(o) => o.get("conversations").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]),
            _ => &[],
        }
    }

    /// Check the caps a model or a collaborator cannot be trusted to respect.
    fn counts(&self) -> Result<(), CountError> {
        let conversations = self.conversations();
        if conversations.len() > MAX_CONVERSATIONS {
            return Err(CountError::Conversations(conversations.len()));
        }
        let mut text = 0usize;
        for c in conversations {
            let turns = c.get("turns").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
            if turns.len() > MAX_TURNS {
                return Err(CountError::Turns(turns.len()));
            }
            for turn in turns {
                text = text.saturating_add(text_len(turn));
                if text > MAX_AGENT_TEXT {
                    return Err(CountError::Text(text));
                }
            }
        }
        Ok(())
    }
}

/// Characters of text in one turn's JSON, bounded so a hostile member cannot make this expensive.
fn text_len(turn: &Value) -> usize {
    const MAX_PER_TURN: usize = 64 * 1024;
    let mut n = 0usize;
    let mut stack = vec![turn];
    while let Some(v) = stack.pop() {
        match v {
            Value::String(s) => n = n.saturating_add(s.chars().count()),
            Value::Array(a) => stack.extend(a.iter()),
            Value::Object(o) => stack.extend(o.values()),
            _ => {}
        }
        if n > MAX_PER_TURN {
            return MAX_PER_TURN;
        }
    }
    n
}

/// Why a transcript was refused.
#[derive(Debug)]
enum CountError {
    Conversations(usize),
    Turns(usize),
    Text(usize),
}

impl std::fmt::Display for CountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CountError::Conversations(n) => write!(f, "{n} conversations, over the {MAX_CONVERSATIONS} limit"),
            CountError::Turns(n) => write!(f, "{n} turns in one conversation, over the {MAX_TURNS} limit"),
            CountError::Text(n) => write!(f, "{n} characters of text, over the {MAX_AGENT_TEXT} limit"),
        }
    }
}

use super::FormatError;
