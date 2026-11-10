#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! The live acceptance test against a reachable OpenAI-compatible endpoint, through Rig's streaming
//! runtime -- the same path the app uses, not a side door.
//!
//! Opt-in, so the deterministic suite never touches the network:
//!
//! ```text
//! FIGCRAFT_AI_BASE_URL=http://127.0.0.1:8111/v1 \
//! FIGCRAFT_AI_MODEL=DeepSeek-V4.1-Flash-1017 \
//! cargo test -p figcraft-agent --test live_vllm -- --ignored --nocapture
//! ```
//!
//! A credential, when the endpoint needs one, is named by `FIGCRAFT_AI_API_KEY_NAME` and read from
//! that variable: no key is committed, and none is printed.
//!
//! Without the variables the test says it skipped and returns, so a stray `--ignored` is harmless.

use figcraft_agent::AgentConfig;
use figcraft_agent::TurnTerminal;
use figcraft_agent::run::{AgentEvent, CancelToken, Limits};
use figcraft_agent::runtime::{Turn, run};
use figcraft_agent::shadow::Shadow;
use figcraft_agent::{SESSION_REFERENCE, SessionCredentials};
use serde_json::json;

/// The endpoint and model from the environment, or `None` when not configured.
fn live_config() -> Option<(AgentConfig, Option<String>)> {
    let base = std::env::var("FIGCRAFT_AI_BASE_URL").ok().filter(|v| !v.trim().is_empty())?;
    let model = std::env::var("FIGCRAFT_AI_MODEL").ok().filter(|v| !v.trim().is_empty())?;
    let key_name = std::env::var("FIGCRAFT_AI_API_KEY_NAME").ok().filter(|v| !v.trim().is_empty());
    let credential = key_name.as_deref().and_then(|n| std::env::var(n).ok()).filter(|v| !v.is_empty());
    let prefs = figcraft_engine::prefs::AgentPrefs {
        enabled: true,
        base_url: base,
        model,
        // The key is held in memory for this process, so the config names the session reference
        // rather than a variable -- the same way a browser build works.
        credential: credential.as_ref().map(|_| SESSION_REFERENCE.to_string()),
        timeout_seconds: 120,
        ..figcraft_engine::prefs::AgentPrefs::default()
    };
    let config = AgentConfig::from_prefs(&prefs).ok()?;
    Some((config, credential))
}

/// A shadow of `session`'s document, as the app builds one for each turn.
fn shadow_of(session: &figcraft_engine::Session) -> Shadow {
    Shadow::capture(session).expect("capture")
}

/// A credential holder for a live endpoint: the key from the environment, or none.
///
/// Uses the same contract the app does, so a live run exercises the path a real turn takes.
fn live_credentials(key: &Option<String>) -> SessionCredentials {
    let mut credentials = SessionCredentials::default();
    if let Some(k) = key {
        credentials.set(k.clone());
    }
    credentials
}

/// A disposable synthetic document, as a shadow the turn edits.
fn shadow() -> Shadow {
    let mut s = figcraft_engine::Session::new();
    s.execute("file.new", &json!({})).expect("file.new");
    Shadow::capture(&s).expect("capture")
}

fn turn(config: AgentConfig, request: &str, cancel: CancelToken) -> Turn {
    Turn { turn: 1, request: request.into(), document_uid: 1, base_revision: 1, config, limits: Limits::default(), cancel, history: Vec::new() }
}

#[test]
#[ignore = "needs FIGCRAFT_AI_BASE_URL and FIGCRAFT_AI_MODEL; set them to run against a real endpoint"]
fn a_streamed_answer_arrives_through_rig() {
    let Some((config, credential)) = live_config() else {
        eprintln!("skipped: set FIGCRAFT_AI_BASE_URL and FIGCRAFT_AI_MODEL to run this");
        return;
    };
    let mut events = Vec::new();
    let terminal =
        run(turn(config.clone(), "Reply with exactly: OK", CancelToken::new()), shadow(), &live_credentials(&credential), &mut |e| events.push(e));

    // Rig streams: the answer must arrive as one or more deltas, not only in the final response.
    let deltas: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(!deltas.is_empty(), "no streamed text arrived; events were {events:?}");
    println!("live: {} text deltas, first {:?}", deltas.len(), deltas.first());

    match terminal {
        TurnTerminal::Success { answer, .. } => {
            println!("live: answer {:?}", answer.chars().take(80).collect::<String>());
            assert!(!answer.is_empty(), "a success has an answer");
        }
        other => panic!("expected a success, got {other:?}"),
    }
}

