//! The Agent panel's state machine, without a window: statuses, requests and the requirements list.

use figcraft_agent::AgentErrorKind;
use figcraft_agent::history::Transcript;
use figcraft_agent::run::{AgentEvent, AgentRun, Limits, Outcome};
use figcraft_doc::{Priority, Requirement, RequirementsDocument};
use serde_json::json;

use crate::agent::{AgentPanel, AgentTab, ChatView, Request, RequirementsDraft, RequirementsList, RunStatus, Running, conversation_view, status_of};

fn limits() -> Limits {
    Limits::default()
}

/// A success with no edits: enough for a panel-state test, and nothing to commit.
fn success_terminal(turn: u64) -> figcraft_agent::TurnTerminal {
    figcraft_agent::TurnTerminal::Success { turn, answer: String::new(), commit: None, continuation: Vec::new() }
}

#[test]
fn a_new_panel_is_idle_with_an_empty_composer() {
    let p = AgentPanel::new("doc-1");
    assert_eq!(p.tab, AgentTab::Chat);
    assert_eq!(p.status, RunStatus::Idle);
    assert!(p.current.is_none());
    assert!(!p.can_send(), "an empty composer cannot send");
    assert!(!p.is_running());
}

#[test]
fn a_blank_or_whitespace_composer_cannot_send() {
    let mut p = AgentPanel::new("doc-1");
    p.draft = "   \n  ".into();
    assert!(!p.can_send(), "whitespace is not a request");
    p.draft = "draw a card".into();
    assert!(p.can_send());
}

#[test]
fn nothing_can_be_sent_while_a_turn_runs() {
    let mut p = AgentPanel::new("doc-1");
    p.draft = "second request".into();
    p.status = RunStatus::Streaming;
    assert!(!p.can_send(), "the composer is disabled mid-turn");
    assert!(p.is_running());
    p.status = RunStatus::Tool;
    assert!(p.is_running(), "a tool is still a running turn");
    p.status = RunStatus::Done;
    assert!(p.can_send(), "the composer is usable again");
}

#[test]
fn a_turn_streams_text_then_commits() {
    let mut p = AgentPanel::new("doc-1");
    let l = limits();
    p.begin_turn(1, "draw a card");
    assert_eq!(p.status, RunStatus::Streaming);

    p.apply(&AgentEvent::TextDelta { turn: 1, text: "Drawing ".into() }, &l);
    p.apply(&AgentEvent::TextDelta { turn: 1, text: "it.".into() }, &l);
    assert_eq!(p.current.as_ref().map(|r| r.answer.as_str()), Some("Drawing it."));

    p.settle(&success_terminal(1), Some("AI: draw a card"));
    assert_eq!(p.status, RunStatus::Done);
    assert_eq!(p.last_undo_label.as_deref(), Some("AI: draw a card"), "the undo step is shown");
    assert!(!p.is_running());
}

#[test]
fn tool_rows_follow_the_events() {
    let mut p = AgentPanel::new("doc-1");
    let l = limits();
    p.begin_turn(1, "x");
    p.apply(&AgentEvent::ToolStarted { turn: 1, name: "create_node".into() }, &l);
    assert_eq!(p.status, RunStatus::Tool);
    assert_eq!(p.tools.len(), 1);
    assert!(!p.tools[0].ok, "a running tool is not yet known to have succeeded");

    p.apply(&AgentEvent::ToolFinished { turn: 1, name: "create_node".into(), ok: true }, &l);
    assert_eq!(p.tools.len(), 1, "the row is updated, not duplicated");
    assert!(p.tools[0].ok);

    p.apply(&AgentEvent::ToolStarted { turn: 1, name: "set_properties".into() }, &l);
    p.apply(&AgentEvent::ToolFinished { turn: 1, name: "set_properties".into(), ok: false }, &l);
    assert_eq!(p.tools.len(), 2);
    assert!(!p.tools[1].ok, "a failed tool is marked");
}

#[test]
fn a_failure_shows_the_message_and_the_action() {
    let mut p = AgentPanel::new("doc-1");
    p.begin_turn(1, "x");
    p.settle(&figcraft_agent::TurnTerminal::Failed { turn: 1, kind: AgentErrorKind::Authentication, message: "refused".into() }, None);
    assert_eq!(p.status, RunStatus::Failed);
    let error = p.error.as_deref().expect("an error is shown");
    assert!(error.contains("refused"), "the message is shown: {error}");
    assert!(error.contains("API key"), "and what to do about it: {error}");
}

#[test]
fn a_revision_conflict_is_its_own_status_with_a_rerun_offer() {
    // The offer differs: a conflict means "run it again", not "fix a setting".
    let mut p = AgentPanel::new("doc-1");
    p.begin_turn(1, "x");
    p.settle(
        &figcraft_agent::TurnTerminal::Failed {
            turn: 1,
            kind: AgentErrorKind::Conflict,
            message: "the document changed while the Agent worked".into(),
        },
        None,
    );
    assert_eq!(p.status, RunStatus::Conflict, "not a generic failure");
    let error = p.error.as_deref().expect("error text");
    assert!(error.contains("Run the request again"), "the action is to rerun: {error}");
    assert_eq!(p.current.as_ref().and_then(|r| r.outcome), Some(Outcome::Failed), "the run records the failure");
}

#[test]
fn cancelling_settles_the_turn() {
    let mut p = AgentPanel::new("doc-1");
    let l = limits();
    p.begin_turn(1, "x");
    p.apply(&AgentEvent::TextDelta { turn: 1, text: "part".into() }, &l);
    p.settle(&figcraft_agent::TurnTerminal::Cancelled { turn: 1 }, None);
    assert_eq!(p.status, RunStatus::Cancelled);
    assert!(!p.is_running());
    assert_eq!(p.current.as_ref().map(|r| r.answer.as_str()), Some("part"), "the text so far is kept");
    // A late commit cannot revive a cancelled turn.
    p.settle(&success_terminal(1), Some("AI: late"));
    assert_eq!(p.status, RunStatus::Done, "the panel shows what the runtime reported last");
    assert_eq!(
        p.current.as_ref().and_then(|r| r.outcome),
        Some(Outcome::Cancelled),
        "the run itself stays cancelled: a commit after cancellation never happened"
    );
}

#[test]
fn the_summary_for_inspect_is_small_and_carries_no_transcript() {
    let mut p = AgentPanel::new("doc-1");
    let l = limits();
    p.begin_turn(1, "x");
    p.apply(&AgentEvent::TextDelta { turn: 1, text: "a secret-ish answer".into() }, &l);
    p.settle(&success_terminal(1), Some("AI: x"));

    let s = p.summary();
    let text = s.to_string();
    assert!(!text.contains("secret-ish"), "the transcript is not in the status summary: {text}");
    assert_eq!(s["status"], "done");
    assert_eq!(s["running"], false);
    assert_eq!(s["tab"], "chat");
    assert!(s["canSend"].is_boolean());
    // A credential-shaped field never appears.
    for leaked in ["credential", "apiKey", "baseUrl", "token"] {
        assert!(!text.contains(leaked), "`{leaked}` is not in the panel summary: {text}");
    }
}

#[test]
fn a_request_clears_the_draft_and_starts_streaming() {
    let mut p = AgentPanel::new("doc-1");
    p.draft = "draw a card".into();
    let mut requests = vec![Request::Send { request: "draw a card".into() }];
    p.take_requests(&mut requests);
    assert!(requests.is_empty(), "the panel took the request");
    assert_eq!(p.status, RunStatus::Streaming);
    assert!(p.draft.is_empty(), "the composer is cleared");
}

