//! The Agent's provider configuration: validated from plain engine preferences.
//!
//! The engine owns the preference data (it serializes to disk and must never depend on Rig), and
//! this crate turns it into a validated [`AgentConfig`]. Validation happens once, here, so the rest
//! of the crate can assume a usable endpoint.

use figcraft_engine::prefs::{AGENT_TIMEOUT_MAX, AGENT_TIMEOUT_MIN, AgentPrefs, DEFAULT_AGENT_ROUTE, DEFAULT_AGENT_TIMEOUT_SECONDS};

/// Why a configuration was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// The Agent is turned off in Preferences.
    Disabled,
    /// The base URL is blank.
    BlankUrl,
    /// The base URL is not an `http://` URL.
    ///
    /// HTTPS is refused until a TLS transport FigCraft already ships can be reused: adding a second
    /// TLS backend for the Agent alone is not a decision this change makes.
    NotHttp(String),
    /// The base URL cannot be parsed.
    BadUrl(String),
    /// The model name is blank.
    BlankModel,
    /// The route is not one this gateway speaks.
    UnknownRoute(String),
    /// The timeout is outside [`AGENT_TIMEOUT_MIN`]..=[`AGENT_TIMEOUT_MAX`].
    TimeoutOutOfRange(u32),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Disabled => write!(f, "the Agent is turned off in Preferences"),
            ConfigError::BlankUrl => write!(f, "set the provider's base URL in Preferences"),
            ConfigError::NotHttp(url) => {
                write!(f, "`{url}` is not an http:// URL; the Agent speaks HTTP until a shared TLS transport is available")
            }
            ConfigError::BadUrl(url) => write!(f, "`{url}` is not a usable URL"),
            ConfigError::BlankModel => write!(f, "set the provider's model name in Preferences"),
            ConfigError::UnknownRoute(route) => write!(f, "`{route}` is not a route this Agent speaks (chatCompletions)"),
            ConfigError::TimeoutOutOfRange(seconds) => {
                write!(f, "a {seconds} s timeout is outside {AGENT_TIMEOUT_MIN}..={AGENT_TIMEOUT_MAX} s")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// A validated provider endpoint.
///
/// Constructed through [`AgentConfig::from_prefs`], so an instance is always usable.
#[derive(Clone, PartialEq, Eq)]
pub struct AgentConfig {
    base_url: String,
    model: String,
    credential: Option<String>,
    route: Route,
    timeout_seconds: u32,
}

/// Which OpenAI-compatible route the gateway calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// `POST {base}/chat/completions`.
    ChatCompletions,
}

impl Route {
    fn parse(name: &str) -> Result<Self, ConfigError> {
        match name {
            DEFAULT_AGENT_ROUTE => Ok(Route::ChatCompletions),
            other => Err(ConfigError::UnknownRoute(other.to_string())),
        }
    }

    /// The route's path below the base URL.
    pub fn path(self) -> &'static str {
        match self {
            Route::ChatCompletions => "chat/completions",
        }
    }
}

impl AgentConfig {
    /// Validate `prefs`.
    pub fn from_prefs(prefs: &AgentPrefs) -> Result<Self, ConfigError> {
        if !prefs.enabled {
            return Err(ConfigError::Disabled);
        }
        let base_url = normalize_base_url(&prefs.base_url)?;
        let model = prefs.model.trim();
        if model.is_empty() {
            return Err(ConfigError::BlankModel);
        }
        let route_name = if prefs.route.trim().is_empty() { DEFAULT_AGENT_ROUTE } else { prefs.route.trim() };
        let route = Route::parse(route_name)?;
        let timeout_seconds = if prefs.timeout_seconds == 0 { DEFAULT_AGENT_TIMEOUT_SECONDS } else { prefs.timeout_seconds };
        if !(AGENT_TIMEOUT_MIN..=AGENT_TIMEOUT_MAX).contains(&timeout_seconds) {
            return Err(ConfigError::TimeoutOutOfRange(timeout_seconds));
        }
        Ok(Self { base_url, model: model.to_string(), credential: prefs.credential.clone().filter(|c| !c.trim().is_empty()), route, timeout_seconds })
    }

    /// The normalized base URL: `http://host/v1`, no trailing slash, `/v1` exactly once.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The model name, as configured.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The name of the credential the request's key comes from, if any. Never the secret.
    pub fn credential(&self) -> Option<&str> {
        self.credential.as_deref()
    }

    /// The route this gateway calls.
    pub fn route(&self) -> Route {
        self.route
    }

    /// How long one request may take.
    pub fn timeout_seconds(&self) -> u32 {
        self.timeout_seconds
    }

    /// The same config with a shorter timeout, for a test that must not wait the real one.
    ///
    /// Not `#[cfg(test)]`: the live tests and other crates' tests use it too, and a two-second
    /// timeout is a legitimate configuration.
    pub fn with_timeout_seconds(mut self, seconds: u32) -> Self {
        self.timeout_seconds = seconds;
        self
    }
}

impl std::fmt::Debug for AgentConfig {
    /// Redacts the credential reference, so a log line cannot leak where the key is.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentConfig")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("credential", &self.credential.as_ref().map(|_| "<redacted>"))
            .field("route", &self.route)
            .field("timeout_seconds", &self.timeout_seconds)
            .finish()
    }
}

/// Normalize a base URL: trim, drop trailing slashes, and make `/v1` appear exactly once.
///
/// People paste base URLs in every shape the OpenAI docs and vLLM use: with and without `/v1`, with
/// a trailing slash, and sometimes with the full `/v1/chat/completions`. All of them mean the same
/// endpoint, so all of them are accepted.
fn normalize_base_url(raw: &str) -> Result<String, ConfigError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(ConfigError::BlankUrl);
    }
    if !raw.starts_with("http://") && !raw.starts_with("https://") {
        return Err(ConfigError::BadUrl(raw.to_string()));
    }
    if raw.starts_with("https://") {
        return Err(ConfigError::NotHttp(raw.to_string()));
    }
    // A pasted full route ends at the API root.
    let mut url = raw.trim_end_matches('/').to_string();
    if let Some(stripped) = url.strip_suffix("/chat/completions") {
        url = stripped.trim_end_matches('/').to_string();
    }
    // `/v1` exactly once.
    if let Some(stripped) = url.strip_suffix("/v1") {
        url = stripped.trim_end_matches('/').to_string();
    }
    if url.is_empty() || url == "http:/" {
        return Err(ConfigError::BadUrl(raw.to_string()));
    }
    Ok(format!("{url}/v1"))
}
