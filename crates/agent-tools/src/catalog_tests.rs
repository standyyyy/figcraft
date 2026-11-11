//! The shared catalog's surface: MCP parity, policy, and argument rejection.

use serde_json::{Value, json};

use crate::catalog::{TOOL_NAMES, ToolPolicy, call_tool, tool_definitions};

/// The tool names the catalog offers, in `tools/list` order.
fn names(policy: ToolPolicy) -> Vec<String> {
    tool_definitions(policy).iter().filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_string)).collect()
}

#[test]
fn catalog_matches_the_existing_mcp_surface() {
    // These 23 tools, in this order, are the MCP surface: the 19 canvas tools MCP has always had,
    // then the 4 requirements tools M13 added. The shared catalog must not add, drop or reorder one
    // without the MCP protocol tests changing too.
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
        "list_requirements",
        "get_requirements",
        "upsert_requirements",
        "export_requirements_markdown",
        "render_requirements_markdown",
        "insert_icon",
        "list_icons",
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

/// A backend that answers commands by name, so dispatch is observable.
struct Commands {
    seen: Vec<(String, Value)>,
}

impl Commands {
    fn new() -> Self {
        Self { seen: Vec::new() }
    }
}

impl crate::backend::Backend for Commands {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.seen.push((method.to_string(), params));
        Ok(json!({"ok": true}))
    }
    fn has_ui(&self) -> bool {
        false
    }
    fn describe(&self) -> String {
        "commands".into()
    }
}

#[test]
fn the_requirements_tools_dispatch_to_commands() {
    // The tools are adapters: they never touch document fields, so undo, collaboration, MCP and the
    // UI all see one implementation.
    for (tool, params, want) in [
        ("list_requirements", json!({}), "requirements.list"),
        ("get_requirements", json!({"id": "PRD:1"}), "requirements.get"),
        ("export_requirements_markdown", json!({"title": "T"}), "requirements.exportMarkdown"),
    ] {
        let mut b = Commands::new();
        let r = call_tool(&mut b, ToolPolicy::ExternalMcp, tool, &params);
        assert!(!r.is_error, "{tool} should succeed: {}", r.first_text());
        let (method, sent) = b.seen.first().expect("a command ran");
        // Every requirement tool is an adapter over a command, sent through `engine.execute`.
        assert_eq!(method, "engine.execute");
        assert_eq!(sent.get("command").and_then(Value::as_str), Some(want), "{tool} must run {want}");
    }
}

#[test]
fn upsert_requirements_forwards_the_whole_structure() {
    let mut b = Commands::new();
    let args = json!({
        "title": "Checkout",
        "summary": "s",
        "goals": ["g"],
        "nonGoals": ["n"],
        "audiences": ["a"],
        "requirements": [{"title": "r", "priority": "must", "acceptanceCriteria": ["c"]}],
        "acceptanceCriteria": ["doc c"],
        "openQuestions": ["q"],
    });
    let r = call_tool(&mut b, ToolPolicy::EmbeddedAgent, "upsert_requirements", &args);
    assert!(!r.is_error, "upsert should succeed: {}", r.first_text());
    let (method, sent) = b.seen.first().expect("a command ran");
    assert_eq!(method, "engine.execute");
    assert_eq!(sent.get("command").and_then(Value::as_str), Some("requirements.upsert"));
    let params = sent.get("params").expect("the command's parameters");
    for key in ["title", "summary", "goals", "nonGoals", "audiences", "requirements", "acceptanceCriteria", "openQuestions"] {
        assert_eq!(params.get(key), args.get(key), "`{key}` is forwarded unchanged");
    }
}

#[test]
fn the_requirements_tools_are_on_both_surfaces() {
    // Reading and writing requirements is document-local; exporting to a path is not, so the Agent
    // gets the read-only renderer instead.
    for tool in ["list_requirements", "get_requirements", "upsert_requirements", "render_requirements_markdown"] {
        assert!(ToolPolicy::ExternalMcp.allows(tool), "{tool} stays available to external MCP");
        assert!(ToolPolicy::EmbeddedAgent.allows(tool), "{tool} is document-local, so the Agent gets it too");
    }
    assert!(ToolPolicy::ExternalMcp.allows("export_requirements_markdown"), "writing a file stays external");
    assert!(!ToolPolicy::EmbeddedAgent.allows("export_requirements_markdown"), "the Agent cannot write a file");
}

