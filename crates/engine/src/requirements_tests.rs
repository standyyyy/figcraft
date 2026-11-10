//! Requirements commands: create, read, update, delete, export, undo, and id allocation.

use serde_json::json;

use crate::Session;

fn new_session() -> Session {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).expect("file.new");
    s
}

fn upsert(s: &mut Session, params: serde_json::Value) -> serde_json::Value {
    s.execute("requirements.upsert", &params).expect("requirements.upsert")
}

fn sample() -> serde_json::Value {
    json!({
        "title": "Checkout redesign",
        "summary": "Fewer steps on a phone.",
        "goals": ["One screen"],
        "nonGoals": ["New payment providers"],
        "audiences": ["Phone buyers"],
        "requirements": [{"title": "Remember the address", "detail": "Prefill it.", "priority": "must",
                          "acceptanceCriteria": ["It appears on return."]}],
        "acceptanceCriteria": ["Under 30 s."],
        "openQuestions": ["Gift orders?"],
    })
}

#[test]
fn creating_a_document_allocates_a_deterministic_id() {
    let mut s = new_session();
    let first = upsert(&mut s, sample());
    assert_eq!(first["id"], "PRD:1", "the first document gets PRD:1: {first}");
    let second = upsert(&mut s, json!({"title": "Search"}));
    assert_eq!(second["id"], "PRD:2", "numbers continue: {second}");
    assert_eq!(first["document"]["requirements"][0]["id"], "r-1", "requirement ids are allocated too");
}

#[test]
fn the_same_commands_produce_the_same_ids_twice() {
    // Replay determinism: ids come from the document, never from a clock or a random source.
    let run = || {
        let mut s = new_session();
        let a = upsert(&mut s, sample());
        let b = upsert(&mut s, json!({"title": "Search"}));
        (a["id"].clone(), b["id"].clone())
    };
    assert_eq!(run(), run());
    assert_eq!(run(), (json!("PRD:1"), json!("PRD:2")));
}

#[test]
fn listing_reports_the_documents_and_their_counts() {
    let mut s = new_session();
    upsert(&mut s, sample());
    let list = s.execute("requirements.list", &json!({})).expect("list");
    let docs = list["documents"].as_array().expect("documents");
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0]["title"], "Checkout redesign");
    assert_eq!(docs[0]["requirements"], 1, "one requirement");
    // The document's own criteria plus the requirement's own: 1 + 1.
    assert_eq!(docs[0]["acceptanceCriteria"], 2, "criteria are counted across the document");
    assert_eq!(list["max"], 32);
}

#[test]
fn reading_a_document_returns_it_with_markdown() {
    let mut s = new_session();
    let made = upsert(&mut s, sample());
    let id = made["id"].as_str().expect("id");

    let got = s.execute("requirements.get", &json!({"id": id})).expect("get");
    assert_eq!(got["document"]["title"], "Checkout redesign");
    let md = got["markdown"].as_str().expect("markdown");
    assert!(md.contains("# Checkout redesign"), "{md}");
    assert!(md.contains("## Requirements"), "{md}");
}

#[test]
fn an_exact_id_wins_over_a_title() {
    // A document titled like another document's id must still be reachable by id.
    let mut s = new_session();
    let a = upsert(&mut s, json!({"title": "PRD:2"}));
    assert_eq!(a["id"], "PRD:1");
    upsert(&mut s, json!({"title": "Second"}));

    let by_id = s.execute("requirements.get", &json!({"id": "PRD:1"})).expect("by id");
    assert_eq!(by_id["document"]["title"], "PRD:2", "the id lookup found the document titled `PRD:2`");

    let by_title = s.execute("requirements.get", &json!({"title": "Second"})).expect("by title");
    assert_eq!(by_title["document"]["title"], "Second");
}

#[test]
fn updating_keeps_the_id_and_replaces_the_content() {
    let mut s = new_session();
    let id = upsert(&mut s, sample())["id"].as_str().expect("id").to_string();

    let updated = upsert(&mut s, json!({"id": id, "title": "Checkout v2", "goals": ["Two screens"]}));
    assert_eq!(updated["id"], id, "the id does not change");
    assert_eq!(updated["document"]["title"], "Checkout v2");
    assert_eq!(updated["document"]["goals"][0], "Two screens");
    // A field left out is cleared, as a full replacement. An empty list is not written at all
    // (`skip_serializing_if`), so it comes back as a missing key rather than an empty array.
    let non_goals = &updated["document"]["nonGoals"];
    assert!(non_goals.is_null() || non_goals.as_array().is_some_and(Vec::is_empty), "nonGoals is cleared: {non_goals}");

    let list = s.execute("requirements.list", &json!({})).expect("list");
    assert_eq!(list["documents"].as_array().map(Vec::len), Some(1), "still one document");
}

