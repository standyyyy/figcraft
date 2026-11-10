//! The shared catalog's surface: MCP parity, policy, and argument rejection.

use serde_json::{Value, json};

use crate::catalog::{TOOL_NAMES, ToolPolicy, call_tool, tool_definitions};

/// The tool names the catalog offers, in `tools/list` order.
fn names(policy: ToolPolicy) -> Vec<String> {
    tool_definitions(policy).iter().filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_string)).collect()
}

#[test]
fn catalog_matches_the_existing_mcp_surface() {
    // These 19 tools, in this order, are what MCP clients have always seen. The shared catalog must
    // not add, drop or reorder one without the MCP protocol tests changing too.
    let expected = [
        "list_commands",
        "run_command",
        "batch",
        "create_node",
        "set_properties",
        "inspect_document",
        "get_node",
        "select",
        "export",
        "screenshot",
        "open_file",
        "save_file",
        "undo",
        "redo",
        "pointer",
        "key",
        "type_text",
        "inspect_ui",
        "wait_for_changes",
    ];
    assert_eq!(TOOL_NAMES, expected, "TOOL_NAMES drifted from the MCP surface");
    assert_eq!(names(ToolPolicy::ExternalMcp), expected, "ExternalMcp definitions drifted");
}

#[test]
fn every_definition_is_a_named_object_schema() {
    for t in tool_definitions(ToolPolicy::ExternalMcp) {
        let name = t.get("name").and_then(Value::as_str).unwrap_or_default();
        assert!(!name.is_empty(), "a tool has no name: {t}");
        assert!(t.get("description").and_then(Value::as_str).is_some(), "{name} has no description");
        assert_eq!(t.pointer("/inputSchema/type").and_then(Value::as_str), Some("object"), "{name} is not an object schema");
    }
}

#[test]
fn the_embedded_policy_offers_a_document_local_subset() {
    let embedded = names(ToolPolicy::EmbeddedAgent);
    let external = names(ToolPolicy::ExternalMcp);
    assert!(!embedded.is_empty());
    for n in &embedded {
        assert!(external.contains(n), "embedded tool `{n}` is not in the external surface");
    }
    // The tools a turn could not replay as one command.batch stay external-only.
    for denied in ["open_file", "save_file", "undo", "redo", "pointer", "key", "wait_for_changes"] {
        assert!(!embedded.iter().any(|n| n == denied), "`{denied}` must not be offered to the embedded Agent");
        assert!(external.iter().any(|n| n == denied), "`{denied}` stays on the external surface");
    }
}

#[test]
fn the_policy_decision_is_one_named_function() {
    // `allows` is the single place a surface decides what exists, so the UI never keeps its own
    // string list that could drift from the catalog.
    assert!(ToolPolicy::ExternalMcp.allows("open_file"));
    assert!(!ToolPolicy::EmbeddedAgent.allows("open_file"));
    assert!(ToolPolicy::EmbeddedAgent.allows("create_node"));
    assert!(!ToolPolicy::ExternalMcp.allows("not_a_tool"));
    assert!(!ToolPolicy::EmbeddedAgent.allows("not_a_tool"));
}

/// A backend that records calls and answers a fixed document, so dispatch is observable without a
/// real session.
struct Recording {
    calls: Vec<String>,
}

impl Recording {
    fn new() -> Self {
        Self { calls: Vec::new() }
    }
}

impl crate::backend::Backend for Recording {
    fn call(&mut self, method: &str, _params: Value) -> Result<Value, String> {
        self.calls.push(method.to_string());
        match method {
            "document.inspect" => Ok(json!({"currentPage": 1, "pages": [{"id": 1, "children": []}]})),
            _ => Ok(json!({})),
        }
    }
    fn has_ui(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        "recording".into()
    }
}

#[test]
fn a_denied_tool_is_refused_before_it_reaches_the_backend() {
    let mut b = Recording::new();
    let r = call_tool(&mut b, ToolPolicy::EmbeddedAgent, "open_file", &json!({"path": "/tmp/x.figcraft"}));
    assert!(r.is_error, "open_file must be refused for the embedded Agent");
    assert!(r.first_text().contains("not available"), "the refusal says why: {}", r.first_text());
    assert!(b.calls.is_empty(), "a refused tool must not reach the backend, but called {:?}", b.calls);
}

#[test]
fn an_allowed_tool_reaches_the_backend() {
    let mut b = Recording::new();
    let r = call_tool(&mut b, ToolPolicy::EmbeddedAgent, "inspect_document", &json!({}));
    assert!(!r.is_error, "inspect_document should work: {}", r.first_text());
    assert_eq!(b.calls, ["document.inspect"]);
}

#[test]
fn unknown_arguments_are_still_refused() {
    let mut b = Recording::new();
    let r = call_tool(&mut b, ToolPolicy::EmbeddedAgent, "inspect_document", &json!({"nonsense": 1}));
    assert!(r.is_error);
    assert!(r.first_text().contains("unknown argument"), "got: {}", r.first_text());
    assert!(b.calls.is_empty());
}

#[test]
fn an_unknown_tool_name_is_refused() {
    let mut b = Recording::new();
    let r = call_tool(&mut b, ToolPolicy::ExternalMcp, "definitely_not_a_tool", &json!({}));
    assert!(r.is_error);
    assert!(b.calls.is_empty());
}
