//! How a turn ends, as a type that cannot hold a replay on failure.
//!
//! The defect this replaces: the worker reported "how it ended" as a bare event and carried the
//! replay in a separate `Option`. Nothing stopped `Failed` from arriving with `Some(replay)`, and
//! the UI had no way to tell a stale envelope from a fresh one. Two unrelated fields meant two
//! things that could disagree.
//!
//! Here success and its replay are one value. A failure, a cancellation, a timeout, an exhausted
//! budget or a host shutdown is a *different variant*, so it cannot carry a replay at all: there is
//! no field to put one in. The compiler enforces the contract instead of a review comment.

use serde_json::Value;

use crate::AgentErrorKind;
use crate::shadow::ReplayStep;

/// The only thing allowed to carry a document edit out of a turn.
///
/// It names the document and the revision the turn was built from, because the UI must recheck both
/// immediately before applying the replay: a turn computed against a document that has since
/// changed would silently overwrite someone's edit.
#[derive(Clone, Debug, PartialEq)]
pub struct CommitEnvelope {
    /// The turn this envelope belongs to, so a late result cannot commit into a newer turn.
    pub turn: u64,
    /// The document the turn inspected.
    pub document_uid: u64,
    /// Its revision when the turn started.
    pub base_revision: u64,
    /// The commands, in order, to replay as one `command.batch`.
    pub replay: Vec<ReplayStep>,
    /// The undo entry's name (`AI: <the request>`).
    pub undo_label: String,
}

impl CommitEnvelope {
    /// A new envelope.
    pub fn new(turn: u64, document_uid: u64, base_revision: u64, replay: Vec<ReplayStep>, undo_label: impl Into<String>) -> Self {
        Self { turn, document_uid, base_revision, replay, undo_label: undo_label.into() }
    }

    /// Whether there is anything to apply.
    ///
    /// A successful turn may change nothing (it only read the document); that is still a success,
    /// and the UI commits no batch for it.
    pub fn is_empty(&self) -> bool {
        self.replay.is_empty()
    }

    /// The replay as the JSON a `command.batch` takes.
    pub fn batch_commands(&self) -> Vec<Value> {
        self.replay.iter().map(|s| serde_json::json!({"command": s.command, "params": s.params})).collect()
    }

    /// Whether this envelope may be applied to a document at `uid`/`revision` right now.
    ///
    /// Both must match: the uid keeps a turn from landing in another open file, and the exact
    /// revision keeps it from overwriting an edit made while the Agent worked.
    pub fn accepts(&self, document_uid: u64, revision: u64) -> bool {
        self.document_uid == document_uid && self.base_revision == revision
    }

    /// Why `accepts` refused, for a message a person can act on.
    pub fn refusal(&self, document_uid: u64, revision: u64) -> Option<String> {
        if self.document_uid != document_uid {
            return Some("the Agent finished in a different file than the one in front".to_string());
        }
        if self.base_revision != revision {
            return Some("the document changed while the Agent worked".to_string());
        }
        None
    }
}

/// How a turn ended. Exactly one of these, and only [`Success`](Self::Success) holds an edit.
#[derive(Clone, Debug, PartialEq)]
pub enum TurnTerminal {
    /// The model answered. `commit` is `None` when the turn changed nothing.
    Success { turn: u64, answer: String, commit: Option<CommitEnvelope> },
    /// It failed: a provider error, a tool failure, a malformed answer.
    Failed { turn: u64, kind: AgentErrorKind, message: String },
    /// It was cancelled.
    Cancelled { turn: u64 },
    /// It hit a limit (rounds, tool calls, wall clock) before answering.
    ///
    /// Its work is **not** applied: a turn cut short is not a finished turn, and applying half of one
    /// would leave a document the user never agreed to. (The earlier build did apply it, which made a
    /// limit look like a success with a partial screen.)
    LimitReached { turn: u64, kind: AgentErrorKind, message: String },
}

impl TurnTerminal {
    /// The turn this terminal event belongs to.
    pub fn turn(&self) -> u64 {
        match self {
            TurnTerminal::Success { turn, .. }
            | TurnTerminal::Failed { turn, .. }
            | TurnTerminal::Cancelled { turn }
            | TurnTerminal::LimitReached { turn, .. } => *turn,
        }
    }

    /// The envelope to apply, if this outcome may change the document.
    ///
    /// The only path to a replay: every other variant returns `None`, whatever the turn did before it
    /// stopped.
    pub fn commit(&self) -> Option<&CommitEnvelope> {
        match self {
            TurnTerminal::Success { commit, .. } => commit.as_ref(),
            TurnTerminal::Failed { .. } | TurnTerminal::Cancelled { .. } | TurnTerminal::LimitReached { .. } => None,
        }
    }

    /// Whether the turn ended well.
    pub fn is_success(&self) -> bool {
        matches!(self, TurnTerminal::Success { .. })
    }

    /// A message for the panel, or `None` for a success with an answer.
    pub fn message(&self) -> Option<&str> {
        match self {
            TurnTerminal::Success { .. } => None,
            TurnTerminal::Failed { message, .. } | TurnTerminal::LimitReached { message, .. } => Some(message),
            TurnTerminal::Cancelled { .. } => Some("Stopped."),
        }
    }
}

/// What a turn builds up while it runs, before it ends.
///
/// A shadow session and its recorded commands live here; only [`finish`](Self::finish) turns them
/// into a [`TurnTerminal`], and only with a success can they become a [`CommitEnvelope`].
#[derive(Debug, Default)]
pub struct TurnProgress {
    shadow: Option<crate::shadow::Shadow>,
    answer: String,
    tools: Vec<(String, bool)>,
}

impl TurnProgress {
    /// A turn with no shadow yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach the shadow the turn runs against.
    pub fn with_shadow(shadow: crate::shadow::Shadow) -> Self {
        Self { shadow: Some(shadow), ..Self::default() }
    }

    /// The shadow, for running a tool.
    pub fn shadow(&mut self) -> Option<&mut crate::shadow::Shadow> {
        self.shadow.as_mut()
    }

    /// Record a tool result for the transcript.
    pub fn tool(&mut self, name: &str, ok: bool) {
        self.tools.push((name.to_string(), ok));
    }

    /// Append answer text.
    pub fn text(&mut self, text: &str) {
        self.answer.push_str(text);
    }

    /// The answer so far.
    pub fn answer(&self) -> &str {
        &self.answer
    }

    /// The tool calls so far.
    pub fn tools(&self) -> &[(String, bool)] {
        &self.tools
    }

    /// End the turn successfully, taking its replay with it.
    pub fn finish(self, turn: u64, document_uid: u64, base_revision: u64, undo_label: impl Into<String>) -> TurnTerminal {
        let replay = self.shadow.map(|s| s.replay().to_vec()).unwrap_or_default();
        let commit = if replay.is_empty() { None } else { Some(CommitEnvelope::new(turn, document_uid, base_revision, replay, undo_label)) };
        TurnTerminal::Success { turn, answer: self.answer, commit }
    }

    /// End the turn without applying anything, dropping the shadow and its work.
    pub fn abandon(self, turn: u64, kind: AgentErrorKind, message: impl Into<String>) -> TurnTerminal {
        // The shadow is dropped here and never read: this is the whole point of the type.
        TurnTerminal::Failed { turn, kind, message: message.into() }
    }
}

#[cfg(test)]
#[path = "terminal_tests.rs"]
mod tests;
