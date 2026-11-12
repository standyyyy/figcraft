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

    /// A credential that cannot be resolved: a configuration problem the person can fix.
    pub fn credential(message: impl Into<String>) -> Self {
        Self { kind: AgentErrorKind::Configuration, message: message.into() }
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
    /// The secret the persisted reference names, or `None` when there is none.
    ///
    /// The reference is what a preferences file may hold: a variable's *name*, never a key. Where the
    /// key actually lives is the host's business -- the process environment on the desktop, app memory
    /// in a browser -- and the gateway never decides it.
    fn resolve(&self, reference: Option<&str>) -> Result<Option<Secret>, ProviderError>;
}

/// A provider key, in a type that cannot be printed by accident.
///
/// `Debug` prints `<redacted>`: a key must never reach a log, a transcript, a panel, an error message
/// or a `.figcraft` file, and the surest way is for no formatting of it to contain it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wrap a key.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The key itself, for the one place that needs it: building an authorization header.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether it holds anything but whitespace.
    pub fn is_set(&self) -> bool {
        !self.0.trim().is_empty()
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately not the value, and not its length either: a length narrows a guess.
        f.write_str("Secret(<redacted>)")
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Reads credentials from the process environment: the reference is a variable's name.
///
/// The desktop ships no key of its own and stores none; a preferences file holds the name, and the
/// value lives wherever the person put it.
#[derive(Clone, Copy, Debug, Default)]
pub struct EnvironmentCredentials;

impl Credentials for EnvironmentCredentials {
    fn resolve(&self, reference: Option<&str>) -> Result<Option<Secret>, ProviderError> {
        let Some(name) = reference.filter(|n| !n.trim().is_empty()) else { return Ok(None) };
        // A name that is not a plausible environment variable is a configuration mistake, not a
        // missing key: say which, so the message is actionable.
        if name.contains('=') || name.contains('\0') {
            return Err(ProviderError::credential("the credential name is not a valid variable name"));
        }
        match std::env::var(name) {
            Ok(v) if !v.trim().is_empty() => Ok(Some(Secret::new(v))),
            // Set but empty reads as unset: an empty key is not a key.
            Ok(_) => Ok(None),
            Err(std::env::VarError::NotPresent) => Ok(None),
            // Present but not UTF-8: cannot be sent, and saying so beats sending a mangled key.
            Err(std::env::VarError::NotUnicode(_)) => Err(ProviderError::credential(format!("`{name}` is not valid UTF-8"))),
        }
    }
}

/// Keeps the key in this process's memory only, for a platform with no environment to read.
///
/// The persisted reference is the reserved logical name [`SESSION_REFERENCE`]; a reload keeps the
/// endpoint and model and asks for the key again. Nothing here writes to storage.
#[derive(Clone, Debug, Default)]
pub struct SessionCredentials {
    key: Option<Secret>,
}

/// The reference [`SessionCredentials`] recognises. Not a variable name: it says where the key is.
pub const SESSION_REFERENCE: &str = "browser-session";

impl SessionCredentials {
    /// Hold `key` for this session.
    pub fn set(&mut self, key: impl Into<String>) {
        let key = Secret::new(key);
        self.key = if key.is_set() { Some(key) } else { None };
    }

    /// Forget the key.
    pub fn clear(&mut self) {
        self.key = None;
    }

    /// Whether a key is held.
    pub fn is_set(&self) -> bool {
        self.key.is_some()
    }
}

impl Credentials for SessionCredentials {
    fn resolve(&self, reference: Option<&str>) -> Result<Option<Secret>, ProviderError> {
        match reference {
            // No reference at all: an endpoint that needs no key.
            None => Ok(None),
            Some(r) if r.trim().is_empty() => Ok(None),
            Some(SESSION_REFERENCE) => Ok(self.key.clone()),
            Some(other) => Err(ProviderError::credential(format!("`{other}` is not a credential this platform can read"))),
        }
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
        // The reference is a name; where the key lives is the host's business. A reference that is
        // set but resolves to nothing is a configuration error, because the person asked for a key
        // and the endpoint will refuse the request without one.
        let key = match credentials.resolve(config.credential())? {
            Some(secret) => secret.expose().to_string(),
            // No reference: an endpoint that needs no credential, which a local vLLM often is.
            None if config.credential().is_none() => String::new(),
            None => {
                let name = config.credential().unwrap_or_default();
                return Err(ProviderError::new(AgentErrorKind::Authentication, format!("no credential named `{name}` is set")));
            }
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
