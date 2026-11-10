//! Provider configuration, the Rig gateway, and error classification.

use figcraft_engine::prefs::{AgentPrefs, DEFAULT_AGENT_TIMEOUT_SECONDS};

use crate::AgentErrorKind;
use crate::config::{AgentConfig, ConfigError};
use crate::gateway::{Credentials, Environment, ModelGateway, classify_http};

fn prefs(base_url: &str, model: &str) -> AgentPrefs {
    AgentPrefs { enabled: true, base_url: base_url.into(), model: model.into(), ..AgentPrefs::default() }
}

#[test]
fn a_local_http_endpoint_is_accepted() {
    let c = AgentConfig::from_prefs(&prefs("http://192.168.15.125:8111", "DeepSeek-V4.1-Flash-1017")).expect("valid");
    assert_eq!(c.base_url(), "http://192.168.15.125:8111/v1", "`/v1` is appended exactly once");
    assert_eq!(c.model(), "DeepSeek-V4.1-Flash-1017");
    assert_eq!(c.credential(), None, "a local endpoint needs no credential");
    assert_eq!(c.timeout_seconds(), DEFAULT_AGENT_TIMEOUT_SECONDS);
}

#[test]
fn every_shape_of_base_url_normalizes_to_one_endpoint() {
    // People paste base URLs the way the OpenAI docs and vLLM write them.
    for raw in [
        "http://127.0.0.1:8000",
        "http://127.0.0.1:8000/",
        "http://127.0.0.1:8000/v1",
        "http://127.0.0.1:8000/v1/",
        "  http://127.0.0.1:8000/v1  ",
        "http://127.0.0.1:8000/v1/chat/completions",
    ] {
        let c = AgentConfig::from_prefs(&prefs(raw, "m")).unwrap_or_else(|e| panic!("{raw} should be accepted: {e}"));
        assert_eq!(c.base_url(), "http://127.0.0.1:8000/v1", "{raw} normalizes");
    }
}

#[test]
fn https_is_refused_until_a_shared_transport_exists() {
    // The point of the rule: no second TLS backend joins the build for the Agent alone.
    let e = AgentConfig::from_prefs(&prefs("https://api.example.com/v1", "m")).expect_err("https is refused");
    assert!(matches!(e, ConfigError::NotHttp(_)), "got {e:?}");
    assert!(e.to_string().contains("http://"), "the message says what is accepted: {e}");
}

#[test]
fn a_blank_or_unusable_url_is_refused() {
    assert_eq!(AgentConfig::from_prefs(&prefs("", "m")), Err(ConfigError::BlankUrl));
    assert_eq!(AgentConfig::from_prefs(&prefs("   ", "m")), Err(ConfigError::BlankUrl));
    assert!(matches!(AgentConfig::from_prefs(&prefs("192.168.1.1:8000", "m")), Err(ConfigError::BadUrl(_))));
    assert!(matches!(AgentConfig::from_prefs(&prefs("ftp://x", "m")), Err(ConfigError::BadUrl(_))));
}

#[test]
fn a_blank_model_is_refused() {
    assert_eq!(AgentConfig::from_prefs(&prefs("http://x:1", "")), Err(ConfigError::BlankModel));
    assert_eq!(AgentConfig::from_prefs(&prefs("http://x:1", "   ")), Err(ConfigError::BlankModel));
}

#[test]
fn a_disabled_agent_is_refused() {
    let mut p = prefs("http://x:1", "m");
    p.enabled = false;
    assert_eq!(AgentConfig::from_prefs(&p), Err(ConfigError::Disabled));
}

#[test]
fn the_timeout_is_bounded() {
    let mut p = prefs("http://x:1", "m");
    p.timeout_seconds = 1;
    assert!(matches!(AgentConfig::from_prefs(&p), Err(ConfigError::TimeoutOutOfRange(1))));
    p.timeout_seconds = 10_000;
    assert!(matches!(AgentConfig::from_prefs(&p), Err(ConfigError::TimeoutOutOfRange(10_000))));
    p.timeout_seconds = 60;
    assert_eq!(AgentConfig::from_prefs(&p).map(|c| c.timeout_seconds()), Ok(60));
    // Zero means "use the default", not "no timeout".
    p.timeout_seconds = 0;
    assert_eq!(AgentConfig::from_prefs(&p).map(|c| c.timeout_seconds()), Ok(DEFAULT_AGENT_TIMEOUT_SECONDS));
}

#[test]
fn an_unknown_route_is_refused() {
    let mut p = prefs("http://x:1", "m");
    p.route = "responses".into();
    assert!(matches!(AgentConfig::from_prefs(&p), Err(ConfigError::UnknownRoute(_))));
    // A blank route takes the default.
    p.route = String::new();
    assert!(AgentConfig::from_prefs(&p).is_ok());
}

#[test]
fn the_credential_reference_is_redacted_in_debug() {
    // A log line must never say where the key is, and must never say what it is.
    let mut p = prefs("http://x:1", "m");
    p.credential = Some("MY_VERY_SECRET_ENV_VAR".into());
    let c = AgentConfig::from_prefs(&p).expect("valid");
    let printed = format!("{c:?}");
    assert!(!printed.contains("MY_VERY_SECRET_ENV_VAR"), "the credential name is redacted: {printed}");
    assert!(printed.contains("redacted"), "it says so: {printed}");
    // Preferences are redacted too, since they serialize to disk.
    let printed = format!("{p:?}");
    assert!(!printed.contains("MY_VERY_SECRET_ENV_VAR"), "prefs Debug redacts: {printed}");
    assert!(printed.contains("base_url"), "the useful fields are still there: {printed}");
}