#[test]
fn opening_and_closing_a_requirements_document() {
    let mut p = AgentPanel::new("doc-1");
    let mut requests = vec![Request::OpenRequirements { id: "PRD:1".into() }];
    p.take_requests(&mut requests);
    assert_eq!(p.editing.as_deref(), Some("PRD:1"));

    let mut requests = vec![Request::CloseRequirements];
    p.take_requests(&mut requests);
    assert!(p.editing.is_none(), "the editor closed");
    assert!(p.requirements_draft.is_none());
}

#[test]
fn a_cancellation_token_is_held_outside_the_summary() {
    let mut running = Running::default();
    assert!(!running.cancel(), "nothing to cancel");
    let token = running.begin();
    assert!(!token.is_cancelled());
    assert!(running.cancel(), "the turn was cancelled");
    assert!(token.is_cancelled(), "the shared flag is set");
    assert!(!running.cancel(), "cancelling twice is harmless");
}

#[test]
fn a_draft_round_trips_through_the_command_parameters() {
    let d = RequirementsDocument {
        id: "PRD:1".into(),
        title: "Checkout".into(),
        summary: "Fewer steps.".into(),
        goals: vec!["One screen".into()],
        non_goals: vec!["New providers".into()],
        audiences: vec!["Phone buyers".into()],
        requirements: vec![
            Requirement {
                id: "r-1".into(),
                title: "Remember the address".into(),
                detail: "Prefill.".into(),
                priority: Priority::Must,
                acceptance_criteria: vec![],
            },
            Requirement {
                id: "r-2".into(),
                title: "Keep the cart".into(),
                detail: String::new(),
                priority: Priority::Should,
                acceptance_criteria: vec![],
            },
        ],
        acceptance_criteria: vec!["Under 30 s.".into()],
        open_questions: vec!["Gifts?".into()],
    };
    let draft = RequirementsDraft::from_document(&d);
    assert_eq!(draft.title, "Checkout");
    assert_eq!(draft.goals, "One screen");
    assert_eq!(draft.requirements.lines().count(), 2);

    let params = draft.to_params();
    assert_eq!(params["id"], "PRD:1", "an existing document updates by id");
    assert_eq!(params["title"], "Checkout");
    assert_eq!(params["goals"], json!(["One screen"]));
    assert_eq!(params["requirements"][0]["title"], "Remember the address");
    assert_eq!(params["requirements"][0]["priority"], "must");
    assert_eq!(params["requirements"][0]["detail"], "Prefill.");
    // A requirement with no detail omits it rather than sending an empty one.
    assert!(params["requirements"][1].get("detail").is_none(), "no empty detail: {}", params["requirements"][1]);
}

#[test]
fn a_new_draft_sends_no_id_so_the_document_allocates_one() {
    let mut draft = RequirementsDraft::new();
    draft.title = "Fresh".into();
    let params = draft.to_params();
    assert!(params.get("id").is_none(), "a new document has no id yet");
    assert_eq!(params["title"], "Fresh");
    // Blank lines are not items.
    draft.goals = "a\n\n  \nb".into();
    assert_eq!(draft.to_params()["goals"], json!(["a", "b"]));
}

#[test]
fn a_draft_needs_a_title_to_save() {
    let mut draft = RequirementsDraft::new();
    assert!(!draft.is_saveable(), "a blank title cannot be saved");
    draft.title = "   ".into();
    assert!(!draft.is_saveable());
    draft.title = "Real".into();
    assert!(draft.is_saveable());
}

#[test]
fn the_requirements_list_reads_the_command_result() {
    let v = json!({
        "documents": [
            {"id": "PRD:1", "title": "Checkout", "requirements": 3, "acceptanceCriteria": 5},
            {"id": "PRD:2", "title": "Search", "requirements": 0, "acceptanceCriteria": 1},
        ],
        "max": 32
    });
    let list = RequirementsList::from_result(&v);
    assert_eq!(list.documents.len(), 2);
    assert_eq!(list.documents[0].id, "PRD:1");
    assert_eq!(list.documents[0].requirements, 3);
    assert_eq!(list.documents[1].title, "Search");
    assert_eq!(list.max, 32);
    assert!(!list.is_full());
}

#[test]
fn a_full_requirements_list_is_known_before_trying() {
    let docs: Vec<serde_json::Value> =
        (0..32).map(|i| json!({"id": format!("PRD:{i}"), "title": format!("D{i}"), "requirements": 0, "acceptanceCriteria": 0})).collect();
    let list = RequirementsList::from_result(&json!({"documents": docs, "max": 32}));
    assert!(list.is_full(), "creating another would be refused by the command");
}

#[test]
fn an_empty_result_is_an_empty_list() {
    let list = RequirementsList::from_result(&json!({"documents": [], "max": 32}));
    assert!(list.documents.is_empty());
    let list = RequirementsList::from_result(&json!({}));
    assert!(list.documents.is_empty());
    assert_eq!(list.max, 32, "a missing cap takes the documented default");
}

#[test]
fn a_chat_view_shows_finished_turns_and_the_summary() {
    let mut t = Transcript::default();
    let c = t.conversation_mut("doc-1");
    let l = limits();
    c.apply(&AgentEvent::Started { turn: 1 }, "draw", &l);
    c.apply(&AgentEvent::TextDelta { turn: 1, text: "done".into() }, "draw", &l);
    c.settle(&success_terminal(1), "draw");

    let view = conversation_view(&t, "doc-1");
    assert_eq!(view.runs.len(), 1);
    assert_eq!(view.runs[0].answer, "done");
    assert!(!view.is_empty());

    // An unknown conversation is empty, not an error.
    let none = conversation_view(&t, "other");
    assert!(none.is_empty());
}

#[test]
fn a_retry_is_offered_after_a_failure_or_conflict() {
    let mut view = ChatView::default();
    view.runs.push(AgentRun::new(1, "x"));
    assert!(view.can_retry(RunStatus::Failed), "a failure can be retried");
    assert!(view.can_retry(RunStatus::Conflict), "a conflict can be rerun");
    assert!(view.can_retry(RunStatus::Cancelled), "a cancelled turn can be rerun");
    assert!(!view.can_retry(RunStatus::Done), "nothing to retry after success");
    assert!(!view.can_retry(RunStatus::Idle));
    assert!(!view.can_retry(RunStatus::Streaming));
    // With no turns at all there is nothing to retry.
    let empty = ChatView::default();
    assert!(!empty.can_retry(RunStatus::Failed));
}

#[test]
fn a_reopened_conversation_shows_as_idle_not_running() {
    // A file saved mid-turn must not open with a spinner that never stops.
    let mut run = AgentRun::new(1, "x");
    assert_eq!(status_of(Some(&run)), RunStatus::Idle, "an unfinished run is not a running one");
    // Settling fills the empty outcome; `finish` never overwrites one that is set.
    run.settle_on_reopen();
    assert_eq!(run.outcome, Some(Outcome::Interrupted));
    assert_eq!(status_of(Some(&run)), RunStatus::Idle, "an interrupted run opens idle");

    let mut done = AgentRun::new(2, "y");
    done.finish(Outcome::Committed);
    assert_eq!(status_of(Some(&done)), RunStatus::Done);
    assert_eq!(status_of(None), RunStatus::Idle);
}

#[test]
fn the_conversation_id_is_per_file() {
    assert_eq!(crate::agent::conversation_id(7), "doc-7");
    assert_ne!(crate::agent::conversation_id(7), crate::agent::conversation_id(8));
}

// ---------- Send actually starts a turn ----------

/// A host that records what it was asked and emits a scripted turn.
struct Scripted {
    started: Vec<(u64, String)>,
    queue: Vec<crate::agent_host::HostMessage>,
    terminal: Option<figcraft_agent::TurnTerminal>,
    status: crate::agent_host::HostStatus,
}