#[test]
#[ignore = "needs FIGCRAFT_AI_BASE_URL and FIGCRAFT_AI_MODEL; set them to run against a real endpoint"]
fn a_model_calls_the_canvas_tool_and_the_edit_replays() {
    let Some((config, credential)) = live_config() else {
        eprintln!("skipped: set FIGCRAFT_AI_BASE_URL and FIGCRAFT_AI_MODEL to run this");
        return;
    };
    let mut events = Vec::new();
    let terminal = run(
        turn(config, "Create a rectangle 100 wide and 60 tall at 10,10 in the design.", CancelToken::new()),
        shadow(),
        &live_credentials(&credential),
        &mut |e| events.push(e),
    );

    let tools: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolStarted { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    println!("live: tools started: {tools:?}");
    assert!(!tools.is_empty(), "the model called no tool; events were {events:?}");

    match terminal {
        TurnTerminal::Success { commit, .. } => {
            let envelope = commit.expect("a turn that created a layer carries a replay");
            let commands: Vec<&str> = envelope.replay.iter().map(|s| s.command.as_str()).collect();
            println!("live: replay commands {commands:?}");
            assert!(
                envelope.replay.iter().any(|s| s.command == "node.create" || s.command == "command.batch"),
                "the replay holds the create: {commands:?}"
            );
        }
        other => panic!("expected a success with a replay, got {other:?}"),
    }
}

#[test]
#[ignore = "needs FIGCRAFT_AI_BASE_URL and FIGCRAFT_AI_MODEL; set them to run against a real endpoint"]
fn cancelling_mid_turn_commits_nothing() {
    let Some((config, credential)) = live_config() else {
        eprintln!("skipped: set FIGCRAFT_AI_BASE_URL and FIGCRAFT_AI_MODEL to run this");
        return;
    };
    // Cancelled before the first poll: the strongest form of "no late result may commit".
    let cancel = CancelToken::new();
    cancel.cancel();
    let terminal = run(turn(config, "Create a very large layout with forty layers.", cancel), shadow(), &live_credentials(&credential), &mut |_| {});
    assert!(terminal.commit().is_none(), "a cancelled turn carries no envelope");
    assert!(matches!(terminal, TurnTerminal::Cancelled { .. }), "got {terminal:?}");
    println!("live: cancellation committed nothing");
}

#[test]
#[ignore = "needs FIGCRAFT_AI_BASE_URL and FIGCRAFT_AI_MODEL; set them to run against a real endpoint"]
fn a_follow_up_turn_continues_the_previous_one() {
    let Some((config, credential)) = live_config() else {
        eprintln!("skipped: set FIGCRAFT_AI_BASE_URL and FIGCRAFT_AI_MODEL to run this");
        return;
    };
    // One document across both turns: the second turn's shadow is what the first turn produced, which
    // is what the app does (each turn snapshots the live document). A fresh empty shadow would make
    // the history and the document contradict each other, and the model would answer about the empty
    // one -- correct behaviour, wrong test.
    let mut session = figcraft_engine::Session::new();
    session.execute("file.new", &json!({})).expect("file.new");

    let mut first_events = Vec::new();
    let first = run(
        turn(config.clone(), "Create a rectangle 20 wide and 20 tall at 0,0.", CancelToken::new()),
        shadow_of(&session),
        &live_credentials(&credential),
        &mut |e| first_events.push(e),
    );
    let (continuation, replay) = match &first {
        TurnTerminal::Success { continuation, commit, .. } => (continuation.clone(), commit.as_ref().map(|c| c.batch_commands()).unwrap_or_default()),
        other => panic!("the first turn must succeed, got {other:?}"),
    };
    println!("live: first turn produced {} continuation messages", continuation.len());
    assert!(!continuation.is_empty(), "a finished run returns its transcript for the next turn");

    // Apply the first turn's edits, so the document holds what the history says it holds.
    if !replay.is_empty() {
        session.execute("command.batch", &json!({"commands": replay})).expect("apply the first turn");
    }
    let layers = session.doc().expect("doc").nodes().count();
    println!("live: the document now holds {layers} layers");

    // The follow-up gets those messages as history and a shadow of the document they describe.
    let mut t = turn(config, "How many layers did you create? Answer with the number only.", CancelToken::new());
    t.history = continuation.clone();
    let mut second_events = Vec::new();
    let second = run(t, shadow_of(&session), &live_credentials(&credential), &mut |e| second_events.push(e));
    match &second {
        TurnTerminal::Success { answer, .. } => {
            let shown: String = answer.chars().take(120).collect();
            println!("live: follow-up answered {shown:?}");
            assert!(!answer.is_empty(), "the follow-up answered");
        }
        other => panic!("the follow-up must succeed, got {other:?}"),
    }
    println!("live: the continuation was accepted by Rig and the follow-up completed");
}

#[test]
#[ignore = "needs FIGCRAFT_AI_BASE_URL and FIGCRAFT_AI_MODEL; set them to run against a real endpoint"]
fn a_stored_continuation_round_trips_through_json() {
    let Some((config, credential)) = live_config() else {
        eprintln!("skipped: set FIGCRAFT_AI_BASE_URL and FIGCRAFT_AI_MODEL to run this");
        return;
    };
    let mut events = Vec::new();
    let first = run(turn(config.clone(), "Say exactly: OK", CancelToken::new()), shadow(), &live_credentials(&credential), &mut |e| events.push(e));
    let continuation = match &first {
        TurnTerminal::Success { continuation, .. } => continuation.clone(),
        other => panic!("expected a success, got {other:?}"),
    };
    // The stored form is plain JSON: serialize it fully, read it back, and convert to Rig messages.
    let as_text = serde_json::to_string(&continuation).expect("the continuation is plain JSON");
    let reparsed: Vec<serde_json::Value> = serde_json::from_str(&as_text).expect("and parses back");
    assert_eq!(reparsed.len(), continuation.len(), "every message survived the round trip");
    let messages = figcraft_agent::history::continuation_messages(&reparsed);
    assert_eq!(messages.len(), reparsed.len(), "and every message is valid Rig history");
    println!("live: {} messages round-tripped through JSON and back into Rig", messages.len());
}
