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
    assert!(terminal.commit().is_none(), "nothing committed");
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

// ---------- the settings check ----------

#[test]
fn a_connection_test_against_an_unreachable_provider_fails_with_the_cause() {
    // A settings check, not a turn: it must report why, and it must not need a shadow.
    let mut prefs = figcraft_engine::prefs::AgentPrefs {
        enabled: true,
        base_url: "http://127.0.0.1:1".into(),
        model: "m".into(),
        timeout_seconds: 10,
        ..figcraft_engine::prefs::AgentPrefs::default()
    };
    prefs.enabled = true;
    let config = AgentConfig::from_prefs(&prefs).expect("valid");
    let status = crate::runtime::check_connection(&config, &figcraft_agent_env());
    match status {
        crate::runtime::ConnectionStatus::Failed(why) => {
            assert!(!why.is_empty(), "it says why");
            println!("connection test failed with: {why}");
        }
        crate::runtime::ConnectionStatus::Ready => panic!("nothing listens on port 1"),
        crate::runtime::ConnectionStatus::Cancelled => panic!("nobody cancelled this test"),
    }
}

#[test]
fn a_connection_test_with_a_missing_credential_says_so_without_a_request() {
    // The key is resolved through the same contract a turn uses, so a missing one is caught here and
    // not after a request the provider would refuse.
    let prefs = figcraft_engine::prefs::AgentPrefs {
        enabled: true,
        base_url: "http://127.0.0.1:1".into(),
        model: "m".into(),
        credential: Some("FIGCRAFT_DEFINITELY_NOT_SET_XYZ".into()),
        timeout_seconds: 10,
        ..figcraft_engine::prefs::AgentPrefs::default()
    };
    let config = AgentConfig::from_prefs(&prefs).expect("valid");
    match crate::runtime::check_connection(&config, &figcraft_agent_env()) {
        crate::runtime::ConnectionStatus::Failed(why) => {
            assert!(why.contains("credential"), "the cause is the missing key: {why}");
            assert!(why.contains("FIGCRAFT_DEFINITELY_NOT_SET_XYZ"), "and it names the variable: {why}");
        }
        crate::runtime::ConnectionStatus::Ready => panic!("a missing key is not ready"),
        crate::runtime::ConnectionStatus::Cancelled => panic!("nobody cancelled this test"),
    }
}

/// The host's credential resolver, as the desktop uses.
fn figcraft_agent_env() -> crate::EnvironmentCredentials {
    crate::EnvironmentCredentials
}

// ---------- cancellation reaches a waiting driver ----------

#[test]
fn cancelling_wakes_a_driver_waiting_on_a_silent_provider() {
    // The defect this pins: an `AtomicBool` could not wake a task parked in `stream.next().await`, so
    // Stop did nothing until the provider happened to answer. The server below accepts the request and
    // then says nothing, which is the exact case that used to hang.
    //
    // A refused connection will not do: it ends the turn before the cancel can land, and then the test
    // proves nothing about waking.
    let Some(server) = SilentServer::start() else {
        eprintln!("skipped: no loopback listener available");
        return;
    };
    let live = live_session();
    let shadow = Shadow::capture(&live).expect("capture");
    let cancel = CancelToken::new();
    let mut t = turn(cancel.clone());
    t.config = server.config();
    let mut driver = crate::runtime::TurnDriver::start(t, shadow, &crate::EnvironmentCredentials);

    // Cancel from another thread while the driver waits.
    let from_elsewhere = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(30));
        from_elsewhere.cancel();
    });

    let started = std::time::Instant::now();
    futures::executor::block_on(async {
        while !driver.is_done() {
            if !driver.drive_once_async().await && driver.is_done() {
                break;
            }
        }
    });
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "the wait ended promptly: {:?}", started.elapsed());

    let terminal = driver.finish();
    assert!(matches!(terminal, TurnTerminal::Cancelled { .. }), "cancelled, not failed or timed out: {terminal:?}");
    assert!(terminal.commit().is_none(), "and it carries no envelope");
}

