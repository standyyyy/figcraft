//! The shadow session: isolation, replay, the revision guard, and one-undo commits.

use figcraft_agent_tools::ToolPolicy;
use figcraft_engine::Session;
use serde_json::json;

use crate::AgentErrorKind;
use crate::shadow::{Commit, ReplayStep, Shadow, undo_label};

/// A live session with one empty design file.
fn live() -> Session {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).expect("file.new");
    s
}

/// Create a rectangle through the shadow, as a model's `create_node` would.
fn create_rect(sh: &mut Shadow, x: f64, y: f64, w: f64, h: f64) -> serde_json::Value {
    let r = sh.call(ToolPolicy::EmbeddedAgent, "create_node", &json!({"type": "rectangle", "x": x, "y": y, "width": w, "height": h, "name": "R"}));
    assert!(!r.is_error, "create_node failed: {}", r.first_text());
    serde_json::from_str(r.first_text()).unwrap_or(json!(null))
}

#[test]
fn edits_land_in_the_shadow_and_not_in_the_live_document() {
    let l = live();
    let before = l.doc().expect("doc").nodes().count();
    let mut sh = Shadow::capture(&l).expect("capture");
    create_rect(&mut sh, 10.0, 10.0, 100.0, 60.0);
    create_rect(&mut sh, 20.0, 20.0, 50.0, 50.0);

    assert_eq!(sh.doc().expect("shadow doc").nodes().count(), before + 2, "the shadow has both layers");
    assert_eq!(l.doc().expect("live doc").nodes().count(), before, "the live document is untouched");
    assert_eq!(l.undo_depth(), (0, 0), "and its undo history is untouched");
    assert!(sh.is_dirty());
    assert_eq!(sh.replay().len(), 2, "both creates were recorded");
}

#[test]
fn a_read_tool_observes_prior_shadow_edits() {
    let l = live();
    let mut sh = Shadow::capture(&l).expect("capture");
    let made = create_rect(&mut sh, 10.0, 10.0, 100.0, 60.0);
    let id = made["id"].clone();

    let read = sh.call(ToolPolicy::EmbeddedAgent, "get_node", &json!({"id": id, "depth": 1}));
    assert!(!read.is_error, "get_node failed: {}", read.first_text());
    let node: serde_json::Value = serde_json::from_str(read.first_text()).expect("json");
    assert_eq!(node["width"], 100.0, "the read sees the shadow's own edit: {node}");
    assert_eq!(sh.replay().len(), 1, "a read is not recorded");
}

#[test]
fn read_only_tools_are_not_recorded() {
    let l = live();
    let mut sh = Shadow::capture(&l).expect("capture");
    for (tool, args) in [("inspect_document", json!({"depth": 2})), ("list_commands", json!({"filter": "node.set"})), ("inspect_ui", json!({}))] {
        let r = sh.call(ToolPolicy::EmbeddedAgent, tool, &args);
        assert!(!r.is_error, "{tool} failed: {}", r.first_text());
    }
    assert!(sh.replay().is_empty(), "reads leave the replay empty: {:?}", sh.replay());
    assert!(!sh.is_dirty(), "and the turn is not dirty");
}

#[test]
fn a_denied_tool_never_executes_on_the_shadow() {
    let l = live();
    let mut sh = Shadow::capture(&l).expect("capture");
    let r = sh.call(ToolPolicy::EmbeddedAgent, "open_file", &json!({"path": "/tmp/x.figcraft"}));
    assert!(r.is_error);
    assert!(sh.replay().is_empty(), "a denied tool records nothing");
    assert!(!sh.is_dirty());
}

#[test]
fn the_replay_holds_canonical_command_ids_not_tool_aliases() {
    // The log must survive the tool catalog changing: a commit replays commands, not tool names.
    let l = live();
    let mut sh = Shadow::capture(&l).expect("capture");
    create_rect(&mut sh, 0.0, 0.0, 10.0, 10.0);
    let steps = sh.replay();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].command, "node.create", "the command id, not `create_node`");
    assert!(figcraft_engine::cmd::find_command(&steps[0].command).is_some(), "the id is in the registry");

    // The parameters are the command's, not the tool's wrapper.
    assert!(steps[0].params.get("width").is_some(), "the command's own parameters: {:?}", steps[0].params);
    assert!(steps[0].params.get("type").is_some());
}

#[test]
fn a_model_batch_expands_into_its_steps() {
    // A model that sends `batch` should not produce a nested batch at commit time.
    let l = live();
    let mut sh = Shadow::capture(&l).expect("capture");
    let r = sh.call(
        ToolPolicy::EmbeddedAgent,
        "batch",
        &json!({"commands": [
            {"command": "node.create", "params": {"type": "rectangle", "width": 10.0, "height": 10.0}},
            {"command": "node.create", "params": {"type": "ellipse", "width": 20.0, "height": 20.0}},
        ]}),
    );
    assert!(!r.is_error, "batch failed: {}", r.first_text());
    assert_eq!(sh.replay().len(), 2, "the inner commands are recorded: {:?}", sh.replay());
    assert!(sh.replay().iter().all(|s| s.command == "node.create"));
}

