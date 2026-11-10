//! How the Agent reaches an OpenAI-compatible provider through Rig.
//!
//! Construction is separate from the run policy: another compatible route can be added without the
//! UI or the turn lifecycle changing. The credential is resolved from the environment at the moment
//! a request is built, so it is never held in preferences, logged, or written to a file.

use rig_core::providers::openai::{OpenAI, OpenAIConfig};

use crate::AgentErrorKind;
use crate::config::{AgentConfig, Route};

/// A provider's error, classified once so no caller inspects an error string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderError {
    pub kind: AgentErrorKind,
    /// A short line a person can act on. Provider bodies are summarized, never echoed whole.
    pub message: String,
}

impl ProviderError {
    pub fn new(kind: AgentErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ProviderError {}

/// What reads a credential: a name in the environment, resolved on use.
pub trait Credentials {
    /// The secret `name` holds, or `None`.
    fn resolve(&self, name: &str) -> Option<String>;
}

/// Reads credentials from the process environment.
#[derive(Clone, Copy, Debug, Default)]
pub struct Environment;

impl Credentials for Environment {
    fn resolve(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.trim().is_empty())
    }
}

/// Builds and holds the Rig client for one [`AgentConfig`].
///
/// The gateway owns the endpoint so the rest of the crate never touches Rig's construction, and so
/// a second route can be supported later by adding a variant here.
pub struct ModelGateway {
    client: OpenAI,
    model: String,
    route: Route,
    timeout_seconds: u32,
}

impl std::fmt::Debug for ModelGateway {
    /// Never prints the client (it holds the credential); only what a person needs to diagnose.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelGateway").field("model", &self.model).field("route", &self.route).finish()
    }
}

impl ModelGateway {
    /// Build a gateway for `config`, resolving its credential through `credentials`.
    ///
    /// An endpoint that needs no credential is allowed: a local vLLM often runs without one, and
    /// refusing that case would make the common local setup impossible.
    pub fn build(config: &AgentConfig, credentials: &dyn Credentials) -> Result<Self, ProviderError> {
        let key = match config.credential() {
            Some(name) => credentials.resolve(name).ok_or_else(|| {
                ProviderError::new(AgentErrorKind::Authentication, format!("no credential named `{name}` is set"))
            })?,
            // Rig needs some key to build a client; a local server ignores it.
            None => String::new(),
        };
        let openai = OpenAIConfig::new(key).with_base_url(config.base_url());
        let client = openai.client();
        debug_assert_eq!(config.route(), Route::ChatCompletions, "only the chat route is supported");
        Ok(Self { client, model: config.model().to_string(), route: config.route(), timeout_seconds: config.timeout_seconds() })
    }

    /// The provider client, for the turn runner to build its model from.
    ///
    /// Rig 0.44 keeps the concrete chat model type crate-private, so the runner drives it through
    /// this client rather than a named type. A second route adds a method here.
    pub fn client(&self) -> &rig_core::providers::openai::OpenAI {
        let _ = self.route;
        &self.client
    }

    /// The chat-completions model name this gateway addresses.
    pub fn chat_model_name(&self) -> &str {
        &self.model
    }

    /// The model name, as configured.
    pub fn model_name(&self) -> &str {
        &self.model
    }

    /// Read an alias the server answered with: opaque, never validated against the configured name.
    pub fn accept_alias(&self, returned: Option<&str>) {
        // A server may answer under a different name (a provider alias, a quantised variant). The
        // configured name identifies what we asked for; the returned one is informational.
        let _ = returned;
    }

    /// How long one request may take.
    pub fn timeout_seconds(&self) -> u32 {
        self.timeout_seconds
    }
}

/// Classify a transport or protocol failure by what a person can do about it.
///
/// The body of a provider response is summarized to a short line: it can contain the request, and
/// must never be logged verbatim.
pub fn classify_http(status: u16, body: &str) -> ProviderError {
    let short = summarize(body);
    match status {
        401 | 403 => ProviderError::new(AgentErrorKind::Authentication, format!("the provider refused the credential ({status}): {short}")),
        404 => ProviderError::new(AgentErrorKind::Protocol, format!("the provider has no such route ({status}): {short}")),
        408 | 504 => ProviderError::new(AgentErrorKind::Timeout, format!("the provider timed out ({status})")),
        429 => ProviderError::new(AgentErrorKind::Unreachable, format!("the provider is rate limiting ({status}); try again shortly")),
        500..=599 => ProviderError::new(AgentErrorKind::Unreachable, format!("the provider failed ({status}): {short}")),
        _ => ProviderError::new(AgentErrorKind::Protocol, format!("the provider answered {status}: {short}")),
    }
}

/// A short, single-line summary of a response body: enough to diagnose, never the whole thing.
fn summarize(body: &str) -> String {
    const MAX: usize = 200;
    let line = body.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let mut out: String = line.chars().take(MAX).collect();
    if line.chars().count() > MAX {
        out.push('…');
    }
    out
}