#[test]
fn upsert_requirements_declares_title_as_required() {
    // The schema tells a model `title` is needed; the command refuses without it (the engine's
    // `req_string`), so the two agree. This pins the declaration, which is what a model sees.
    let defs = tool_definitions(ToolPolicy::ExternalMcp);
    let upsert = defs.iter().find(|t| t.get("name").and_then(Value::as_str) == Some("upsert_requirements")).expect("upsert_requirements is defined");
    assert_eq!(upsert.pointer("/inputSchema/required"), Some(&json!(["title"])), "title is required: {upsert}");
    // Every property it documents is reachable, and unknown ones are refused before dispatch.
    let mut b = Commands::new();
    let r = call_tool(&mut b, ToolPolicy::EmbeddedAgent, "upsert_requirements", &json!({"nonsense": 1}));
    assert!(r.is_error);
    assert!(r.first_text().contains("unknown argument"), "got: {}", r.first_text());
    assert!(b.seen.is_empty(), "nothing reached the backend");
}

#[test]
fn export_returns_markdown_text_rather_than_a_json_wrapper() {
    // What a model reads should be the Markdown itself.
    struct Markdown;
    impl crate::backend::Backend for Markdown {
        fn call(&mut self, _method: &str, _params: Value) -> Result<Value, String> {
            Ok(json!({"markdown": "# A title\n\nbody", "bytes": 18}))
        }
        fn has_ui(&self) -> bool {
            false
        }
        fn describe(&self) -> String {
            "markdown".into()
        }
    }
    let mut b = Markdown;
    let r = call_tool(&mut b, ToolPolicy::ExternalMcp, "export_requirements_markdown", &json!({"id": "PRD:1"}));
    assert!(!r.is_error);
    assert_eq!(r.first_text(), "# A title\n\nbody", "the raw Markdown is the tool result");
}

// ---------- the embedded policy is default-deny on commands ----------

/// A backend that records every command it is asked to run.
struct CommandRecorder {
    ran: Vec<String>,
}

impl CommandRecorder {
    fn new() -> Self {
        Self { ran: Vec::new() }
    }
}

impl crate::backend::Backend for CommandRecorder {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        if method == "engine.execute"
            && let Some(cmd) = params.get("command").and_then(Value::as_str)
        {
            self.ran.push(cmd.to_string());
        }
        Ok(json!({"ok": true}))
    }
    fn has_ui(&self) -> bool {
        false
    }
    fn describe(&self) -> String {
        "recording".into()
    }
}

#[test]
fn the_embedded_agent_cannot_run_a_non_replayable_command() {
    // `run_command` forwards any command id, so a model that names `file.save` or `edit.undo` reaches
    // the engine. Those are not document-local replayable edits: a turn that saved a file or undid
    // someone's work could not be committed as one batch, and undoing inside a turn would corrupt the
    // history the turn is supposed to add one entry to.
    for denied in ["file.open", "file.save", "file.saveAs", "file.export", "file.close", "edit.undo", "edit.redo"] {
        let mut b = CommandRecorder::new();
        let r = call_tool(&mut b, ToolPolicy::EmbeddedAgent, "run_command", &json!({"command": denied}));
        assert!(r.is_error, "`{denied}` must be refused for the embedded Agent");
        assert!(b.ran.is_empty(), "`{denied}` reached the engine: {:?}", b.ran);
    }
}

#[test]
fn the_embedded_agent_cannot_run_a_namespaced_command() {
    // Namespaces whose whole family is outside the document: collaboration, plugins, UI input,
    // clipboard, branches, sharing.
    for denied in ["collab.join", "live.publish", "branch.review", "plugin.directory.install", "ui.pointer", "clipboard.copy"] {
        let mut b = CommandRecorder::new();
        let r = call_tool(&mut b, ToolPolicy::EmbeddedAgent, "run_command", &json!({"command": denied}));
        assert!(r.is_error, "`{denied}` must be refused");
        assert!(b.ran.is_empty(), "`{denied}` reached the engine");
    }
}