#[test]
fn a_nested_batch_is_refused() {
    let l = live();
    let mut sh = Shadow::capture(&l).expect("capture");
    let r = sh.call(ToolPolicy::EmbeddedAgent, "batch", &json!({"commands": [{"command": "command.batch", "params": {"commands": []}}]}));
    assert!(r.is_error);
}

#[test]
fn committing_applies_every_change_as_one_undo_step() {
    let mut l = live();
    let before_count = l.doc().expect("doc").nodes().count();
    let mut sh = Shadow::capture(&l).expect("capture");
    create_rect(&mut sh, 10.0, 10.0, 100.0, 60.0);
    create_rect(&mut sh, 30.0, 30.0, 50.0, 50.0);
    sh.call(ToolPolicy::EmbeddedAgent, "set_properties", &json!({"ids": [3], "props": {"fill": "#FF0000"}}));

    let depth_before = l.undo_depth();
    let commit = sh.commit(&mut l, "AI: make two rectangles").expect("commit");
    assert!(matches!(commit, Commit::Applied { steps: 3, .. }), "got {commit:?}");

    assert_eq!(l.doc().expect("doc").nodes().count(), before_count + 2, "both layers are in the live document");
    assert_eq!(l.undo_depth().0, depth_before.0 + 1, "exactly one undo step");

    // One undo restores the document exactly.
    l.execute("edit.undo", &json!({})).expect("undo");
    assert_eq!(l.doc().expect("doc").nodes().count(), before_count, "one undo removes the whole turn");

    // And redo brings it back.
    l.execute("edit.redo", &json!({})).expect("redo");
    assert_eq!(l.doc().expect("doc").nodes().count(), before_count + 2, "redo restores the whole turn");
}

#[test]
fn the_undo_step_is_named_after_the_request() {
    let mut l = live();
    let mut sh = Shadow::capture(&l).expect("capture");
    create_rect(&mut sh, 0.0, 0.0, 10.0, 10.0);
    sh.commit(&mut l, "AI: create a login card").expect("commit");
    let labels = l.undo_labels(1);
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0], "AI: create a login card", "the undo entry shows what was asked");
}

#[test]
fn a_turn_that_only_read_changes_nothing() {
    let mut l = live();
    let mut sh = Shadow::capture(&l).expect("capture");
    sh.call(ToolPolicy::EmbeddedAgent, "inspect_document", &json!({}));
    let depth = l.undo_depth();
    let commit = sh.commit(&mut l, "AI: look").expect("commit");
    assert_eq!(commit, Commit::Unchanged);
    assert_eq!(l.undo_depth(), depth, "no undo step for a read-only turn");
}

#[test]
fn a_stale_revision_is_refused_and_nothing_is_replayed() {
    let mut l = live();
    let mut sh = Shadow::capture(&l).expect("capture");
    create_rect(&mut sh, 0.0, 0.0, 10.0, 10.0);

    // A person edits the live document while the Agent works.
    l.execute("view.zoomTo", &json!({"zoom": 2.0})).ok();
    l.set_selection(Vec::new());
    let nodes_before = l.doc().expect("doc").nodes().count();

    let e = sh.commit(&mut l, "AI: stale").expect_err("a stale commit is refused");
    assert_eq!(e.kind, AgentErrorKind::Conflict, "the kind is Conflict, not a generic error");
    assert!(e.message.contains("changed"), "the message says why: {}", e.message);
    assert_eq!(l.doc().expect("doc").nodes().count(), nodes_before, "nothing was replayed");
}

#[test]
fn a_turn_that_failed_changes_nothing() {
    // A tool error stops the turn; the shadow is dropped without a commit.
    let l = live();
    let mut sh = Shadow::capture(&l).expect("capture");
    create_rect(&mut sh, 0.0, 0.0, 10.0, 10.0);
    let r = sh.call(ToolPolicy::EmbeddedAgent, "get_node", &json!({"id": 999_999}));
    assert!(r.is_error, "an unknown node is an error");

    let nodes_before = l.doc().expect("doc").nodes().count();
    drop(sh); // the turn was abandoned
    assert_eq!(l.doc().expect("doc").nodes().count(), nodes_before, "abandoning the shadow changes nothing");
    assert_eq!(l.undo_depth(), (0, 0));
}

#[test]
fn generated_ids_in_the_replay_match_the_shadow() {
    // The commit replays the same commands against the same document, so ids come out the same and
    // the recorded parameters stay valid.
    let mut l = live();
    let mut sh = Shadow::capture(&l).expect("capture");
    let first = create_rect(&mut sh, 0.0, 0.0, 10.0, 10.0);
    let second = create_rect(&mut sh, 20.0, 0.0, 10.0, 10.0);
    let shadow_ids = (first["id"].as_u64(), second["id"].as_u64());
    assert!(shadow_ids.0.is_some() && shadow_ids.1.is_some());

    sh.commit(&mut l, "AI: two").expect("commit");
    // Both layers exist in the live document, at the ids the shadow produced.
    for id in [shadow_ids.0.expect("id"), shadow_ids.1.expect("id")] {
        assert!(l.doc().expect("doc").node(figcraft_doc::NodeId(id)).is_some(), "layer {id} landed at the shadow's id");
    }
}

