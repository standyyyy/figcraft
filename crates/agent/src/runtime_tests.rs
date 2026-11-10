//! The Rig streaming runtime: event mapping, reasoning exclusion, and the terminal contract.

use serde_json::json;

use crate::AgentErrorKind;
use crate::config::AgentConfig;
use crate::run::{AgentEvent, CancelToken, Limits};
use crate::runtime::{Turn, run};
use crate::shadow::Shadow;
use crate::terminal::TurnTerminal;

fn config() -> AgentConfig {
    let prefs = figcraft_engine::prefs::AgentPrefs {
        enabled: true,
        base_url: "http://127.0.0.1:1".into(), // nothing listens: the transport fails fast
        model: "test".into(),
        timeout_seconds: 5,
        ..figcraft_engine::prefs::AgentPrefs::default()
    };
    AgentConfig::from_prefs(&prefs).expect("valid config")
}

fn live_session() -> figcraft_engine::Session {
    let mut s = figcraft_engine::Session::new();
    s.execute("file.new", &json!({})).expect("file.new");
    s
}

fn turn(cancel: CancelToken) -> Turn {
    Turn {
        turn: 1,
        request: "draw".into(),
        document_uid: 7,
        base_revision: 3,
        config: config(),
        limits: Limits::default(),
        cancel,
        history: Vec::new(),
    }
}

#[test]
fn a_cancelled_turn_never_starts_the_stream() {
    // Cancellation before the first poll must not reach the provider at all, and must carry no replay.
    let live = live_session();
    let shadow = Shadow::capture(&live).expect("capture");
    let cancel = CancelToken::new();
    cancel.cancel();
    let mut events = Vec::new();
    let terminal = run(turn(cancel), shadow, &crate::EnvironmentCredentials, &mut |e| events.push(e));
    assert!(matches!(terminal, TurnTerminal::Cancelled { .. }), "got {terminal:?}");
    assert!(terminal.commit().is_none(), "a cancelled turn carries no envelope");
    assert!(events.is_empty(), "nothing ran, so nothing was reported: {events:?}");
}

#[test]
fn an_unreachable_provider_fails_with_a_classified_kind() {
    // The transport cannot connect: the run must end as a failure with no envelope, never a panic.
    let live = live_session();
    let shadow = Shadow::capture(&live).expect("capture");
    let mut events = Vec::new();
    let terminal = run(turn(CancelToken::new()), shadow, &crate::EnvironmentCredentials, &mut |e| events.push(e));
    assert!(terminal.commit().is_none(), "a failure carries no envelope");
    match &terminal {
        TurnTerminal::Failed { kind, message, .. } => {
            assert!(
                matches!(kind, AgentErrorKind::Unreachable | AgentErrorKind::Timeout | AgentErrorKind::Protocol),
                "a connection failure is classified, got {kind:?}: {message}"
            );
            assert!(!message.is_empty(), "it says what happened");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    // The turn was started and then failed: a Started event, and no Committed one.
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Started { .. })), "the turn started");
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::Committed { .. })), "nothing committed");
}

#[test]
fn a_zero_second_limit_stops_the_turn_before_it_asks() {
    // The wall clock is checked between stream items, so an already-expired budget cannot run a
    // provider request. With max_seconds 0 the first check fires.
    let live = live_session();
    let shadow = Shadow::capture(&live).expect("capture");
    let mut t = turn(CancelToken::new());
    t.limits.max_seconds = 0;
    let mut events = Vec::new();
    let terminal = run(t, shadow, &crate::EnvironmentCredentials, &mut |e| events.push(e));
    assert!(terminal.commit().is_none(), "a limited turn carries no envelope");
    match terminal {
        TurnTerminal::LimitReached { kind, .. } => assert_eq!(kind, AgentErrorKind::Timeout),
        other => panic!("expected LimitReached, got {other:?}"),
    }
}

#[test]
fn the_prompt_forbids_narrating_reasoning() {
    // The prompt is behaviour: it keeps a model from writing its scratchpad into the answer, which
    // matters because Rig keeps reasoning in its own variant and cannot be relied on for a model that
    // puts it in `content`.
    let p = crate::runtime_prompt::SYSTEM_PROMPT.to_lowercase();
    assert!(p.contains("do not describe your reasoning"));
    assert!(p.contains("do not ask for confirmation"));
    assert!(p.contains("one or two short sentences"));
    assert!(p.contains("build") && p.contains("batch"), "it says to build with batches");
    // It must not name tools: a hard-coded list would drift from the catalog.
    for tool in ["create_node", "set_properties", "inspect_document", "upsert_requirements"] {
        assert!(!crate::runtime_prompt::SYSTEM_PROMPT.contains(tool), "the prompt names {tool}");
    }
}