impl Scripted {
    fn new() -> Self {
        Self { started: Vec::new(), queue: Vec::new(), terminal: None, status: crate::agent_host::HostStatus::Idle }
    }
    /// A turn that succeeds with `replay`, built against `uid`/`revision`.
    fn committing(replay: Vec<serde_json::Value>, uid: u64, revision: u64) -> Self {
        let steps: Vec<figcraft_agent::shadow::ReplayStep> = replay
            .iter()
            .filter_map(|v| {
                Some(figcraft_agent::shadow::ReplayStep {
                    command: v.get("command")?.as_str()?.to_string(),
                    params: v.get("params").cloned().unwrap_or(serde_json::Value::Null),
                })
            })
            .collect();
        let envelope = figcraft_agent::CommitEnvelope::new(1, uid, revision, steps, "AI: draw");
        Self {
            terminal: Some(figcraft_agent::TurnTerminal::Success {
                turn: 1,
                answer: "done".into(),
                commit: Some(envelope),
                continuation: Vec::new(),
            }),
            ..Self::new()
        }
    }
}

impl crate::agent_host::AgentHost for Scripted {
    fn start(&mut self, request: crate::agent_host::HostRequest) -> figcraft_agent::run::CancelToken {
        if let crate::agent_host::HostRequest::Start(req) = request {
            self.started.push((req.turn, req.request.clone()));
            let turn = req.turn;
            self.queue = vec![
                crate::agent_host::HostMessage::Progress(figcraft_agent::run::AgentEvent::Started { turn }),
                crate::agent_host::HostMessage::Progress(figcraft_agent::run::AgentEvent::TextDelta { turn, text: "done".into() }),
            ];
            if let Some(terminal) = self.terminal.take() {
                self.queue.push(crate::agent_host::HostMessage::Terminal(terminal));
            }
            self.status = crate::agent_host::HostStatus::Idle;
        }
        figcraft_agent::run::CancelToken::new()
    }
    fn drain(&mut self) -> Vec<crate::agent_host::HostMessage> {
        std::mem::take(&mut self.queue)
    }
    fn status(&self) -> crate::agent_host::HostStatus {
        self.status
    }
    fn describe(&self) -> String {
        "scripted".into()
    }
}

/// A panel-only harness: the app fields `perform_requests` touches, without a window.
///
/// `provider`: whether the Agent's endpoint is configured, which decides whether Send can start a
/// turn at all.
fn harness_with(provider: bool) -> crate::FigApp {
    let mut s = figcraft_engine::Session::new();
    s.execute("file.new", &serde_json::json!({})).expect("file.new");
    if provider {
        s.prefs.agent = figcraft_engine::prefs::AgentPrefs {
            enabled: true,
            base_url: "http://127.0.0.1:8000".into(),
            model: "test-model".into(),
            ..figcraft_engine::prefs::AgentPrefs::default()
        };
    }
    let mut app = crate::FigApp::new(s, Box::new(crate::NoServices));
    app.ui.agent = Some(AgentPanel::new("doc-1"));
    app
}

fn harness() -> crate::FigApp {
    harness_with(false)
}

#[test]
fn sending_starts_a_turn_on_the_host() {
    // The gap this test closes: Send used to clear the draft and do nothing else, so the panel
    // looked like it had sent while no turn was ever started.
    let mut app = harness_with(true);
    app.ui.agent_host = Some(Box::new(Scripted::new()));

    app.ui.agent_requests.push(Request::Send { request: "draw a card".into() });
    crate::agent::perform_requests(&mut app);

    // The panel moved to streaming, and it did so because a turn was asked for.
    let panel = app.ui.agent.as_ref().expect("panel");
    assert!(panel.current.is_some(), "a turn was begun");
    assert_eq!(panel.current.as_ref().map(|r| r.request.as_str()), Some("draw a card"));
    assert_eq!(panel.status, RunStatus::Streaming);
    assert!(app.ui.agent_turn > 0, "a turn number was allocated");
}

#[test]
fn a_turn_with_no_provider_reports_a_readable_error_and_starts_nothing() {
    // No provider configured: the panel says what to fix rather than clearing the draft silently.
    let mut app = harness_with(false);
    app.ui.agent_host = Some(Box::new(Scripted::new()));
    app.ui.agent_requests.push(Request::Send { request: "draw".into() });
    crate::agent::perform_requests(&mut app);

    let panel = app.ui.agent.as_ref().expect("panel");
    assert_eq!(panel.status, RunStatus::Failed);
    let error = panel.error.as_deref().unwrap_or("");
    assert!(error.contains("provider") || error.contains("Preferences") || error.contains("URL"), "got {error}");
}

#[test]
fn a_build_without_a_host_says_so() {
    let mut app = harness();
    app.ui.agent_requests.push(Request::Send { request: "draw".into() });
    crate::agent::perform_requests(&mut app);
    let panel = app.ui.agent.as_ref().expect("panel");
    assert_eq!(panel.status, RunStatus::Failed);
    assert!(panel.error.as_deref().is_some_and(|e| e.contains("no Agent runtime")));
}

#[test]
fn the_hosts_events_reach_the_panel_and_its_replay_commits() {
    let mut app = harness_with(true);
    let replay = vec![serde_json::json!({
        "command": "node.create",
        "params": {"type": "rectangle", "width": 10.0, "height": 10.0}
    })];
    let (uid, revision) = app.session.ds().map(|d| (d.uid, d.revision)).expect("a document");
    app.ui.agent_host = Some(Box::new(Scripted::committing(replay, uid, revision)));
    app.ui.agent_requests.push(Request::Send { request: "draw".into() });
    crate::agent::perform_requests(&mut app);
    let before = app.session.doc().expect("doc").nodes().count();

    // The frame's second step: drain the events, then commit the replay.
    crate::agent::poll_host(&mut app);

    let panel = app.ui.agent.as_ref().expect("panel");
    assert_eq!(panel.status, RunStatus::Done, "the turn finished");
    assert_eq!(panel.current.as_ref().map(|r| r.answer.as_str()), Some("done"));
    assert_eq!(panel.last_undo_label.as_deref(), Some("AI: draw"), "the undo step is named after the request");
    assert_eq!(app.session.doc().expect("doc").nodes().count(), before + 1, "the replay landed");
    assert_eq!(app.session.undo_depth().0, 1, "as exactly one undo step");
}

#[test]
fn a_configured_provider_enables_the_send_button() {
    // The bug this pins: `agent_configured` was initialized to false and never updated, so the Send
    // button stayed disabled however the provider was set, and clicking it did nothing.
    let mut app = harness_with(true);
    let _ = crate::agent::panel_input(&mut app);
    assert!(app.ui.agent_configured, "a valid provider must enable the panel");
}

#[test]
fn an_unconfigured_or_invalid_provider_leaves_the_button_disabled() {
    // No provider at all.
    let mut app = harness_with(false);
    let _ = crate::agent::panel_input(&mut app);
    assert!(!app.ui.agent_configured, "nothing configured: the button stays disabled");

    // A provider that is enabled but unusable (https is refused, see `config`).
    let mut app = harness_with(false);
    app.session.prefs.agent = figcraft_engine::prefs::AgentPrefs {
        enabled: true,
        base_url: "https://example.com/v1".into(),
        model: "m".into(),
        ..figcraft_engine::prefs::AgentPrefs::default()
    };
    let _ = crate::agent::panel_input(&mut app);
    assert!(!app.ui.agent_configured, "an unusable provider does not enable the button");
}

#[test]
fn the_snapshot_is_a_document_the_worker_can_rebuild() {
    // The bug this pins: the snapshot used `inspect_document`'s output, which is a report for a
    // model (paths, history, a dirty flag), not a document. The worker's `serde_json::from_value`
    // then failed and every turn reported a failure with no tool calls.
    let mut app = harness_with(true);
    app.session.execute("node.create", &serde_json::json!({"type": "rectangle", "width": 10.0, "height": 10.0})).expect("create");
    let snapshot = crate::agent::document_snapshot(&mut app).expect("a snapshot");
    let doc: figcraft_doc::Document = serde_json::from_value(snapshot.document).expect("the snapshot rebuilds into a document");
    assert_eq!(doc.nodes().count(), app.session.doc().expect("doc").nodes().count(), "every layer came along");
    assert!(snapshot.uid > 0, "it names the file");
    assert!(snapshot.revision > 0, "and the revision the commit checks");
}