#[test]
fn deleting_removes_the_document() {
    let mut s = new_session();
    let id = upsert(&mut s, sample())["id"].as_str().expect("id").to_string();
    let out = s.execute("requirements.delete", &json!({"id": id})).expect("delete");
    assert_eq!(out["id"], id);
    let list = s.execute("requirements.list", &json!({})).expect("list");
    assert_eq!(list["documents"].as_array().map(Vec::len), Some(0));
}

#[test]
fn a_missing_document_is_an_error_not_a_panic() {
    let mut s = new_session();
    assert!(s.execute("requirements.get", &json!({"id": "PRD:99"})).is_err());
    assert!(s.execute("requirements.get", &json!({"title": "Nope"})).is_err());
    assert!(s.execute("requirements.delete", &json!({"id": "PRD:99"})).is_err());
    assert!(s.execute("requirements.upsert", &json!({"id": "PRD:99", "title": "X"})).is_err());
    // Nothing was created by the refused calls.
    let list = s.execute("requirements.list", &json!({})).expect("list");
    assert_eq!(list["documents"].as_array().map(Vec::len), Some(0));
}

#[test]
fn an_invalid_document_is_refused_and_changes_nothing() {
    let mut s = new_session();
    // A blank title is refused.
    assert!(s.execute("requirements.upsert", &json!({"title": "   "})).is_err());
    // An unknown priority is refused, naming the problem.
    let err = s
        .execute("requirements.upsert", &json!({"title": "T", "requirements": [{"title": "r", "priority": "banana"}]}))
        .expect_err("bad priority");
    assert!(format!("{err:?}").contains("priority") || format!("{err}").contains("priority"), "the error names the field: {err:?}");

    let list = s.execute("requirements.list", &json!({})).expect("list");
    assert_eq!(list["documents"].as_array().map(Vec::len), Some(0), "a refused write changes nothing");
}

#[test]
fn overlong_text_is_refused() {
    let mut s = new_session();
    let long = "x".repeat(300);
    let err = s
        .execute("requirements.upsert", &json!({"title": long}))
        .expect_err("overlong title");
    assert!(format!("{err}").contains("limit"), "the error says what the limit is: {err}");
}

#[test]
fn one_undo_removes_the_whole_write() {
    let mut s = new_session();
    upsert(&mut s, sample());
    let list = s.execute("requirements.list", &json!({})).expect("list");
    assert_eq!(list["documents"].as_array().map(Vec::len), Some(1));

    s.execute("edit.undo", &json!({})).expect("undo");
    let after = s.execute("requirements.list", &json!({})).expect("list after undo");
    assert_eq!(after["documents"].as_array().map(Vec::len), Some(0), "one undo removes the write");

    s.execute("edit.redo", &json!({})).expect("redo");
    let back = s.execute("requirements.list", &json!({})).expect("list after redo");
    assert_eq!(back["documents"].as_array().map(Vec::len), Some(1), "redo brings it back");
}

#[test]
fn exporting_returns_the_markdown_without_a_path() {
    let mut s = new_session();
    upsert(&mut s, sample());
    let out = s.execute("requirements.exportMarkdown", &json!({"title": "Checkout redesign"})).expect("export");
    let md = out["markdown"].as_str().expect("markdown");
    assert!(md.contains("# Checkout redesign"));
    assert!(out["bytes"].as_u64().is_some_and(|b| b > 0));
    assert!(out.get("path").is_none(), "no path means no file");
}

#[test]
fn the_document_count_is_capped() {
    let mut s = new_session();
    let mut created = 0;
    for i in 0..40 {
        let r = s.execute("requirements.upsert", &json!({"title": format!("Doc {i}")}));
        if r.is_err() {
            break;
        }
        created += 1;
    }
    assert_eq!(created, 32, "the cap from figcraft_doc::MAX_DOCUMENTS applies");
    let list = s.execute("requirements.list", &json!({})).expect("list");
    assert_eq!(list["documents"].as_array().map(Vec::len), Some(32));
}

#[test]
fn priority_names_are_offered_for_a_model_to_use() {
    let mut s = new_session();
    let out = s.execute("requirements.priorityNames", &json!({})).expect("names");
    let names: Vec<&str> = out["priorities"].as_array().expect("array").iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(names, ["must", "should", "could", "wont"]);
}

#[test]
fn a_whole_document_round_trips_through_the_document_model() {
    let mut s = new_session();
    let made = upsert(&mut s, sample());
    let stored = s.doc().expect("doc").requirements.first().cloned().expect("stored");
    assert_eq!(stored.id, "PRD:1");
    assert_eq!(stored.title, "Checkout redesign");
    assert_eq!(stored.goals, ["One screen"]);
    assert_eq!(stored.requirements[0].title, "Remember the address");
    assert_eq!(stored.requirements[0].priority, figcraft_doc::Priority::Must);
    assert_eq!(made["document"]["requirements"][0]["acceptanceCriteria"][0], "It appears on return.");
}