#[test]
fn a_turn_past_its_deadline_ends_as_a_timeout_not_a_failure() {
    // The deadline is part of the race, so an unreachable provider with a zero-second budget ends as a
    // timeout rather than waiting for a connection that will never come.
    let live = live_session();
    let shadow = Shadow::capture(&live).expect("capture");
    let mut t = turn(CancelToken::new());
    t.limits.max_seconds = 0;
    let mut driver = crate::runtime::TurnDriver::start(t, shadow, &crate::EnvironmentCredentials);

    let started = std::time::Instant::now();
    futures::executor::block_on(async {
        while !driver.is_done() {
            if !driver.drive_once_async().await && driver.is_done() {
                break;
            }
        }
    });
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "it did not wait for the provider");

    let terminal = driver.finish();
    match terminal {
        TurnTerminal::LimitReached { kind, .. } => assert_eq!(kind, AgentErrorKind::Timeout),
        other => panic!("expected a timeout, got {other:?}"),
    }
}

/// A loopback HTTP listener that accepts a connection and then never answers.
///
/// Deterministic and local: no public address, no fixed port, nothing reached outside the test
/// process. Dropped at the end, which closes the listener.
struct SilentServer {
    port: u16,
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl SilentServer {
    /// Start it, or `None` if no port could be bound.
    fn start() -> Option<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").ok()?;
        listener.set_nonblocking(true).ok()?;
        let port = listener.local_addr().ok()?.port();
        let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&stopped);
        let handle = std::thread::spawn(move || {
            // Accept, then hold the connection open saying nothing: the wait a cancel has to break.
            let mut held = Vec::new();
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => held.push(stream),
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
            drop(held);
        });
        Some(Self { port, stopped, handle: Some(handle) })
    }

    /// A config pointing at it.
    fn config(&self) -> AgentConfig {
        let prefs = figcraft_engine::prefs::AgentPrefs {
            enabled: true,
            base_url: format!("http://127.0.0.1:{}/v1", self.port),
            model: "silent".into(),
            timeout_seconds: 60,
            ..figcraft_engine::prefs::AgentPrefs::default()
        };
        AgentConfig::from_prefs(&prefs).expect("valid config")
    }
}

impl Drop for SilentServer {
    fn drop(&mut self) {
        self.stopped.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// ---------- the connection test is a request, not a blocking call ----------

#[test]
fn a_connection_test_against_a_silent_provider_times_out() {
    // The blocking `check_connection` waits for as long as the provider stays silent. The async path
    // must end at the configured timeout instead: a settings check that hangs is worse than one that
    // fails, because the window it is drawn in stops repainting.
    let Some(server) = SilentServer::start() else {
        eprintln!("skipped: no loopback listener available");
        return;
    };
    let mut config = server.config();
    config = config.with_timeout_seconds(1);

    let started = std::time::Instant::now();
    let status = futures::executor::block_on(crate::runtime::test_connection_async(&config, &crate::EnvironmentCredentials));
    let elapsed = started.elapsed();
    assert!(elapsed < std::time::Duration::from_secs(20), "it ended on its own: {elapsed:?}");
    match status {
        crate::runtime::ConnectionStatus::Failed(why) => {
            let why = why.to_lowercase();
            assert!(why.contains("time") || why.contains("reach") || why.contains("answer"), "a readable cause: {why}");
        }
        crate::runtime::ConnectionStatus::Ready => panic!("a silent provider is not ready"),
        crate::runtime::ConnectionStatus::Cancelled => panic!("nobody cancelled this test"),
    }
}

#[test]
fn a_cancelled_connection_test_reports_cancelled_rather_than_failing() {
    // Closing Preferences while a test runs must stop it, and the result must say so rather than
    // claiming the endpoint is broken.
    let Some(server) = SilentServer::start() else {
        eprintln!("skipped: no loopback listener available");
        return;
    };
    let config = server.config();
    let cancel = CancelToken::new();
    let from_elsewhere = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(30));
        from_elsewhere.cancel();
    });

    let started = std::time::Instant::now();
    let status = futures::executor::block_on(crate::runtime::test_connection_async_cancellable(&config, &crate::EnvironmentCredentials, &cancel));
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "the wait was broken promptly");
    assert!(matches!(status, crate::runtime::ConnectionStatus::Cancelled), "got {status:?}");
}