// ---------- the commit gate: turn, uid and revision ----------

/// A scripted host whose terminal the test sets directly, so a stale envelope can be handed over.
struct TerminalHost(Option<figcraft_agent::TurnTerminal>);

impl crate::agent_host::AgentHost for TerminalHost {
    fn start(&mut self, _request: crate::agent_host::HostRequest) -> figcraft_agent::run::CancelToken {
        figcraft_agent::run::CancelToken::new()
    }
    fn drain(&mut self) -> Vec<crate::agent_host::HostMessage> {
        self.0.take().map(|t| vec![crate::agent_host::HostMessage::Terminal(t)]).unwrap_or_default()
    }
    fn status(&self) -> crate::agent_host::HostStatus {
        crate::agent_host::HostStatus::Idle
    }
    fn describe(&self) -> String {
        "terminal".into()
    }
}

fn current(app: &crate::FigApp) -> (u64, u64) {
    let ds = app.session.ds().expect("a document");
    (ds.uid, ds.revision)
}

#[test]
fn a_matching_envelope_commits_as_one_undo_step() {
    let mut app = harness_with(true);
    let (uid, revision) = current(&app);
    if let Some(p) = app.ui.agent.as_mut() {
        p.begin_turn(1, "draw");
    }
    app.ui.agent_turn = 1;
    let step = figcraft_agent::shadow::ReplayStep {
        command: "node.create".into(),
        params: serde_json::json!({"type": "rectangle", "width": 10.0, "height": 10.0}),
    };
    let envelope = figcraft_agent::CommitEnvelope::new(1, uid, revision, vec![step], "AI: draw");
    app.ui.agent_host = Some(Box::new(TerminalHost(Some(figcraft_agent::TurnTerminal::Success {
        turn: 1,
        answer: "done".into(),
        commit: Some(envelope),
        continuation: Vec::new(),
    }))));
    let before = app.session.doc().expect("doc").nodes().count();

    crate::agent::poll_host(&mut app);

    assert_eq!(app.session.doc().expect("doc").nodes().count(), before + 1, "the envelope landed");
    assert_eq!(app.session.undo_depth().0, 1, "as one undo step");
    assert_eq!(app.ui.agent.as_ref().map(|p| p.status), Some(RunStatus::Done));
}

#[test]
fn a_stale_revision_is_refused_and_nothing_is_applied() {
    // The document moved on while the Agent worked: applying the envelope would overwrite whatever
    // changed it. Nothing lands and the panel offers a rerun.
    let mut app = harness_with(true);
    let (uid, revision) = current(&app);
    if let Some(p) = app.ui.agent.as_mut() {
        p.begin_turn(1, "draw");
    }
    app.ui.agent_turn = 1;
    let step = figcraft_agent::shadow::ReplayStep { command: "node.create".into(), params: serde_json::json!({"type": "ellipse"}) };
    // Built against a revision the document no longer has.
    let envelope = figcraft_agent::CommitEnvelope::new(1, uid, revision + 5, vec![step], "AI: draw");
    app.ui.agent_host = Some(Box::new(TerminalHost(Some(figcraft_agent::TurnTerminal::Success {
        turn: 1,
        answer: "done".into(),
        commit: Some(envelope),
        continuation: Vec::new(),
    }))));
    let before = app.session.doc().expect("doc").nodes().count();
    let depth = app.session.undo_depth();

    crate::agent::poll_host(&mut app);

    assert_eq!(app.session.doc().expect("doc").nodes().count(), before, "nothing was applied");
    assert_eq!(app.session.undo_depth(), depth, "no undo step");
    let panel = app.ui.agent.as_ref().expect("panel");
    assert_eq!(panel.status, RunStatus::Conflict, "the panel offers a rerun");
    assert!(panel.error.as_deref().is_some_and(|e| e.contains("changed")), "and says why: {:?}", panel.error);
}

#[test]
fn an_envelope_for_another_document_is_refused() {
    let mut app = harness_with(true);
    let (uid, revision) = current(&app);
    if let Some(p) = app.ui.agent.as_mut() {
        p.begin_turn(1, "draw");
    }
    app.ui.agent_turn = 1;
    let step = figcraft_agent::shadow::ReplayStep { command: "node.create".into(), params: serde_json::json!({"type": "ellipse"}) };
    let envelope = figcraft_agent::CommitEnvelope::new(1, uid + 99, revision, vec![step], "AI: draw");
    app.ui.agent_host = Some(Box::new(TerminalHost(Some(figcraft_agent::TurnTerminal::Success {
        turn: 1,
        answer: "done".into(),
        commit: Some(envelope),
        continuation: Vec::new(),
    }))));
    let before = app.session.doc().expect("doc").nodes().count();

    crate::agent::poll_host(&mut app);

    assert_eq!(app.session.doc().expect("doc").nodes().count(), before, "nothing was applied");
    assert_eq!(app.ui.agent.as_ref().map(|p| p.status), Some(RunStatus::Conflict));
}

#[test]
fn a_late_result_from_an_older_turn_is_ignored_completely() {
    // A slow turn finishing after the panel moved on must not land in the newer one, and must not
    // touch it at all: the newer turn keeps streaming, its error stays clear, and its own outcome is
    // decided by its own terminal. Reporting a conflict here would put a message about turn 3 in
    // front of a person watching turn 7.
    let mut app = harness_with(true);
    let (uid, revision) = current(&app);
    if let Some(p) = app.ui.agent.as_mut() {
        p.begin_turn(7, "the newer request"); // the panel now shows turn 7
    }
    app.ui.agent_turn = 7;
    let step = figcraft_agent::shadow::ReplayStep { command: "node.create".into(), params: serde_json::json!({"type": "star"}) };
    let envelope = figcraft_agent::CommitEnvelope::new(3, uid, revision, vec![step], "AI: the older one");
    app.ui.agent_host = Some(Box::new(TerminalHost(Some(figcraft_agent::TurnTerminal::Success {
        turn: 3,
        answer: "old".into(),
        commit: Some(envelope),
        continuation: Vec::new(),
    }))));
    let before = app.session.doc().expect("doc").nodes().count();

    crate::agent::poll_host(&mut app);

    assert_eq!(app.session.doc().expect("doc").nodes().count(), before, "the stale turn did not land");
    let panel = app.ui.agent.as_ref().expect("panel");
    assert_eq!(panel.status, RunStatus::Streaming, "the newer turn is untouched and still running");
    assert!(panel.error.is_none(), "no message about the older turn appears: {:?}", panel.error);
    assert!(panel.last_undo_label.is_none(), "and it did not claim an undo step");
}

#[test]
fn a_limited_turn_applies_nothing() {
    // A turn cut short by a limit is not a finished turn: its edits are dropped by the typed terminal.
    let mut app = harness_with(true);
    if let Some(p) = app.ui.agent.as_mut() {
        p.begin_turn(1, "draw");
    }
    app.ui.agent_turn = 1;
    app.ui.agent_host = Some(Box::new(TerminalHost(Some(figcraft_agent::TurnTerminal::LimitReached {
        turn: 1,
        kind: figcraft_agent::AgentErrorKind::Model,
        message: "the turn used its rounds".into(),
    }))));
    let before = app.session.doc().expect("doc").nodes().count();
    let depth = app.session.undo_depth();

    crate::agent::poll_host(&mut app);

    assert_eq!(app.session.doc().expect("doc").nodes().count(), before);
    assert_eq!(app.session.undo_depth(), depth);
    let panel = app.ui.agent.as_ref().expect("panel");
    assert_eq!(panel.status, RunStatus::Failed);
    assert!(panel.error.as_deref().is_some_and(|e| e.contains("rounds")));
}