#[test]
fn an_unknown_credential_name_fails_with_authentication() {
    struct Nothing;
    impl Credentials for Nothing {
        fn resolve(&self, _name: &str) -> Option<String> {
            None
        }
    }
    let mut p = prefs("http://x:1", "m");
    p.credential = Some("MISSING".into());
    let c = AgentConfig::from_prefs(&p).expect("valid config");
    let e = ModelGateway::build(&c, &Nothing).expect_err("no credential");
    assert_eq!(e.kind, AgentErrorKind::Authentication);
    assert!(e.message.contains("MISSING"), "the message names the variable: {}", e.message);
}

#[test]
fn an_endpoint_that_needs_no_credential_builds() {
    // A local vLLM often runs without one; refusing that would break the common setup.
    let c = AgentConfig::from_prefs(&prefs("http://127.0.0.1:8000", "Qwen3")).expect("valid");
    let g = ModelGateway::build(&c, &Environment).expect("builds without a credential");
    assert_eq!(g.model_name(), "Qwen3");
    assert_eq!(g.timeout_seconds(), DEFAULT_AGENT_TIMEOUT_SECONDS);
}

#[test]
fn a_configured_credential_builds_through_the_resolver() {
    // The workspace forbids `unsafe`, so the test never touches the process environment; a fixed
    // resolver exercises the same path a real one takes.
    struct Fixed;
    impl Credentials for Fixed {
        fn resolve(&self, name: &str) -> Option<String> {
            (name == "FIGCRAFT_AGENT_TEST_KEY").then(|| "test-value".to_string())
        }
    }
    let mut p = prefs("http://x:1", "m");
    p.credential = Some("FIGCRAFT_AGENT_TEST_KEY".into());
    let c = AgentConfig::from_prefs(&p).expect("valid");
    let g = ModelGateway::build(&c, &Fixed).expect("builds");
    let printed = format!("{g:?}");
    assert!(!printed.contains("test-value"), "the gateway never prints the key: {printed}");
    assert!(!printed.contains("FIGCRAFT_AGENT_TEST_KEY"), "nor where it came from: {printed}");
}

#[test]
fn a_server_alias_is_accepted_without_validation() {
    // A provider may answer under a different name (an alias, a quantised variant): the model name is
    // opaque, so nothing rejects it.
    let c = AgentConfig::from_prefs(&prefs("http://x:1", "DeepSeek-V4.1-Flash-1017")).expect("valid");
    let g = ModelGateway::build(&c, &Environment).expect("builds");
    g.accept_alias(Some("qwen3-235b-a22b-instruct-2507"));
    g.accept_alias(None);
    assert_eq!(g.model_name(), "DeepSeek-V4.1-Flash-1017", "the configured name is unchanged");
}

#[test]
fn http_failures_are_classified_by_what_a_person_can_do() {
    assert_eq!(classify_http(401, "").kind, AgentErrorKind::Authentication);
    assert_eq!(classify_http(403, "").kind, AgentErrorKind::Authentication);
    assert_eq!(classify_http(404, "").kind, AgentErrorKind::Protocol);
    assert_eq!(classify_http(408, "").kind, AgentErrorKind::Timeout);
    assert_eq!(classify_http(504, "").kind, AgentErrorKind::Timeout);
    assert_eq!(classify_http(429, "").kind, AgentErrorKind::Unreachable);
    assert_eq!(classify_http(500, "").kind, AgentErrorKind::Unreachable);
    assert_eq!(classify_http(503, "").kind, AgentErrorKind::Unreachable);
    assert_eq!(classify_http(400, "").kind, AgentErrorKind::Protocol);
    // Every kind says what to do.
    for status in [400, 401, 404, 408, 429, 500, 503] {
        assert!(!classify_http(status, "").message.is_empty());
    }
}

#[test]
fn a_provider_body_is_summarized_never_echoed() {
    // The body can contain the request; a log line must stay short.
    let long = "x".repeat(5000);
    let e = classify_http(500, &long);
    assert!(e.message.chars().count() < 400, "the body is summarized: {} chars", e.message.chars().count());
    // Only the first non-empty line is used.
    let e = classify_http(400, "\n\n  the real problem  \nmore detail");
    assert!(e.message.contains("the real problem"), "got {}", e.message);
    assert!(!e.message.contains("more detail"), "later lines are dropped: {}", e.message);
}

#[test]
fn the_runnable_kinds_say_whether_a_retry_could_help() {
    assert!(AgentErrorKind::Timeout.is_retryable());
    assert!(AgentErrorKind::Unreachable.is_retryable());
    // A configuration or authentication failure will not fix itself.
    assert!(!AgentErrorKind::Configuration.is_retryable());
    assert!(!AgentErrorKind::Authentication.is_retryable());
    assert!(!AgentErrorKind::Conflict.is_retryable());
    assert!(!AgentErrorKind::Cancelled.is_retryable());
}

#[test]
fn the_url_is_the_normalized_base_plus_the_route() {
    // Kept from the removed custom transport: it tests `AgentConfig`, not the wire, so it still holds.
    // Rig builds the request from `base_url()` + `route().path()`.
    let c = AgentConfig::from_prefs(&prefs("http://127.0.0.1:8000/v1/", "m")).expect("valid");
    assert_eq!(c.base_url(), "http://127.0.0.1:8000/v1");
    assert_eq!(format!("{}/{}", c.base_url(), c.route().path()), "http://127.0.0.1:8000/v1/chat/completions");
}