#[test]
fn a_batch_step_is_checked_too() {
    // The bypass through `batch`: a denied command hidden in a step must refuse the whole call.
    let mut b = CommandRecorder::new();
    let r = call_tool(
        &mut b,
        ToolPolicy::EmbeddedAgent,
        "batch",
        &json!({"commands": [
            {"command": "node.create", "params": {"type": "rectangle"}},
            {"command": "file.save", "params": {}},
        ]}),
    );
    assert!(r.is_error, "the batch must be refused");
    assert!(!b.ran.iter().any(|c| c == "file.save"), "the denied step ran: {:?}", b.ran);
}

#[test]
fn an_allowed_command_still_runs() {
    // The policy is a filter, not a blanket denial: the document-local commands still work.
    for allowed in ["node.create", "node.set", "edit.delete", "page.create", "requirements.upsert"] {
        let mut b = CommandRecorder::new();
        let r = call_tool(&mut b, ToolPolicy::EmbeddedAgent, "run_command", &json!({"command": allowed, "params": {}}));
        assert!(!r.is_error, "`{allowed}` should be allowed: {}", r.first_text());
        assert_eq!(b.ran, [allowed.to_string()], "`{allowed}` did not reach the engine");
    }
}

#[test]
fn an_unknown_command_is_refused_by_default() {
    // A command added later must not become Agent-accessible by accident: the default is deny.
    let mut b = CommandRecorder::new();
    let r = call_tool(&mut b, ToolPolicy::EmbeddedAgent, "run_command", &json!({"command": "some.futureCommand"}));
    assert!(r.is_error, "an unknown command is refused");
    assert!(b.ran.is_empty());
}

#[test]
fn every_engine_command_is_classified_explicitly() {
    // This is the guard against silent widening: when a command is added to the registry, this test
    // fails until it is classified for the embedded Agent. A command the policy has never heard of
    // defaults to denied, which is safe; this test makes that default visible rather than quiet.
    let all: Vec<&str> = figcraft_engine::all_commands().iter().map(|c| c.id).collect();
    assert!(!all.is_empty(), "the registry is readable");
    let mut unclassified = Vec::new();
    for id in &all {
        // `allows_command` answers for every id: allowed, or denied by the explicit rule.
        let allowed = crate::catalog::allows_command(ToolPolicy::EmbeddedAgent, id);
        // A command is "classified" when it is explicitly named either way. Denied-by-default is the
        // safe side, so only record the ones nobody has looked at.
        if !allowed && !crate::catalog::is_explicitly_denied(id) {
            unclassified.push(*id);
        }
    }
    assert!(
        unclassified.is_empty(),
        "{} commands reach the embedded Agent decision without an explicit rule: {:?}",
        unclassified.len(),
        &unclassified[..unclassified.len().min(20)]
    );
}

#[test]
fn the_embedded_agent_cannot_write_a_file_through_the_export_tool() {
    // `export_requirements_markdown` takes a path, which is file I/O the Agent must not do: a turn
    // that wrote a file could not be replayed as a document edit.
    let mut b = CommandRecorder::new();
    let r = call_tool(&mut b, ToolPolicy::EmbeddedAgent, "export_requirements_markdown", &json!({"id": "PRD:1", "path": "/tmp/out.md"}));
    assert!(r.is_error, "a path-taking export is refused for the embedded Agent");
    assert!(b.ran.is_empty(), "nothing ran");
}

#[test]
fn the_embedded_agent_can_render_markdown_without_a_path() {
    // Reading the Markdown is fine; only writing is not.
    assert!(ToolPolicy::EmbeddedAgent.allows("render_requirements_markdown"), "the read-only renderer is offered");
    assert!(!ToolPolicy::EmbeddedAgent.allows("export_requirements_markdown"), "the path-taking export is not");
}