// ---------- the conversation follows the file ----------

#[test]
fn a_started_turn_is_recorded_in_the_file_transcript() {
    // A file saved mid-turn must reopen showing that turn, not as if nothing happened.
    let mut app = harness_with(true);
    app.ui.agent_host = Some(Box::new(Scripted::new()));
    app.ui.agent_requests.push(Request::Send { request: "draw a card".into() });
    crate::agent::perform_requests(&mut app);

    let turns: Vec<&figcraft_agent::run::AgentRun> = app.ui.agent_transcript.conversations.iter().flat_map(|c| c.turns.iter()).collect();
    assert_eq!(turns.len(), 1, "one turn is recorded");
    assert_eq!(turns[0].request, "draw a card");
    assert_eq!(turns[0].outcome, None, "it has not finished");
    assert!(!app.session.ds().expect("doc").agent().is_empty(), "and it reached the document");
}

#[test]
fn a_finished_turn_records_its_outcome_and_keeps_it_out_of_undo() {
    let mut app = harness_with(true);
    let (uid, revision) = current(&app);
    app.ui.agent_host = Some(Box::new(TerminalHost(None)));
    app.ui.agent_requests.push(Request::Send { request: "draw".into() });
    crate::agent::perform_requests(&mut app);
    let turn = app.ui.agent_turn;
    let step = figcraft_agent::shadow::ReplayStep {
        command: "node.create".into(),
        params: serde_json::json!({"type": "rectangle", "width": 10.0, "height": 10.0}),
    };
    let envelope = figcraft_agent::CommitEnvelope::new(turn, uid, revision, vec![step], "AI: draw");
    app.ui.agent_host = Some(Box::new(TerminalHost(Some(figcraft_agent::TurnTerminal::Success {
        turn,
        answer: "done".into(),
        commit: Some(envelope),
        continuation: Vec::new(),
    }))));
    crate::agent::poll_host(&mut app);

    let turns: Vec<&figcraft_agent::run::AgentRun> = app.ui.agent_transcript.conversations.iter().flat_map(|c| c.turns.iter()).collect();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].outcome, Some(figcraft_agent::run::Outcome::Committed));
    // The design edit is one undo entry; the transcript added none.
    assert_eq!(app.session.undo_depth().0, 1, "only the commit is in the history");
}

#[test]
fn a_failed_turn_is_recorded_as_failed_not_committed() {
    // The transcript and the document must agree: a turn that applied nothing is not "committed".
    let mut app = harness_with(true);
    app.ui.agent_host = Some(Box::new(TerminalHost(None)));
    app.ui.agent_requests.push(Request::Send { request: "draw".into() });
    crate::agent::perform_requests(&mut app);
    let turn = app.ui.agent_turn;
    app.ui.agent_host = Some(Box::new(TerminalHost(Some(figcraft_agent::TurnTerminal::Failed {
        turn,
        kind: figcraft_agent::AgentErrorKind::Unreachable,
        message: "offline".into(),
    }))));
    crate::agent::poll_host(&mut app);

    let turns: Vec<&figcraft_agent::run::AgentRun> = app.ui.agent_transcript.conversations.iter().flat_map(|c| c.turns.iter()).collect();
    assert_eq!(turns.first().and_then(|t| t.outcome), Some(figcraft_agent::run::Outcome::Failed));
    assert_eq!(app.session.undo_depth().0, 0, "nothing was applied");
}

#[test]
fn opening_another_file_shows_that_files_conversation() {
    // Two files, two conversations: switching tabs must not leak one file's chat into the other.
    let mut app = harness_with(true);
    crate::agent::sync_transcript_from_document(&mut app);
    let first_uid = app.session.ds().expect("doc").uid;
    if let Some(p) = app.ui.agent.as_mut() {
        p.begin_turn(1, "the first file's request");
    }
    app.ui.agent_turn = 1;
    crate::agent::begin_transcript_turn(&mut app.ui.agent_transcript, "doc-1", 1, "the first file's request");
    crate::agent::store_transcript(&mut app);

    // A second file, with its own stored transcript.
    app.session.execute("file.new", &serde_json::json!({})).expect("file.new");
    let second_uid = app.session.ds().expect("doc").uid;
    assert_ne!(first_uid, second_uid, "the test needs two files");
    let mut other = figcraft_agent::history::Transcript::default();
    crate::agent::begin_transcript_turn(&mut other, "doc-2", 9, "the second file's request");
    if let Ok(ds) = app.session.ds_mut() {
        ds.set_agent(crate::agent::agent_data_for_test(&other));
    }

    crate::agent::sync_transcript_from_document(&mut app);
    let requests: Vec<&str> = app.ui.agent_transcript.conversations.iter().flat_map(|c| c.turns.iter()).map(|t| t.request.as_str()).collect();
    assert_eq!(requests, ["the second file's request"], "the panel shows the new file's conversation");
}

#[test]
fn a_turn_that_was_running_when_saved_reopens_as_interrupted() {
    // A turn with no outcome was running when the file was written: after a reopen nothing is
    // running, so it shows as interrupted rather than as still streaming.
    let mut s = figcraft_engine::Session::new();
    s.execute("file.new", &serde_json::json!({})).expect("file.new");
    let mut transcript = figcraft_agent::history::Transcript::default();
    crate::agent::begin_transcript_turn(&mut transcript, "doc-1", 1, "an unfinished request");
    if let Ok(ds) = s.ds_mut() {
        ds.set_agent(crate::agent::agent_data_for_test(&transcript));
        ds.mark_saved();
    }
    let mut app = crate::FigApp::new(s, Box::new(crate::NoServices));
    app.ui.agent = Some(AgentPanel::new("doc-1"));

    crate::agent::sync_transcript_from_document(&mut app);
    // Force the switch path by pretending a different file was bound before.
    app.ui.agent_transcript_uid = Some(0);
    crate::agent::sync_transcript_from_document(&mut app);

    let panel = app.ui.agent.as_ref().expect("panel");
    assert_eq!(panel.status, RunStatus::Idle, "not running after a reopen");
    assert_eq!(panel.current.as_ref().map(|r| r.request.as_str()), Some("an unfinished request"), "the request is shown");
    assert_eq!(panel.current.as_ref().and_then(|r| r.outcome), None, "and it has no outcome");
}

// ---------- Markdown export writes the whole document ----------

#[test]
fn a_markdown_filename_is_safe_and_ends_in_md() {
    // The name comes from a document title a person or a model wrote, so it can hold anything a path
    // rejects, including path separators.
    assert_eq!(crate::agent::markdown_filename("Checkout flow"), "Checkout flow.md");
    assert_eq!(crate::agent::markdown_filename("a/b:c*d?e\"f<g>h|i"), "a-b-c-d-e-f-g-h-i.md");
    assert_eq!(crate::agent::markdown_filename("../../etc/passwd"), "..-..-etc-passwd.md");
    assert_eq!(crate::agent::markdown_filename(""), "requirements.md");
    assert_eq!(crate::agent::markdown_filename("   "), "requirements.md");
    assert_eq!(crate::agent::markdown_filename("---"), "requirements.md");
    // A very long title is bounded.
    let long = "x".repeat(500);
    let name = crate::agent::markdown_filename(&long);
    assert!(name.len() <= 84, "bounded: {}", name.len());
    assert!(name.ends_with(".md"));
}

#[test]
fn a_markdown_filename_keeps_non_ascii() {
    // A Chinese title is a perfectly good filename; only what a path rejects is replaced.
    let name = crate::agent::markdown_filename("企业微信侧边栏需求");
    assert_eq!(name, "企业微信侧边栏需求.md");
}

