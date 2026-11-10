//! Requirements documents through MCP: create, read, export, edit and undo.

use crate::common;
use common::Client;
use serde_json::{Value, json};

fn prd() -> Value {
    json!({
        "title": "Checkout redesign",
        "summary": "Fewer steps on a phone.",
        "goals": ["One screen"],
        "nonGoals": ["New payment providers"],
        "audiences": ["Phone buyers"],
        "requirements": [
            {"title": "Remember the address", "detail": "Prefill it.", "priority": "must",
             "acceptanceCriteria": ["It appears on return."]},
            {"title": "Keep the cart visible", "priority": "should"}
        ],
        "acceptanceCriteria": ["Checkout completes in under 30 s."],
        "openQuestions": ["What about gift orders?"]
    })
}

#[test]
fn an_agent_creates_reads_and_exports_a_requirements_document() {
    let mut c = Client::new();
    let made = c.ok("upsert_requirements", prd());
    assert_eq!(made["id"], "PRD:1", "the document allocates the id: {made}");
    assert_eq!(made["document"]["title"], "Checkout redesign");
    assert_eq!(made["document"]["requirements"][0]["id"], "r-1", "requirement ids are allocated");

    // The listing sees it.
    let list = c.ok("list_requirements", json!({}));
    let docs = list["documents"].as_array().expect("documents");
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0]["id"], "PRD:1");

    // Reading it back gives the structure and its Markdown.
    let got = c.call("get_requirements", json!({"id": "PRD:1"}));
    let text = got["content"][0]["text"].as_str().expect("text");
    let parsed: Value = serde_json::from_str(text).expect("json");
    assert_eq!(parsed["document"]["title"], "Checkout redesign");
    let md = parsed["markdown"].as_str().expect("markdown");
    assert!(md.contains("# Checkout redesign"), "{md}");
    assert!(md.contains("### Remember the address"), "{md}");

    // Export without a path returns the Markdown text itself, not a JSON wrapper.
    let exported = c.call("export_requirements_markdown", json!({"id": "PRD:1"}));
    let md = exported["content"][0]["text"].as_str().expect("markdown text");
    assert!(!exported["isError"].as_bool().unwrap_or(false), "export should succeed: {md}");
    assert!(md.starts_with("# Checkout redesign"), "a model reads the Markdown: {md}");
    assert!(md.contains("_Priority: must_"), "{md}");
    // With a path it writes a file instead.
    let dir = std::env::temp_dir().join(format!("figcraft-prd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let out = dir.join("prd.md");
    let out_str = out.to_str().expect("path");
    let written = c.run("requirements.exportMarkdown", json!({"id": "PRD:1", "path": out_str}));
    assert_eq!(written["path"], out_str);
    let on_disk = std::fs::read_to_string(&out).expect("the file was written");
    assert!(on_disk.starts_with("# Checkout redesign"), "the file holds the Markdown: {on_disk}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn one_undo_removes_the_whole_write() {
    let mut c = Client::new();
    c.ok("upsert_requirements", prd());
    assert_eq!(c.ok("list_requirements", json!({}))["documents"].as_array().map(Vec::len), Some(1));

    c.ok("run_command", json!({"command": "edit.undo", "params": {}}));
    assert_eq!(c.ok("list_requirements", json!({}))["documents"].as_array().map(Vec::len), Some(0), "one undo removes the write");

    c.ok("run_command", json!({"command": "edit.redo", "params": {}}));
    assert_eq!(c.ok("list_requirements", json!({}))["documents"].as_array().map(Vec::len), Some(1), "redo restores it");
}

#[test]
fn the_command_surface_is_reachable_through_run_command_too() {
    // The tools are adapters; the commands behind them are the same ones every surface uses.
    let mut c = Client::new();
    let made = c.run("requirements.upsert", prd());
    assert_eq!(made["id"], "PRD:1");
    let listed = c.run("requirements.list", json!({}));
    assert_eq!(listed["documents"].as_array().map(Vec::len), Some(1));
    let exported = c.run("requirements.exportMarkdown", json!({"id": "PRD:1"}));
    assert!(exported["markdown"].as_str().is_some_and(|m| m.contains("# Checkout redesign")));
}

#[test]
fn the_requirements_tools_are_listed_with_their_schemas() {
    let mut c = Client::new();
    let list = c.rpc("tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().expect("tools");
    for name in ["list_requirements", "get_requirements", "upsert_requirements", "export_requirements_markdown"] {
        let t = tools.iter().find(|t| t["name"] == name).unwrap_or_else(|| panic!("{name} is listed"));
        assert_eq!(t["inputSchema"]["type"], "object", "{name} has an object schema");
        assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()), "{name} has a description");
    }
    let upsert = tools.iter().find(|t| t["name"] == "upsert_requirements").expect("upsert");
    assert_eq!(upsert["inputSchema"]["required"], json!(["title"]), "upsert requires a title");
}

#[test]
fn an_invalid_requirements_write_is_refused_with_a_readable_error() {
    let mut c = Client::new();
    // The command refuses a blank title, so nothing is created.
    c.err("upsert_requirements", json!({"title": "   "}));
    // An unknown priority is named.
    let e = c.err("upsert_requirements", json!({"title": "T", "requirements": [{"title": "r", "priority": "banana"}]}));
    assert!(e.contains("priority"), "the error names the field: {e}");
    // Reading something that isn't there is an error, not a panic.
    c.err("get_requirements", json!({"id": "PRD:99"}));
    assert_eq!(c.ok("list_requirements", json!({}))["documents"].as_array().map(Vec::len), Some(0), "a refused write leaves the document unchanged");
}

#[test]
fn updating_a_document_keeps_its_id() {
    let mut c = Client::new();
    c.ok("upsert_requirements", prd());
    let updated = c.ok("upsert_requirements", json!({"id": "PRD:1", "title": "Checkout v2"}));
    assert_eq!(updated["id"], "PRD:1");
    assert_eq!(updated["document"]["title"], "Checkout v2");
    assert_eq!(c.ok("list_requirements", json!({}))["documents"].as_array().map(Vec::len), Some(1), "still one document");
}

#[test]
fn deleting_removes_it() {
    let mut c = Client::new();
    c.ok("upsert_requirements", prd());
    let out = c.run("requirements.delete", json!({"id": "PRD:1"}));
    assert_eq!(out["id"], "PRD:1");
    assert_eq!(c.ok("list_requirements", json!({}))["documents"].as_array().map(Vec::len), Some(0));
}

#[test]
fn requirements_survive_a_save_and_reopen() {
    let dir = std::env::temp_dir().join(format!("figcraft-req-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("prd.figcraft");
    let path_str = path.to_str().expect("path");

    let mut c = Client::new();
    c.ok("upsert_requirements", prd());
    c.ok("run_command", json!({"command": "file.save", "params": {"path": path_str}}));

    // Reopen through a second headless client and read the document back.
    let mut c2 = Client::new();
    c2.ok("run_command", json!({"command": "file.open", "params": {"path": path_str}}));
    let got = c2.run("requirements.get", json!({"id": "PRD:1"}));
    assert_eq!(got["document"]["title"], "Checkout redesign", "requirements survive save and reopen");
    assert_eq!(got["document"]["requirements"][1]["title"], "Keep the cart visible");

    std::fs::remove_dir_all(&dir).ok();
}
