//! The browser's session key: held in memory, never written, never printed.
//!
//! The contract the plan asks for: a key entered in Preferences reaches the turn and the connection
//! test, and reaches nothing else -- not preferences, not the transcript, not a document, not a log,
//! not a debug listing. A reload keeps the endpoint and model and asks for the key again.

use figcraft_agent::Credentials;

use crate::agent_credentials::AgentCredentials;

#[test]
fn a_new_store_holds_no_key() {
    let credentials = AgentCredentials::default();
    assert!(!credentials.is_set());
    assert!(credentials.resolver().resolve(Some(figcraft_agent::SESSION_REFERENCE)).expect("no error").is_none());
}

#[test]
fn setting_a_key_makes_it_resolvable_for_the_session_reference() {
    // The turn and the connection test both resolve through this, so a key that reaches the store
    // reaches both.
    let mut credentials = AgentCredentials::default();
    credentials.set_session_key("sk-browser-test-key");
    assert!(credentials.is_set());

    let resolved = credentials.resolver().resolve(Some(figcraft_agent::SESSION_REFERENCE)).expect("resolves").expect("a secret");
    assert_eq!(resolved.expose(), "sk-browser-test-key");
}

#[test]
fn clearing_the_key_makes_it_unresolvable() {
    // Emptying the field must make an authenticated turn fail before any network I/O, not send an
    // empty key.
    let mut credentials = AgentCredentials::default();
    credentials.set_session_key("sk-browser-test-key");
    credentials.clear_session_key();
    assert!(!credentials.is_set());
    assert!(credentials.resolver().resolve(Some(figcraft_agent::SESSION_REFERENCE)).expect("no error").is_none());
}

#[test]
fn a_reload_starts_with_no_key() {
    // Preferences keep the endpoint and model; the key is app memory and does not survive.
    let mut before = AgentCredentials::default();
    before.set_session_key("sk-browser-test-key");
    assert!(before.is_set());

    let after = AgentCredentials::default();
    assert!(!after.is_set(), "a fresh store holds nothing");
    assert!(after.resolver().resolve(Some(figcraft_agent::SESSION_REFERENCE)).expect("no error").is_none());
}

#[test]
fn a_key_never_appears_in_debug_output() {
    // A debug listing ends up in a log, a panic message or a bug report.
    let mut credentials = AgentCredentials::default();
    credentials.set_session_key("sk-must-not-appear-anywhere");
    let debug = format!("{credentials:?}");
    assert!(!debug.contains("sk-must-not-appear-anywhere"), "the key leaked into debug: {debug}");
    assert!(!debug.contains("must-not-appear"), "part of the key leaked: {debug}");
}

#[test]
fn the_input_buffer_never_appears_in_debug_output_either() {
    // The masked field's buffer is secret state too: it holds what the person is typing.
    let mut credentials = AgentCredentials::default();
    credentials.set_draft("sk-being-typed-right-now");
    let debug = format!("{credentials:?}");
    assert!(!debug.contains("sk-being-typed-right-now"), "the draft leaked into debug: {debug}");
    assert!(credentials.draft().is_some(), "the draft is still there for the field to show masked");
}

#[test]
fn an_unset_reference_resolves_to_no_key_rather_than_an_error() {
    // An endpoint that needs no key is a normal setup, so no reference is not a failure.
    let credentials = AgentCredentials::default();
    assert!(credentials.resolver().resolve(None).expect("no error").is_none());
    assert!(credentials.resolver().resolve(Some("   ")).expect("no error").is_none());
}

#[test]
fn a_reference_this_platform_cannot_read_is_refused() {
    // The desktop's variable-name reference has no meaning in a browser; refusing it is better than
    // silently sending no key to an endpoint that needs one.
    let credentials = AgentCredentials::default();
    let error = credentials.resolver().resolve(Some("MY_ENV_VAR")).expect_err("refused");
    assert_eq!(error.kind, figcraft_agent::AgentErrorKind::Configuration);
}

#[test]
fn a_committed_draft_reaches_the_store_and_not_the_persisted_reference() {
    // The only path from what someone typed to what a request uses. What is persisted is the reserved
    // reference, never the key: preferences are written as text.
    let mut credentials = AgentCredentials::default();
    credentials.set_draft("sk-typed-into-the-field");
    credentials.commit_draft();

    assert!(credentials.is_set(), "the key reached the store");
    assert!(credentials.draft().is_none(), "and the buffer is cleared");

    // What a preference may hold.
    let reference = figcraft_agent::SESSION_REFERENCE;
    assert_eq!(reference, "browser-session", "a name, not a key");
    assert!(!reference.contains("sk-"), "the reference cannot be mistaken for a key");

    let resolved = credentials.resolver().resolve(Some(reference)).expect("resolves").expect("a secret");
    assert_eq!(resolved.expose(), "sk-typed-into-the-field");
}

#[test]
fn an_empty_draft_clears_the_key_rather_than_leaving_a_stale_one() {
    // Committing an empty field must mean "no key", not "keep the old one".
    let mut credentials = AgentCredentials::default();
    credentials.set_session_key("sk-old-key");
    credentials.set_draft("   ");
    credentials.commit_draft();
    assert!(!credentials.is_set(), "an empty draft clears the key");
}

#[test]
fn a_preference_holding_the_key_instead_of_a_reference_cannot_be_resolved_as_a_key() {
    // If a document or preferences file ever held a key where a reference belongs, the resolver refuses
    // it rather than treating the key as a variable name to look up.
    let credentials = AgentCredentials::default();
    let error = credentials.resolver().resolve(Some("sk-totally-a-key-not-a-reference")).expect_err("refused");
    assert_eq!(error.kind, figcraft_agent::AgentErrorKind::Configuration);
    assert!(!error.message.contains("sk-totally-a-key"), "the refusal must not echo it: {}", error.message);
}