#[test]
fn the_undo_label_is_one_readable_line() {
    assert_eq!(undo_label("Create a login card"), "AI: Create a login card");
    // Newlines and runs of whitespace collapse: an undo entry is one line.
    assert_eq!(undo_label("make\n  a   card"), "AI: make a card");
    // A long request is shortened, not truncated mid-character.
    let long = "请给我画一个非常长的中文界面".repeat(20);
    let label = undo_label(&long);
    assert!(label.chars().count() < 80, "the label stays short: {} chars", label.chars().count());
    assert!(label.ends_with('…'), "it says it was shortened: {label}");
    assert!(label.starts_with("AI: "));
}

#[test]
fn a_manual_step_recorded_directly_also_replays() {
    // The runner may record a step itself (a command it ran outside a tool call).
    let mut l = live();
    let mut sh = Shadow::capture(&l).expect("capture");
    sh.record(ReplayStep { command: "node.create".into(), params: json!({"type": "rectangle", "width": 5.0, "height": 5.0}) });
    // A recorded step that never ran in the shadow still replays: the caller owns its correctness.
    assert!(sh.is_dirty());
    let nodes_before = sh.doc().expect("shadow doc").nodes().count();
    sh.commit(&mut l, "AI: direct").expect("commit");
    assert_eq!(l.doc().expect("doc").nodes().count(), nodes_before + 1, "the recorded command ran on commit");
}

#[test]
fn the_shadow_is_a_copy_not_a_reference() {
    // Editing the shadow must not mutate the live document through a shared Arc.
    let mut l = live();
    let page = l.page().expect("page");
    l.execute("node.create", &json!({"type": "rectangle", "parent": page, "width": 10.0, "height": 10.0})).expect("create");
    let live_count = l.doc().expect("doc").nodes().count();

    let mut sh = Shadow::capture(&l).expect("capture");
    sh.call(ToolPolicy::EmbeddedAgent, "set_properties", &json!({"ids": [page], "props": {"name": "Renamed in shadow"}}));
    assert_eq!(l.doc().expect("doc").nodes().count(), live_count);
    assert_ne!(l.doc().expect("doc").node(page).map(|n| n.name.clone()), Some("Renamed in shadow".to_string()), "the live page kept its name");
}

#[test]
fn an_immediately_repeated_call_is_not_run_twice() {
    // A model that repeats a call -- same tool, same arguments, straight after -- used to create the
    // same frame twice, and every child under both. The second call is answered from the first result
    // so the document keeps one copy.
    let mut s = Session::new();
    s.execute("file.new", &json!({})).expect("file.new");
    let mut shadow = Shadow::capture(&s).expect("capture");

    let args = json!({"type": "frame", "x": 10.0, "y": 10.0, "width": 100.0, "height": 100.0, "name": "Once"});
    let first = shadow.call(ToolPolicy::EmbeddedAgent, "create_node", &args);
    let second = shadow.call(ToolPolicy::EmbeddedAgent, "create_node", &args);

    assert_eq!(first.first_text(), second.first_text(), "the repeat reports the first result");
    assert_eq!(shadow.replay().len(), 1, "and records one command, not two");
}

#[test]
fn a_repeat_after_another_call_is_allowed() {
    // Only the *immediately* previous call is remembered: create, inspect, create the same thing again
    // is a different intent, and refusing it would break a legitimate repeat.
    let mut s = Session::new();
    s.execute("file.new", &json!({})).expect("file.new");
    let mut shadow = Shadow::capture(&s).expect("capture");

    let args = json!({"type": "rectangle", "x": 0.0, "y": 0.0, "width": 24.0, "height": 24.0, "name": "Cell"});
    shadow.call(ToolPolicy::EmbeddedAgent, "create_node", &args);
    shadow.call(ToolPolicy::EmbeddedAgent, "list_commands", &json!({}));
    shadow.call(ToolPolicy::EmbeddedAgent, "create_node", &args);

    assert_eq!(shadow.replay().len(), 2, "the second rectangle is real");
}

#[test]
fn a_different_argument_is_never_treated_as_a_repeat() {
    // Two frames of the same size at different places are two frames.
    let mut s = Session::new();
    s.execute("file.new", &json!({})).expect("file.new");
    let mut shadow = Shadow::capture(&s).expect("capture");

    shadow.call(ToolPolicy::EmbeddedAgent, "create_node", &json!({"type": "frame", "x": 0.0, "y": 0.0, "width": 50.0, "height": 50.0}));
    shadow.call(ToolPolicy::EmbeddedAgent, "create_node", &json!({"type": "frame", "x": 500.0, "y": 0.0, "width": 50.0, "height": 50.0}));
    assert_eq!(shadow.replay().len(), 2);
}