#[test]
fn the_exported_markdown_is_the_whole_document() {
    // The engine renders it; this asserts the contract the UI depends on, that the Markdown has every
    // section rather than the first line the panel used to show.
    let mut app = harness_with(true);
    let created = app
        .session
        .execute(
            "requirements.upsert",
            &serde_json::json!({
                "title": "Checkout",
                "summary": "A checkout flow",
                "goals": ["One screen", "Fast"],
                "requirements": [
                    {"title": "Cart", "detail": "Shows the items", "priority": "must"},
                    {"title": "Pay", "detail": "Takes the card", "priority": "should"},
                ],
                "acceptanceCriteria": ["The cart shows every item", "Payment confirms"],
            }),
        )
        .expect("create the document");
    let id = created["id"].as_str().expect("an id").to_string();

    let rendered = app.session.execute("requirements.exportMarkdown", &serde_json::json!({"id": id})).expect("render");
    let markdown = rendered["markdown"].as_str().expect("markdown");
    for expected in
        ["# Checkout", "A checkout flow", "One screen", "Fast", "Cart", "Shows the items", "Pay", "The cart shows every item", "Payment confirms"]
    {
        assert!(markdown.contains(expected), "the Markdown holds `{expected}`: {markdown}");
    }
    assert!(markdown.lines().count() > 5, "it is a document, not a preview line: {markdown}");
    assert_eq!(rendered["bytes"].as_u64(), Some(markdown.len() as u64), "the byte count is the whole text");
}

#[test]
fn an_export_request_for_a_missing_document_reports_an_error_and_changes_nothing() {
    // An export that cannot render says so; it must not touch the document or the undo history.
    let mut app = harness_with(true);
    let depth = app.session.undo_depth();
    let layers = app.session.doc().expect("doc").nodes().count();
    if let Ok(v) = app.session.execute("requirements.exportMarkdown", &serde_json::json!({"id": "PRD:missing"})) {
        panic!("a missing document must not render: {v}");
    }
    assert_eq!(app.session.undo_depth(), depth, "no undo entry");
    assert_eq!(app.session.doc().expect("doc").nodes().count(), layers, "no document change");
}

// ---------- provider preferences ----------

#[test]
fn the_agent_can_be_configured_by_command() {
    // Preferences has Agent controls; they dispatch this command. Without it there was no way to
    // configure a provider from inside the app at all.
    let mut app = harness_with(false);
    assert!(!app.ui.agent_configured, "nothing configured yet");

    app.session
        .execute("pref.agent", &serde_json::json!({"enabled": true, "baseUrl": "http://127.0.0.1:8000", "model": "test-model"}))
        .expect("pref.agent");

    // Read back through the same path the panel uses, so the change takes effect without a restart.
    let _ = crate::agent::panel_input(&mut app);
    assert!(app.ui.agent_configured, "a valid provider enables the panel at once");
}

#[test]
fn the_provider_status_never_carries_the_credential() {
    // Preferences are saved as text: the status a person sees must not echo the variable's name back
    // as if it were the key, and must never contain a key.
    let mut app = harness_with(false);
    let out = app
        .session
        .execute("pref.agent", &serde_json::json!({"enabled": true, "baseUrl": "http://x:1", "model": "m", "credential": "MY_SECRET_VAR"}))
        .expect("pref.agent");
    assert_eq!(out["hasCredential"], true);
    let text = serde_json::to_string(&out).expect("serialize");
    assert!(!text.contains("MY_SECRET_VAR"), "the status must not echo the name: {text}");
    assert!(out.get("credential").is_none(), "and has no field for it");
}

#[test]
fn an_invalid_provider_url_is_not_configured() {
    // A URL the client refuses leaves the panel disabled and says why, rather than failing at the
    // first turn.
    let mut app = harness_with(false);
    app.session.execute("pref.agent", &serde_json::json!({"enabled": true, "baseUrl": "https://example.com/v1", "model": "m"})).expect("pref.agent");
    let _ = crate::agent::panel_input(&mut app);
    assert!(!app.ui.agent_configured, "https is refused until a shared transport exists");
}

#[test]
fn disabling_the_agent_leaves_it_unconfigured() {
    let mut app = harness_with(false);
    app.session.execute("pref.agent", &serde_json::json!({"enabled": true, "baseUrl": "http://127.0.0.1:8000", "model": "m"})).expect("enable");
    let _ = crate::agent::panel_input(&mut app);
    assert!(app.ui.agent_configured);

    app.session.execute("pref.agent", &serde_json::json!({"enabled": false})).expect("disable");
    let _ = crate::agent::panel_input(&mut app);
    assert!(!app.ui.agent_configured, "a disabled Agent is not configured");
}

#[test]
fn a_test_of_the_settings_never_touches_the_document() {
    // Whatever a "test connection" does, it must not create a shadow, a replay or an undo entry: it
    // is a settings check, not a turn.
    let app = harness_with(true);
    let depth = app.session.undo_depth();
    let layers = app.session.doc().expect("doc").nodes().count();
    // Building the config and validating it is what a settings check does before any request.
    let config = figcraft_agent::AgentConfig::from_prefs(&app.session.prefs.agent).expect("valid");
    assert_eq!(config.model(), "test-model");
    assert_eq!(app.session.undo_depth(), depth, "no undo entry");
    assert_eq!(app.session.doc().expect("doc").nodes().count(), layers, "no document change");
}

// ---------- the connection test is a request, not a blocking call ----------

#[test]
fn a_test_in_flight_disables_the_button_so_it_cannot_start_twice() {
    // Clicking twice used to start two requests. The persistent state is what prevents it: the button
    // is drawn from it, and a second click finds it enabled only when nothing is running.
    let mut state = crate::agent::AgentConnectionState::default();
    assert!(!state.is_testing(), "nothing tested yet");
    assert!(state.message().is_none(), "and nothing to show");

    state = crate::agent::AgentConnectionState::Testing;
    assert!(state.is_testing(), "a test in flight says so");
    assert!(state.message().is_some(), "and shows that it is working");
}

#[test]
fn each_result_kind_has_something_to_show() {
    use crate::agent::AgentConnectionState;
    // Every ending is visible: a silent no-op is what makes a person press the button again.
    for state in [
        AgentConnectionState::Testing,
        AgentConnectionState::Ready,
        AgentConnectionState::Failed("the provider refused the key".into()),
        AgentConnectionState::Cancelled,
    ] {
        assert!(state.message().is_some(), "{state:?} must say something");
    }
    let failed = AgentConnectionState::from_status(figcraft_agent::runtime::ConnectionStatus::Failed("nope".into()));
    assert_eq!(failed.message().as_deref(), Some("nope"), "a failure shows its cause");
    let cancelled = AgentConnectionState::from_status(figcraft_agent::runtime::ConnectionStatus::Cancelled);
    assert!(!cancelled.is_testing(), "a stopped test is not still running");
}

#[test]
fn a_result_from_an_older_test_is_discarded() {
    // A test whose result arrives after the panel moved on must not settle the newer one: showing
    // "ready" for an endpoint the person has since edited would be worse than showing nothing.
    let mut app = harness_with(true);
    app.ui.agent_connection_request = 7;
    app.ui.agent_connection = crate::agent::AgentConnectionState::Testing;

    // A result for request 3 arrives.
    crate::agent::apply_connection_result(&mut app, 3, figcraft_agent::runtime::ConnectionStatus::Ready);
    assert_eq!(app.ui.agent_connection, crate::agent::AgentConnectionState::Testing, "the stale result changed nothing");

    // The one it is waiting for.
    crate::agent::apply_connection_result(&mut app, 7, figcraft_agent::runtime::ConnectionStatus::Ready);
    assert_eq!(app.ui.agent_connection, crate::agent::AgentConnectionState::Ready);
}

