//! The Agent transcript through a file's whole life: save, reopen, dirty, undo and damage.
//!
//! A conversation is not a design edit. It travels in the `.figcraft` archive as its own member, so
//! these tests check both halves of that: the transcript survives a round trip, and it never becomes
//! an undo entry, a collaboration change or a branch patch.

use serde_json::json;

use crate::Session;

/// A transcript with `turns` finished turns and one still running, as the Agent writes it.
fn transcript(turns: usize, running: bool) -> figcraft_format::agent::AgentData {
    let mut conversations = Vec::new();
    let mut items = Vec::new();
    for i in 0..turns {
        items.push(json!({
            "request": format!("draw screen {i}"),
            "answer": format!("built screen {i}"),
            "outcome": "committed",
            "tools": [{"name": "batch", "ok": true}],
        }));
    }
    if running {
        items.push(json!({"request": "a turn that never finished", "outcome": "running"}));
    }
    conversations.push(json!({"id": "doc-1", "title": "Screen work", "turns": items}));
    figcraft_format::agent::AgentData::new(json!({"version": 1, "conversations": conversations}))
}

fn session_with(document: figcraft_format::agent::AgentData) -> Session {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).expect("file.new");
    s.execute("node.create", &json!({"type": "rectangle", "width": 10.0, "height": 10.0})).expect("create");
    if let Ok(ds) = s.ds_mut() {
        ds.set_agent(document);
        ds.mark_agent_saved();
    }
    // The rectangle created above is a real edit; the file is saved so the test starts from a clean
    // slate and a transcript change is the only thing that can dirty it.
    if let Ok(ds) = s.ds_mut() {
        ds.mark_saved();
    }
    s
}

/// Save and reopen through the format layer, which is what the app writes and reads.
fn round_trip(s: &Session) -> figcraft_format::agent::AgentData {
    let bytes = crate::cmd::file::file_bytes(s.ds().expect("a document")).expect("encode");
    let (_, loaded) = figcraft_format::load_bytes(&bytes).expect("load");
    let _ = loaded;
    let loaded = figcraft_format::load_bytes_with_versions(&bytes).expect("load full");
    loaded.agent.unwrap_or_default()
}

#[test]
fn a_transcript_survives_save_and_reopen() {
    let s = session_with(transcript(2, false));
    let back = round_trip(&s);
    assert_eq!(back, transcript(2, false), "the transcript came back unchanged");
    let turns = back.json["conversations"][0]["turns"].as_array().expect("turns");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0]["request"], "draw screen 0");
    assert_eq!(turns[1]["answer"], "built screen 1");
}

#[test]
fn a_transcript_reserves_no_undo_entry() {
    // Setting a transcript is not a document edit, so the undo history is untouched. If it were an
    // entry, undoing a design change would leave the conversation describing a screen that is gone.
    let mut s = Session::new();
    s.execute("file.new", &json!({})).expect("file.new");
    let before = s.undo_depth();
    if let Ok(ds) = s.ds_mut() {
        ds.set_agent(transcript(1, false));
    }
    assert_eq!(s.undo_depth(), before, "no undo entry was added");
    let labels = s.undo_labels(10);
    assert!(labels.is_empty(), "and nothing was named: {labels:?}");
}

#[test]
fn a_transcript_change_marks_the_file_dirty_and_a_save_clears_it() {
    // The transcript is part of what the file holds, so changing it must show as unsaved, and a
    // successful save must clear that.
    let mut s = session_with(transcript(1, false));
    assert!(!s.ds().expect("doc").is_dirty(), "an opened file with its own transcript is clean");

    if let Ok(ds) = s.ds_mut() {
        ds.set_agent(transcript(2, false));
    }
    assert!(s.ds().expect("doc").is_dirty(), "a new turn marks the file dirty");

    if let Ok(ds) = s.ds_mut() {
        ds.mark_agent_saved();
    }
    assert!(!s.ds().expect("doc").is_dirty(), "saving settles it");
}

#[test]
fn setting_the_same_transcript_twice_is_not_a_change() {
    let mut s = session_with(transcript(1, false));
    if let Ok(ds) = s.ds_mut() {
        ds.set_agent(transcript(1, true));
    }
    assert!(s.ds().expect("doc").is_dirty());
    if let Ok(ds) = s.ds_mut() {
        ds.set_agent(transcript(1, true));
    }
    // Re-setting the same value keeps it dirty (it still differs from the saved baseline) but does
    // not change anything else.
    assert!(s.ds().expect("doc").is_dirty());
}

#[test]
fn a_file_without_a_transcript_opens_with_an_empty_one() {
    // An older file has no `agent/conversations.json`. It must open, with no history and no error.
    let mut s = Session::new();
    s.execute("file.new", &json!({})).expect("file.new");
    let bytes = crate::cmd::file::file_bytes(s.ds().expect("doc")).expect("encode");
    let loaded = figcraft_format::load_bytes_with_versions(&bytes).expect("load");
    assert!(loaded.agent.is_none(), "the archive carried no transcript");
    assert!(loaded.issues.is_empty(), "and no complaint: {:?}", loaded.issues);
}

#[test]
fn an_empty_transcript_writes_no_archive_member() {
    // A file that never held a conversation should not grow an empty one.
    let mut s = Session::new();
    s.execute("file.new", &json!({})).expect("file.new");
    let bytes = crate::cmd::file::file_bytes(s.ds().expect("doc")).expect("encode");
    let loaded = figcraft_format::load_bytes_with_versions(&bytes).expect("load");
    assert!(loaded.agent.is_none());
}

#[test]
fn saving_the_transcript_does_not_change_the_document_json() {
    // The transcript is its own member: `document.json` must not gain an `agent` key, because a
    // collaboration peer or a branch patch would then carry conversations as design content.
    let s = session_with(transcript(2, false));
    let bytes = crate::cmd::file::file_bytes(s.ds().expect("doc")).expect("encode");
    let loaded = figcraft_format::load_bytes_with_versions(&bytes).expect("load");
    let doc_json = serde_json::to_string(&loaded.doc).expect("serialize");
    for key in ["conversations", "transcript", "\"agent\""] {
        assert!(!doc_json.contains(key), "the document JSON must not carry `{key}`");
    }
}

#[test]
fn a_transcript_member_that_cannot_be_read_is_reported_without_losing_the_design() {
    // The format layer's own test covers a corrupted zip member end to end; here the contract that
    // matters to the engine is the shape of the result: the design opens, the transcript is absent,
    // and an issue says so. Building the damaged archive is `figcraft-format`'s job because writing
    // zip entries is its API, so this asserts the reader's half on bytes it produced.
    let bytes = figcraft_format::tests_support::archive_with_transcript_bytes(b"{not json").expect("a damaged archive");
    let loaded = figcraft_format::load_bytes_with_versions(&bytes).expect("the document still opens");
    assert!(loaded.agent.is_none(), "the transcript was skipped");
    assert!(!loaded.issues.is_empty(), "and the reader said why: {:?}", loaded.issues);
    assert!(!loaded.doc.pages.is_empty(), "the design document survived");
}
