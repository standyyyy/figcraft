//! FigCraft's embedded Agent runtime.
//!
//! Wraps Rig's agent loop, runs tool calls against a shadow [`figcraft_engine::Session`], and
//! commits a completed turn to the live session as a single `command.batch` undo step. This crate
//! is L6 and UI-independent: the egui panel drives it through an `AgentHost` implemented by each
//! application target.
//!
//! The engine owns the provider preference data ([`figcraft_engine::prefs::AgentPrefs`]); this
//! crate validates it into a [`config::AgentConfig`] and builds Rig from it. Nothing below the UI
//! knows about Rig, and nothing here writes a document field: every edit is a command.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod config;
pub mod gateway;
pub mod history;
pub mod run;
pub mod shadow;
pub mod tools;
/// The model transport (blocking HTTP with tool calling). Native only: the browser has no sockets
/// and uses fetch, and ureq's TLS stack cannot build for wasm (dependencies are not empty).
#[cfg(not(target_arch = "wasm32"))]
pub mod transport;

#[cfg(test)]
#[path = "gateway_tests.rs"]
mod gateway_tests;

#[cfg(test)]
#[path = "shadow_tests.rs"]
mod shadow_tests;

#[cfg(test)]
#[path = "history_tests.rs"]
mod history_tests;

#[cfg(test)]
#[path = "run_tests.rs"]
mod run_tests;

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tools_tests;

pub use config::{AgentConfig, ConfigError, Route};
pub use gateway::{Credentials, Environment, ModelGateway, ProviderError};
pub use run::{AgentEvent, AgentRun, CancelToken, Limits, Outcome, RetryPolicy, ToolCall};

/// What went wrong, classified once at the Agent boundary.
///
/// The UI chooses how to react from this variant, never by inspecting an error string. Each kind
/// maps to a short action a person can take (see [`AgentErrorKind::action`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentErrorKind {
    /// The provider settings are missing or wrong.
    Configuration,
    /// The provider refused the credential, or none was found.
    Authentication,
    /// The provider could not be reached at all.
    Unreachable,
    /// The request took too long.
    Timeout,
    /// The provider answered something this client does not understand.
    Protocol,
    /// The model itself failed (a refusal, a malformed answer).
    Model,
    /// A tool call failed.
    Tool,
    /// The live document changed while the turn ran, so committing would replay stale commands.
    Conflict,
    /// The turn was cancelled.
    Cancelled,
}

impl AgentErrorKind {
    /// A short line saying what to do about it.
    pub fn action(self) -> &'static str {
        match self {
            AgentErrorKind::Configuration => "Check the provider URL and model in Preferences.",
            AgentErrorKind::Authentication => "Check the API key the provider expects.",
            AgentErrorKind::Unreachable => "Check that the provider is running and reachable.",
            AgentErrorKind::Timeout => "Try again, or raise the timeout in Preferences.",
            AgentErrorKind::Protocol => "The provider answered something unexpected; check its URL.",
            AgentErrorKind::Model => "Ask again; the model's answer was unusable.",
            AgentErrorKind::Tool => "The change could not be applied; adjust the request.",
            AgentErrorKind::Conflict => "The document changed while the Agent worked. Run the request again.",
            AgentErrorKind::Cancelled => "Stopped.",
        }
    }

    /// Whether a retry could plausibly succeed unchanged.
    pub fn is_retryable(self) -> bool {
        matches!(self, AgentErrorKind::Unreachable | AgentErrorKind::Timeout)
    }
}

/// A named failure: what kind, and a readable message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentError {
    pub kind: AgentErrorKind,
    pub message: String,
}

impl AgentError {
    pub fn new(kind: AgentErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }

    /// What a person should do next.
    pub fn action(&self) -> &'static str {
        self.kind.action()
    }
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AgentError {}

impl From<ConfigError> for AgentError {
    fn from(e: ConfigError) -> Self {
        AgentError::new(AgentErrorKind::Configuration, e.to_string())
    }
}

impl From<ProviderError> for AgentError {
    fn from(e: ProviderError) -> Self {
        AgentError::new(e.kind, e.message)
    }
}