// ---------- the transcript records the live commit, not the model's answer ----------

/// The transcript outcome recorded for `turn` in the file's conversation.
fn recorded_outcome(app: &crate::FigApp, turn: u64) -> Option<figcraft_agent::run::Outcome> {
    app.ui.agent_transcript.conversations.iter().flat_map(|c| c.turns.iter()).find(|r| r.turn == turn).and_then(|r| r.outcome)
}

#[test]
fn a_replay_that_applies_is_recorded_as_committed() {
    // The model answered *and* the batch landed: only then is the turn committed.
    let mut app = harness_with(true);
    let (uid, revision) = current(&app);
    app.ui.agent_host = Some(Box::new(TerminalHost(None)));
    app.ui.agent_requests.push(Request::Send { request: "draw".into() });
    crate::agent::perform_requests(&mut app);
    let turn = app.ui.agent_turn;

    let step = figcraft_agent::shadow::ReplayStep {
        command: "node.create".into(),
        params: serde_json::json!({"type": "rectangle", "width": 10.0, "height": 10.0}),
    };
    let envelope = figcraft_agent::CommitEnvelope::new(turn, uid, revision, vec![step], "AI: draw");
    app.ui.agent_host = Some(Box::new(TerminalHost(Some(figcraft_agent::TurnTerminal::Success {
        turn,
        answer: "done".into(),
        commit: Some(envelope),
        continuation: Vec::new(),
    }))));
    crate::agent::poll_host(&mut app);

    assert_eq!(recorded_outcome(&app, turn), Some(figcraft_agent::run::Outcome::Committed), "the batch landed");
    assert_eq!(app.session.undo_depth().0, 1, "and it is one undo entry");
}

#[test]
fn a_read_only_success_is_recorded_as_completed_not_committed() {
    // The model answered and changed nothing. Calling that "committed" claims an undo entry that does
    // not exist, and the transcript would repeat the claim.
    let mut app = harness_with(true);
    app.ui.agent_host = Some(Box::new(TerminalHost(None)));
    app.ui.agent_requests.push(Request::Send { request: "what is here?".into() });
    crate::agent::perform_requests(&mut app);
    let turn = app.ui.agent_turn;

    app.ui.agent_host = Some(Box::new(TerminalHost(Some(figcraft_agent::TurnTerminal::Success {
        turn,
        answer: "an empty page".into(),
        commit: None,
        continuation: Vec::new(),
    }))));
    crate::agent::poll_host(&mut app);

    assert_eq!(recorded_outcome(&app, turn), Some(figcraft_agent::run::Outcome::Completed));
    assert_eq!(app.session.undo_depth().0, 0, "nothing was applied");
}

#[test]
fn a_stale_revision_is_recorded_as_conflict_and_keeps_the_last_good_continuation() {
    // The model answered but the document moved on, so nothing was applied. The transcript must say so,
    // and the continuation must not advance: the next turn would otherwise assume edits landed that
    // were rejected.
    let mut app = harness_with(true);
    let (uid, revision) = current(&app);
    app.ui.agent_host = Some(Box::new(TerminalHost(None)));
    app.ui.agent_requests.push(Request::Send { request: "draw".into() });
    crate::agent::perform_requests(&mut app);
    let turn = app.ui.agent_turn;

    let step = figcraft_agent::shadow::ReplayStep { command: "node.create".into(), params: serde_json::json!({"type": "star"}) };
    // Built against a revision the document no longer has.
    let envelope = figcraft_agent::CommitEnvelope::new(turn, uid, revision + 5, vec![step], "AI: draw");
    let continuation = vec![serde_json::json!({"role": "user", "content": [{"type": "text", "text": "the rejected run"}]})];
    app.ui.agent_host = Some(Box::new(TerminalHost(Some(figcraft_agent::TurnTerminal::Success {
        turn,
        answer: "done".into(),
        commit: Some(envelope),
        continuation: continuation.clone(),
    }))));
    let before = app.session.doc().expect("doc").nodes().count();
    crate::agent::poll_host(&mut app);

    assert_eq!(recorded_outcome(&app, turn), Some(figcraft_agent::run::Outcome::Conflict), "the transcript says the edits were not applied");
    assert_eq!(app.session.doc().expect("doc").nodes().count(), before, "and nothing was applied");
    let kept = app.ui.agent_transcript.conversations.iter().find_map(|c| c.continuation.first()).cloned();
    assert!(kept.is_none(), "a rejected run does not become the next turn's history: {kept:?}");
}

#[test]
fn a_batch_that_cannot_apply_is_recorded_as_conflict() {
    // The envelope was accepted but `command.batch` refused it and rolled back. Nothing landed, so the
    // transcript must not claim a commit.
    let mut app = harness_with(true);
    let (uid, revision) = current(&app);
    app.ui.agent_host = Some(Box::new(TerminalHost(None)));
    app.ui.agent_requests.push(Request::Send { request: "draw".into() });
    crate::agent::perform_requests(&mut app);
    let turn = app.ui.agent_turn;

    // A command that does not exist: the batch refuses the whole thing.
    let step = figcraft_agent::shadow::ReplayStep { command: "node.thisCommandDoesNotExist".into(), params: serde_json::json!({}) };
    let envelope = figcraft_agent::CommitEnvelope::new(turn, uid, revision, vec![step], "AI: broken");
    app.ui.agent_host = Some(Box::new(TerminalHost(Some(figcraft_agent::TurnTerminal::Success {
        turn,
        answer: "done".into(),
        commit: Some(envelope),
        continuation: Vec::new(),
    }))));
    let before = app.session.doc().expect("doc").nodes().count();
    crate::agent::poll_host(&mut app);

    assert_eq!(recorded_outcome(&app, turn), Some(figcraft_agent::run::Outcome::Conflict));
    assert_eq!(app.session.doc().expect("doc").nodes().count(), before, "the batch rolled back");
    assert_eq!(app.session.undo_depth().0, 0, "and left no undo entry");
}

#[test]
fn a_stale_terminal_changes_no_transcript_record_for_the_current_turn() {
    // A late result from an older turn must not write anything about the newer one.
    let mut app = harness_with(true);
    app.ui.agent_host = Some(Box::new(TerminalHost(None)));
    app.ui.agent_requests.push(Request::Send { request: "the newer request".into() });
    crate::agent::perform_requests(&mut app);
    let newer = app.ui.agent_turn;

    // A terminal for an older turn arrives.
    app.ui.agent_host = Some(Box::new(TerminalHost(Some(figcraft_agent::TurnTerminal::Failed {
        turn: newer.saturating_sub(3),
        kind: figcraft_agent::AgentErrorKind::Unreachable,
        message: "the old turn".into(),
    }))));
    crate::agent::poll_host(&mut app);

    assert_eq!(recorded_outcome(&app, newer), None, "the newer turn is untouched");
}

// ---------- reasoning is shown, never recorded ----------

