//! The Rig tool adapters: one per catalog tool, arguments unchanged, failures readable.

use figcraft_agent_tools::ToolPolicy;
use figcraft_engine::Session;
use serde_json::json;

use crate::shadow::Shadow;
use crate::tools::ShadowTools;

fn live() -> Session {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).expect("file.new");
    s
}

fn tools_for(policy: ToolPolicy) -> ShadowTools {
    let l = live();
    let sh = Shadow::capture(&l).expect("capture");
    ShadowTools::new(sh, policy)
}

#[test]
fn every_embedded_tool_is_advertised_once() {
    let tools = tools_for(ToolPolicy::EmbeddedAgent);
    let defs = tools.definitions();
    let names: Vec<String> = defs.iter().map(|d| d.name().to_string()).collect();

    // The embedded policy's set, in catalog order, each exactly once.
    let expected: Vec<String> = figcraft_agent_tools::tool_definitions(ToolPolicy::EmbeddedAgent)
        .iter()
        .filter_map(|d| d.get("name").and_then(serde_json::Value::as_str).map(str::to_string))
        .collect();
    assert_eq!(names, expected, "one adapter per catalog tool, in order");
    let unique: std::collections::HashSet<&String> = names.iter().collect();
    assert_eq!(unique.len(), names.len(), "no tool is advertised twice");

    // Nothing outside the embedded policy leaks in: file I/O, undo/redo and pointer input are for
    // external MCP only.
    for denied in ["open_file", "save_file", "undo", "redo", "pointer", "key", "wait_for_changes"] {
        assert!(!names.iter().any(|n| n == denied), "`{denied}` must not be advertised to the Agent");
    }
}

#[test]
fn the_external_policy_advertises_the_full_catalog() {
    let tools = tools_for(ToolPolicy::ExternalMcp);
    let names: Vec<String> = tools.definitions().iter().map(|d| d.name().to_string()).collect();
    assert_eq!(names.len(), figcraft_agent_tools::TOOL_NAMES.len(), "every catalog tool is advertised");
    for name in figcraft_agent_tools::TOOL_NAMES {
        assert!(names.iter().any(|n| n == name), "`{name}` is advertised");
    }
}

#[test]
fn a_call_reaches_the_catalog_with_its_arguments_unchanged() {
    let tools = tools_for(ToolPolicy::EmbeddedAgent);
    let r = tools.call("create_node", &json!({"type": "rectangle", "x": 10.0, "y": 20.0, "width": 100.0, "height": 50.0, "name": "Card"}));
    assert!(!r.is_error, "create_node failed: {}", r.first_text());
    assert_eq!(tools.replay_len(), 1, "the call ran as a command and was recorded");

    // The node exists with exactly the arguments given.
    let node: serde_json::Value = serde_json::from_str(r.first_text()).expect("json");
    assert_eq!(node["node"]["width"], 100.0);
    assert_eq!(node["node"]["height"], 50.0);
    assert_eq!(node["node"]["name"], "Card");
}

#[test]
fn a_tool_failure_is_readable_and_does_not_end_the_turn() {
    let tools = tools_for(ToolPolicy::EmbeddedAgent);
    // An argument the tool does not take.
    let r = tools.call("create_node", &json!({"type": "rectangle", "nonsense": 1}));
    assert!(r.is_error, "an unknown argument is refused");
    let text = crate::tools::tool_text_for_test(&r);
    assert!(text.starts_with("Error: "), "a failure is marked for the model: {text}");
    assert!(text.contains("unknown argument"), "and says what was wrong: {text}");

    // A missing node: still a tool result, not a panic.
    let r = tools.call("get_node", &json!({"id": 999_999}));
    assert!(r.is_error);
    assert!(crate::tools::tool_text_for_test(&r).contains("Error"));

    // The tools still work afterwards: one failure did not poison the shadow.
    let r = tools.call("create_node", &json!({"type": "rectangle", "width": 5.0, "height": 5.0}));
    assert!(!r.is_error, "the turn continues after a tool failure: {}", r.first_text());
}

#[test]
fn no_mcp_jsonrpc_wrapper_leaks_into_a_tool_result() {
    // A model reads content, not protocol: no `jsonrpc`, `id` or `result` envelope.
    let tools = tools_for(ToolPolicy::EmbeddedAgent);
    let r = tools.call("inspect_document", &json!({"depth": 1}));
    assert!(!r.is_error);
    let text = crate::tools::tool_text_for_test(&r);
    for leaked in ["\"jsonrpc\"", "\"result\":", "tools/call"] {
        assert!(!text.contains(leaked), "the result is content, not an envelope ({leaked}): {text}");
    }
    // It is the document, as JSON.
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("the result is JSON");
    assert!(parsed.get("pages").is_some(), "got {parsed}");
}

#[test]
fn a_denied_tool_is_not_advertised_and_calling_it_fails() {
    let tools = tools_for(ToolPolicy::EmbeddedAgent);
    // Even if a model guesses a name, the catalog refuses it before any command runs.
    let r = tools.call("open_file", &json!({"path": "/tmp/x.figcraft"}));
    assert!(r.is_error, "a guessed external-only name is refused");
    assert_eq!(tools.replay_len(), 0, "nothing ran");
}

#[test]
fn the_tool_set_keeps_catalog_order() {
    // Rig's definitions follow registration order, which is what a model sees.
    let tools = tools_for(ToolPolicy::EmbeddedAgent);
    let defs = tools.tool_set().tool_definitions();
    let names: Vec<String> = defs.into_iter().map(|d| d.name.into()).collect();
    let expected: Vec<String> = figcraft_agent_tools::tool_definitions(ToolPolicy::EmbeddedAgent)
        .iter()
        .filter_map(|d| d.get("name").and_then(serde_json::Value::as_str).map(str::to_string))
        .collect();
    assert_eq!(names, expected);
}

#[test]
fn a_screenshot_result_still_gives_a_model_something_to_read() {
    // An image-only result would be empty text; the adapter says so instead.
    let tools = tools_for(ToolPolicy::EmbeddedAgent);
    let r = tools.call("screenshot", &json!({"maxSize": 64}));
    if !r.is_error {
        let text = crate::tools::tool_text_for_test(&r);
        assert!(!text.is_empty(), "a model never gets an empty tool result");
    }
}
