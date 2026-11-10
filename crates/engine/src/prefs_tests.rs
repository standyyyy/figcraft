//! Preferences: the Agent provider settings are plain data, never serialized into a document, and
//! never print a credential.

use crate::prefs::*;
use crate::Document;

#[test]
fn agent_preferences_default_to_off_with_no_endpoint() {
    let p = AgentPrefs::default();
    assert!(!p.enabled, "the Agent is opt-in");
    assert!(p.base_url.is_empty());
    assert!(p.model.is_empty());
    assert_eq!(p.credential, None);
    assert_eq!(p.timeout_seconds, 0, "zero means `use the default`");
    assert!(p.is_default());
}

#[test]
fn default_agent_preferences_are_not_written() {
    // A preferences file from before this field existed stays byte-identical.
    let p = Prefs::default();
    let json = serde_json::to_string(&p).expect("serialize");
    assert!(!json.contains("\"agent\""), "the default Agent block is not written: {json}");
}

#[test]
fn configured_agent_preferences_round_trip() {
    let p = Prefs {
        agent: AgentPrefs {
            enabled: true,
            base_url: "http://127.0.0.1:8000/v1".into(),
            model: "DeepSeek-V4.1-Flash-1017".into(),
            credential: Some("FIGCRAFT_AI_KEY".into()),
            route: DEFAULT_AGENT_ROUTE.into(),
            timeout_seconds: 120,
        },
        ..Prefs::default()
    };
    let json = serde_json::to_string(&p).expect("serialize");
    assert!(json.contains("\"agent\""), "a configured block is written: {json}");
    // camelCase on the wire, as the rest of the preferences file uses.
    assert!(json.contains("baseUrl"), "camelCase: {json}");
    assert!(json.contains("timeoutSeconds"), "camelCase: {json}");
    let back: Prefs = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.agent, p.agent);
}

#[test]
fn a_preferences_file_without_agent_settings_still_reads() {
    let json = r#"{"nudge_small": 2.0}"#;
    let p: Prefs = serde_json::from_str(json).expect("an older file reads");
    assert_eq!(p.nudge_small, 2.0);
    assert_eq!(p.agent, AgentPrefs::default(), "a missing block takes the defaults");
}

#[test]
fn the_agent_credential_is_redacted_in_debug_output() {
    // A log line or a panic message must not say where the key is stored.
    let p = AgentPrefs {
        enabled: true,
        base_url: "http://x:1/v1".into(),
        model: "m".into(),
        credential: Some("FIGCRAFT_AI_KEY".into()),
        ..AgentPrefs::default()
    };
    let printed = format!("{p:?}");
    assert!(!printed.contains("FIGCRAFT_AI_KEY"), "the credential reference is redacted: {printed}");
    assert!(printed.contains("redacted"), "it says so: {printed}");
    assert!(printed.contains("base_url"), "the useful fields remain: {printed}");
    // And it is not written to a preferences file either: only the variable's name is.
    let printed = format!("{p:?}");
    assert!(!printed.contains("sk-"), "no key-shaped text: {printed}");
}

#[test]
fn agent_preferences_are_not_document_data() {
    // The engine owns only serializable preference data. Requirements live in `Document`;
    // provider settings and transcripts must never appear there, or a shared `.figcraft` would
    // leak the endpoint and the chat.
    let doc = Document::new("f");
    let json = serde_json::to_string(&doc).expect("serialize a document");
    assert!(!json.contains("agent"), "a document carries no agent settings: {json}");
    assert!(!json.contains("credential"), "nor a credential: {json}");
    assert!(!json.contains("conversations"), "nor a transcript: {json}");
}

#[test]
fn the_engine_does_not_depend_on_the_agent_crate() {
    // Pinned by the layering check too, but stated here where the preference lives: the engine owns
    // the data, the L6 crate owns the behaviour.
    let manifest = include_str!("../Cargo.toml");
    assert!(!manifest.contains("figcraft-agent"), "figcraft-engine must not depend on figcraft-agent");
}

#[test]
fn the_timeout_bounds_are_sane() {
    let (min, default, max) = (AGENT_TIMEOUT_MIN, DEFAULT_AGENT_TIMEOUT_SECONDS, AGENT_TIMEOUT_MAX);
    assert!(min < default, "the default sits inside the accepted range");
    assert!(default < max);
    assert_eq!((min, max), (5, 600), "the accepted range is what the UI offers");
}