#[test]
fn reasoning_streams_into_the_panel_but_not_the_transcript() {
    // What the person asked for: see how the model got there. What must not happen: the scratchpad
    // becoming part of the conversation a later turn continues from.
    let mut app = harness_with(true);
    app.ui.agent_host = Some(Box::new(TerminalHost(None)));
    app.ui.agent_requests.push(Request::Send { request: "draw".into() });
    crate::agent::perform_requests(&mut app);
    let turn = app.ui.agent_turn;

    let l = limits();
    {
        let panel = app.ui.agent.as_mut().expect("panel");
        panel.apply(&AgentEvent::ReasoningDelta { turn, text: "First a frame, then ".into() }, &l);
        panel.apply(&AgentEvent::ReasoningDelta { turn, text: "the rows.".into() }, &l);
        assert_eq!(panel.reasoning, "First a frame, then the rows.", "the panel holds the reasoning");
        assert!(!panel.show_reasoning, "collapsed by default: the answer is what most turns are for");
    }
    // The answer the panel will record: a turn's own answer arrives as text, not as reasoning.
    if let Some(panel) = app.ui.agent.as_mut() {
        panel.apply(&AgentEvent::TextDelta { turn, text: "Built it.".into() }, &l);
    }

    // Settle the turn and record it: the reasoning must not appear in the run.
    app.ui.agent_host = Some(Box::new(TerminalHost(Some(figcraft_agent::TurnTerminal::Success {
        turn,
        answer: "Built it.".into(),
        commit: None,
        continuation: Vec::new(),
    }))));
    crate::agent::poll_host(&mut app);

    let run = app.ui.agent_transcript.conversations.iter().flat_map(|c| c.turns.iter()).find(|r| r.turn == turn);
    let recorded = run.expect("the turn was recorded");
    assert_eq!(recorded.answer, "Built it.", "the answer is recorded");
    assert!(!format!("{recorded:?}").contains("First a frame"), "the reasoning is not: {:?}", recorded);
}

#[test]
fn a_new_turn_clears_the_previous_reasoning() {
    // Otherwise a new turn opens showing the last one's thinking, which reads as its own.
    let mut app = harness_with(true);
    app.ui.agent_host = Some(Box::new(TerminalHost(None)));
    app.ui.agent_requests.push(Request::Send { request: "first".into() });
    crate::agent::perform_requests(&mut app);
    let first = app.ui.agent_turn;
    let l = limits();
    {
        let panel = app.ui.agent.as_mut().expect("panel");
        panel.apply(&AgentEvent::ReasoningDelta { turn: first, text: "old thinking".into() }, &l);
        assert!(!panel.reasoning.is_empty());
    }

    app.ui.agent_requests.push(Request::Send { request: "second".into() });
    crate::agent::perform_requests(&mut app);
    assert_eq!(app.ui.agent.as_ref().map(|p| p.reasoning.as_str()), Some(""), "the new turn starts clean");
}

#[test]
fn the_reasoning_pane_toggles() {
    // A disclosure, so a person can open it, read it and put it away without losing the answer.
    let mut app = harness_with(true);
    {
        let panel = app.ui.agent.as_mut().expect("panel");
        assert!(!panel.show_reasoning, "closed by default");
        panel.show_reasoning = true;
        assert!(panel.show_reasoning);
    }
}

// ---------- a refused replay is kept, not thrown away ----------

#[test]
fn a_revision_conflict_keeps_the_replay_so_the_work_is_not_lost() {
    // A turn takes minutes and its commands are absolute, so the work survives a conflict: the panel
    // keeps the envelope and offers to apply it. Discarding it is what made a conflict mean "do it all
    // again".
    let mut app = harness_with(true);
    let (uid, revision) = current(&app);
    app.ui.agent_host = Some(Box::new(TerminalHost(None)));
    app.ui.agent_requests.push(Request::Send { request: "draw".into() });
    crate::agent::perform_requests(&mut app);
    let turn = app.ui.agent_turn;

    let step = figcraft_agent::shadow::ReplayStep {
        command: "node.create".into(),
        params: serde_json::json!({"type": "rectangle", "width": 10.0, "height": 10.0}),
    };
    let envelope = figcraft_agent::CommitEnvelope::new(turn, uid, revision + 9, vec![step], "AI: draw");
    app.ui.agent_host = Some(Box::new(TerminalHost(Some(figcraft_agent::TurnTerminal::Success {
        turn,
        answer: "done".into(),
        commit: Some(envelope),
        continuation: Vec::new(),
    }))));
    crate::agent::poll_host(&mut app);

    let panel = app.ui.agent.as_ref().expect("panel");
    assert_eq!(panel.status, RunStatus::Conflict, "refused");
    assert!(panel.recoverable.is_some(), "but the work is kept");
    // A new file is one page and no layers; the refused replay added nothing.
    assert_eq!(app.session.undo_depth().0, 0, "and nothing was applied without a click");
}

#[test]
fn applying_a_refused_replay_lands_it_as_one_undo_step() {
    // The person's decision: the document moved on, but the Agent's commands are absolute, so applying
    // them now produces what it built.
    let mut app = harness_with(true);
    let (uid, revision) = current(&app);
    app.ui.agent_host = Some(Box::new(TerminalHost(None)));
    app.ui.agent_requests.push(Request::Send { request: "draw".into() });
    crate::agent::perform_requests(&mut app);
    let turn = app.ui.agent_turn;

    let step = figcraft_agent::shadow::ReplayStep {
        command: "node.create".into(),
        params: serde_json::json!({"type": "rectangle", "width": 10.0, "height": 10.0}),
    };
    let envelope = figcraft_agent::CommitEnvelope::new(turn, uid, revision + 9, vec![step], "AI: draw");
    app.ui.agent_host = Some(Box::new(TerminalHost(Some(figcraft_agent::TurnTerminal::Success {
        turn,
        answer: "done".into(),
        commit: Some(envelope),
        continuation: Vec::new(),
    }))));
    crate::agent::poll_host(&mut app);
    assert_eq!(app.session.undo_depth().0, 0, "refused: no undo step");

    app.ui.agent_requests.push(Request::ApplyRefused);
    crate::agent::perform_requests(&mut app);

    assert_eq!(app.session.undo_depth().0, 1, "the replay landed as one undo step");
    let panel = app.ui.agent.as_ref().expect("panel");
    assert_eq!(panel.status, RunStatus::Done);
    assert!(panel.recoverable.is_none(), "the offer is spent");
    assert!(panel.error.is_none(), "and the error is cleared");
}

// ---------- the requirements documents are reachable ----------

#[test]
fn the_requirements_panel_opens_on_its_own_tab() {
    // The documents are Agent-produced, but reaching them used to need opening the Agent and then
    // finding a second tab -- which is why a written document read as missing.
    let mut app = harness_with(true);
    app.ui.agent = None;
    app.run("view.requirements", serde_json::json!({}));
    let panel = app.ui.agent.as_ref().expect("the panel opened");
    assert_eq!(panel.tab, AgentTab::Requirements, "on the requirements tab");
}

#[test]
fn a_written_document_is_named_in_the_panel() {
    // A turn that writes a PRD answers "写好了"; naming the document and offering to open it is the
    // difference between a feature and a mystery.
    let mut app = harness_with(true);
    let (uid, revision) = current(&app);
    app.ui.agent_host = Some(Box::new(TerminalHost(None)));
    app.ui.agent_requests.push(Request::Send { request: "write a prd".into() });
    crate::agent::perform_requests(&mut app);
    let turn = app.ui.agent_turn;

    let step = figcraft_agent::shadow::ReplayStep {
        command: "requirements.upsert".into(),
        params: serde_json::json!({"title": "Sidebar", "requirements": [{"title": "a"}]}),
    };
    let envelope = figcraft_agent::CommitEnvelope::new(turn, uid, revision, vec![step], "AI: write a prd");
    app.ui.agent_host = Some(Box::new(TerminalHost(Some(figcraft_agent::TurnTerminal::Success {
        turn,
        answer: "写好了".into(),
        commit: Some(envelope),
        continuation: Vec::new(),
    }))));
    crate::agent::poll_host(&mut app);

    let panel = app.ui.agent.as_ref().expect("panel");
    assert!(panel.wrote_requirements.is_some(), "the panel says it wrote one");
    assert_eq!(panel.status, RunStatus::Done, "and the turn committed");
}

#[test]
fn showing_requirements_switches_the_tab() {
    let mut app = harness_with(true);
    app.ui.agent_requests.push(Request::ShowRequirements);
    crate::agent::perform_requests(&mut app);
    assert_eq!(app.ui.agent.as_ref().map(|p| p.tab), Some(AgentTab::Requirements));
}