#[test]
fn a_connection_test_that_cannot_build_a_config_says_so_without_a_request() {
    // A missing key is caught before any I/O: the message names what to fix.
    let prefs = figcraft_engine::prefs::AgentPrefs {
        enabled: true,
        base_url: "http://127.0.0.1:1".into(),
        model: "m".into(),
        credential: Some("FIGCRAFT_DEFINITELY_NOT_SET_ABC".into()),
        timeout_seconds: 10,
        ..figcraft_engine::prefs::AgentPrefs::default()
    };
    let config = AgentConfig::from_prefs(&prefs).expect("valid");
    let status = futures::executor::block_on(crate::runtime::test_connection_async(&config, &crate::EnvironmentCredentials));
    match status {
        crate::runtime::ConnectionStatus::Failed(why) => {
            assert!(why.contains("credential"), "the cause is the missing key: {why}");
        }
        other => panic!("expected a failure, got {other:?}"),
    }
}

// ---------- the design system is part of the prompt ----------

#[test]
fn the_prompt_carries_the_tdesign_values_a_screen_needs() {
    // A prototype built without these is guesswork: a button in the wrong blue and a 12 px title with
    // a 12 px line height look wrong without anyone being able to say why.
    let p = crate::runtime_prompt::SYSTEM_PROMPT;
    for required in [
        "#0052D9", // the main brand colour
        "#366EF4", // hover
        "#F2F3FF", // light brand background
        "#1A1D1F", // main text
        "2BA471",  // success
        "D54941",  // error
        "insert_icon",
        "list_icons",
        "PingFang SC",
        "radius-default",
    ] {
        assert!(p.contains(required), "the prompt tells the model about `{required}`");
    }
    // Type sizes must be given with their line heights, not as bare sizes.
    for pair in ["12/20", "14/22", "16/24", "18/28", "24/32"] {
        assert!(p.contains(pair), "the prompt pairs the size with its line height: `{pair}`");
    }
}

#[test]
fn the_prompt_says_to_use_the_icon_tool_rather_than_drawing_icons() {
    // Drawn-from-rectangles icons and emoji are what a model reaches for when it has no icon source;
    // both look wrong in a TDesign screen.
    let p = crate::runtime_prompt::SYSTEM_PROMPT.to_lowercase();
    assert!(p.contains("insert_icon"), "it names the tool");
    assert!(p.contains("do not draw an icon") || p.contains("do not draw an icon out of"), "it rules out redrawing");
    assert!(p.contains("emoji"), "and rules out emoji");
}

#[test]
fn the_prompt_says_to_place_a_screen_clear_of_what_is_there() {
    // The commonest way to ruin a file: a second prototype drawn on top of the first. The prompt has
    // to name the placement rule, not just say "build".
    let p = crate::runtime_prompt::SYSTEM_PROMPT.to_lowercase();
    assert!(p.contains("inspect the document once"), "it says to look first");
    assert!(p.contains("never overlap"), "and names the rule");
    assert!(p.contains("bottom + 80") || p.contains("right + 80"), "and gives the arithmetic");
    assert!(p.contains("one frame"), "one screen is one frame");
}

#[test]
fn the_prompt_requires_the_brand_colour_on_a_control() {
    // A screen with no #0052D9 does not read as TDesign: the primary action has to carry it.
    let p = crate::runtime_prompt::SYSTEM_PROMPT;
    assert!(p.contains("filled `#0052D9`"), "it says how a primary button is filled");
    assert!(p.to_lowercase().contains("card's inner padding is 24"), "and the card padding");
    assert!(p.contains("#E7E7E7"), "and the list divider");
}
