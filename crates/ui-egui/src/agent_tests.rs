//! The Agent panel's state machine, without a window: statuses, requests and the requirements list.

use figcraft_agent::AgentErrorKind;
use figcraft_agent::history::Transcript;
use figcraft_agent::run::{AgentEvent, AgentRun, Limits, Outcome};
use figcraft_doc::{Priority, Requirement, RequirementsDocument};
use serde_json::json;

use crate::agent::{
    AgentPanel, AgentTab, ChatView, Request, RequirementsDraft, RequirementsList, RunStatus, Running, conversation_view, status_of,
};

fn limits() -> Limits {
    Limits::default()
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

    p.apply(&AgentEvent::Committed { turn: 1, undo_label: "AI: draw a card".into() }, &l);
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
    let l = limits();
    p.begin_turn(1, "x");
    p.apply(&AgentEvent::Failed { turn: 1, kind: AgentErrorKind::Authentication, message: "refused".into() }, &l);
    assert_eq!(p.status, RunStatus::Failed);
    let error = p.error.as_deref().expect("an error is shown");
    assert!(error.contains("refused"), "the message is shown: {error}");
    assert!(error.contains("API key"), "and what to do about it: {error}");
}

#[test]
fn a_revision_conflict_is_its_own_status_with_a_rerun_offer() {
    // The offer differs: a conflict means "run it again", not "fix a setting".
    let mut p = AgentPanel::new("doc-1");
    let l = limits();
    p.begin_turn(1, "x");
    p.apply(
        &AgentEvent::Failed {
            turn: 1,
            kind: AgentErrorKind::Conflict,
            message: "the document changed while the Agent worked".into(),
        },
        &l,
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
    p.apply(&AgentEvent::Cancelled { turn: 1 }, &l);
    assert_eq!(p.status, RunStatus::Cancelled);
    assert!(!p.is_running());
    assert_eq!(p.current.as_ref().map(|r| r.answer.as_str()), Some("part"), "the text so far is kept");
    // A late commit cannot revive a cancelled turn.
    p.apply(&AgentEvent::Committed { turn: 1, undo_label: "AI: late".into() }, &l);
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
    p.apply(&AgentEvent::Committed { turn: 1, undo_label: "AI: x".into() }, &l);

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
            Requirement { id: "r-1".into(), title: "Remember the address".into(), detail: "Prefill.".into(), priority: Priority::Must, acceptance_criteria: vec![] },
            Requirement { id: "r-2".into(), title: "Keep the cart".into(), detail: String::new(), priority: Priority::Should, acceptance_criteria: vec![] },
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
    c.apply(&AgentEvent::Committed { turn: 1, undo_label: "AI: draw".into() }, "draw", &l);

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