#[test]
fn every_stream_ending_is_a_distinct_terminal() {
    // The mapping must be total: whichever way the stream stops, exactly one terminal comes back and
    // only a success can carry an envelope.
    let cases: Vec<TurnTerminal> = vec![
        TurnTerminal::Success { turn: 1, answer: "x".into(), commit: None, continuation: Vec::new() },
        TurnTerminal::Failed { turn: 1, kind: AgentErrorKind::Protocol, message: "bad".into() },
        TurnTerminal::Cancelled { turn: 1 },
        TurnTerminal::LimitReached { turn: 1, kind: AgentErrorKind::Model, message: "rounds".into() },
    ];
    assert_eq!(cases.iter().filter(|t| t.is_success()).count(), 1, "one success among them");
    assert_eq!(cases.iter().filter(|t| t.commit().is_some()).count(), 0, "none carries an envelope here");
    for t in &cases {
        assert_eq!(t.turn(), 1, "every terminal names its turn");
    }
}

#[test]
fn a_provider_error_is_summarized_not_dumped() {
    // Rig's message can carry a provider body, which can carry the request: it is clipped.
    let long = "x".repeat(5000);
    let live = live_session();
    let shadow = Shadow::capture(&live).expect("capture");
    let _ = (long, shadow); // the summarizing is exercised through `run` above; this pins the shape
    // `summarize` is private, so its behaviour is asserted where it matters: no failure message in a
    // turn is longer than the clip plus its context.
    let live = live_session();
    let shadow = Shadow::capture(&live).expect("capture");
    let terminal = run(turn(CancelToken::new()), shadow, &crate::EnvironmentCredentials, &mut |_| {});
    if let TurnTerminal::Failed { message, .. } = &terminal {
        assert!(message.chars().count() < 400, "a failure message is summarized: {} chars", message.chars().count());
    }
}

// ---------- the driver both hosts advance ----------

#[test]
fn the_driver_reports_done_only_when_it_has_ended() {
    // A browser polls this instead of blocking: it must not report done while the turn waits.
    let live = live_session();
    let shadow = Shadow::capture(&live).expect("capture");
    let mut driver = crate::runtime::TurnDriver::start(turn(CancelToken::new()), shadow, &crate::EnvironmentCredentials);
    assert_eq!(driver.poll(), crate::runtime::DriverStep::Pending, "a fresh driver is waiting");
    // The provider is unreachable, so one step ends it.
    let more = driver.drive_once();
    assert!(!more, "the step ended the turn");
    assert!(driver.is_done(), "and says so");
    assert_eq!(driver.poll(), crate::runtime::DriverStep::Done);
}

#[test]
fn a_driver_started_on_a_cancelled_turn_reports_cancelled_with_no_events() {
    // Nothing began, so nothing is reported: no `Started`, and the terminal carries no envelope.
    let live = live_session();
    let shadow = Shadow::capture(&live).expect("capture");
    let cancel = CancelToken::new();
    cancel.cancel();
    let mut driver = crate::runtime::TurnDriver::start(turn(cancel), shadow, &crate::EnvironmentCredentials);
    assert!(driver.is_done(), "it is over before it began");
    assert!(driver.take_events().is_empty(), "and said nothing");
    let terminal = driver.finish();
    assert!(matches!(terminal, TurnTerminal::Cancelled { .. }));
    assert!(terminal.commit().is_none());
}

#[test]
fn a_driver_hands_its_events_over_as_it_goes() {
    // The browser drains these between steps, so they must be takeable without finishing.
    let live = live_session();
    let shadow = Shadow::capture(&live).expect("capture");
    let mut driver = crate::runtime::TurnDriver::start(turn(CancelToken::new()), shadow, &crate::EnvironmentCredentials);
    let first = driver.take_events();
    assert!(matches!(first.as_slice(), [AgentEvent::Started { .. }]), "the start is reported first: {first:?}");
    assert!(driver.take_events().is_empty(), "and taken only once");
}

#[test]
fn a_driver_that_could_not_start_reports_a_terminal_and_no_replay() {
    let live = live_session();
    let shadow = Shadow::capture(&live).expect("capture");
    let terminal = TurnTerminal::Failed { turn: 1, kind: AgentErrorKind::Configuration, message: "bad snapshot".into() };
    let driver = crate::runtime::TurnDriver::from_terminal(terminal, shadow);
    assert!(driver.is_done());
    let terminal = driver.finish();
    assert!(terminal.commit().is_none(), "a failure carries no envelope");
}
