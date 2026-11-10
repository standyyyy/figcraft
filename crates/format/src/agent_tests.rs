//! The Agent transcript archive member: caps, absence, damage, and old-reader compatibility.

use figcraft_doc::Document;
use serde_json::json;

use super::*;

fn plain() -> Document {
    let mut d = Document::new("f");
    let page = d.first_page().unwrap();
    d.create(page, figcraft_doc::NodeKind::Rectangle, "R", 0.0, 0.0, 10.0, 10.0).unwrap();
    d
}

fn transcript(turns: usize) -> agent::AgentData {
    let turns: Vec<serde_json::Value> =
        (0..turns).map(|i| json!({"turn": i, "request": format!("request {i}"), "answer": format!("answer {i}"), "outcome": "committed"})).collect();
    agent::AgentData::new(json!({"version": 1, "conversations": [{"id": "c1", "turns": turns}]}))
}

#[test]
fn a_file_without_a_transcript_writes_no_member() {
    // A file that never used the Agent must be byte-comparable to one saved before the feature.
    let bytes = save_bytes(&plain(), None).unwrap();
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    assert!(z.by_name(agent::AGENT_MEMBER).is_err(), "no transcript member");
}

#[test]
fn an_empty_transcript_writes_no_member() {
    let d = plain();
    let empty = agent::AgentData::new(json!({"version": 1, "conversations": []}));
    assert!(empty.is_empty());
    let bytes = save_bytes_full(&d, None, &[], &[], Some(&empty)).unwrap();
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    assert!(z.by_name(agent::AGENT_MEMBER).is_err(), "an empty transcript writes nothing");
}

#[test]
fn a_transcript_round_trips() {
    let d = plain();
    let data = transcript(3);
    let bytes = save_bytes_full(&d, None, &[], &[], Some(&data)).unwrap();
    let loaded = load_bytes_with_versions(&bytes).unwrap();
    assert!(loaded.issues.is_empty(), "no issues: {:?}", loaded.issues);
    let back = loaded.agent.expect("the transcript came back");
    assert_eq!(back, data);
    // And the document itself is unaffected.
    assert_eq!(loaded.doc.name, "f");
}

#[test]
fn a_transcript_does_not_enter_the_document() {
    // The transcript is file metadata: it must not appear in `document.json`, or a shared file would
    // carry the chat as if it were content.
    let d = plain();
    let bytes = save_bytes_full(&d, None, &[], &[], Some(&transcript(1))).unwrap();
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut json = String::new();
    std::io::Read::read_to_string(&mut z.by_name("document.json").unwrap(), &mut json).unwrap();
    assert!(!json.contains("conversations"), "the document holds no transcript: {json}");
    assert!(!json.contains("request 0"), "nor any turn text: {json}");
}

#[test]
fn an_old_reader_ignoring_the_member_still_opens_the_file() {
    // The point of a separate member: a reader from before the feature reads the document and
    // ignores what it does not know.
    let d = plain();
    let bytes = save_bytes_full(&d, None, &[], &[], Some(&transcript(2))).unwrap();
    // Strip the member, as an old reader effectively does.
    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut src = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
        let mut dst = zip::ZipWriter::new(&mut out);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for i in 0..src.len() {
            let mut f = src.by_index(i).unwrap();
            if f.name() == agent::AGENT_MEMBER {
                continue;
            }
            let name = f.name().to_string();
            let mut data = Vec::new();
            std::io::Read::read_to_end(&mut f, &mut data).unwrap();
            dst.start_file(name, opts).unwrap();
            std::io::Write::write_all(&mut dst, &data).unwrap();
        }
        dst.finish().unwrap();
    }
    let loaded = load_bytes_with_versions(&out.into_inner()).unwrap();
    assert_eq!(loaded.doc.name, "f", "an old reader gets the document");
    assert!(loaded.agent.is_none(), "and no transcript");
}

#[test]
fn a_malformed_transcript_is_skipped_with_an_issue() {
    // A damaged transcript must not stop the document from opening.
    let d = plain();
    let bytes = save_bytes_full(&d, None, &[], &[], Some(&transcript(1))).unwrap();
    let broken = replace_member(&bytes, agent::AGENT_MEMBER, b"{not json");
    let loaded = load_bytes_with_versions(&broken).unwrap();
    assert_eq!(loaded.doc.name, "f", "the document still opens");
    assert!(loaded.agent.is_none(), "the damaged transcript is dropped");
    assert!(loaded.issues.iter().any(|i| i.contains("transcript")), "and says so: {:?}", loaded.issues);
}

#[test]
fn a_non_utf8_transcript_is_skipped() {
    let d = plain();
    let bytes = save_bytes_full(&d, None, &[], &[], Some(&transcript(1))).unwrap();
    let broken = replace_member(&bytes, agent::AGENT_MEMBER, &[0xff, 0xfe, 0x00]);
    let loaded = load_bytes_with_versions(&broken).unwrap();
    assert!(loaded.agent.is_none());
    assert!(loaded.issues.iter().any(|i| i.contains("transcript")));
}

#[test]
fn an_oversized_transcript_is_refused_before_being_read() {
    // The cap is checked on the bytes, before parsing, so a huge member cannot be allocated.
    let big = vec![b' '; agent::MAX_AGENT_BYTES + 1];
    let e = agent::AgentData::from_bytes(&big).expect_err("over the cap");
    assert!(e.contains("limit"), "the error names the limit: {e}");
}

#[test]
fn writing_an_oversized_transcript_is_refused() {
    let huge = agent::AgentData::new(json!({"version": 1, "conversations": [{"turns": [{"answer": "x".repeat(agent::MAX_AGENT_BYTES + 100)}]}]}));
    let e = huge.to_bytes().expect_err("too big to write");
    assert!(format!("{e}").contains("limit"), "got {e}");
}

#[test]
fn too_many_conversations_are_refused() {
    let conversations: Vec<serde_json::Value> = (0..agent::MAX_CONVERSATIONS + 1).map(|i| json!({"id": format!("c{i}"), "turns": []})).collect();
    let data = agent::AgentData::new(json!({"version": 1, "conversations": conversations}));
    let e = agent::AgentData::from_bytes(&serde_json::to_vec(&data.json).unwrap()).expect_err("too many conversations");
    assert!(e.contains("limit"), "got {e}");
}

#[test]
fn too_many_turns_in_one_conversation_are_refused() {
    let turns: Vec<serde_json::Value> = (0..agent::MAX_TURNS + 1).map(|i| json!({"turn": i})).collect();
    let json = serde_json::json!({"version": 1, "conversations": [{"id": "c", "turns": turns}]});
    let e = agent::AgentData::from_bytes(&serde_json::to_vec(&json).unwrap()).expect_err("too many turns");
    assert!(e.contains("limit"), "got {e}");
}

#[test]
fn a_transcript_is_capped_by_decoded_text_not_just_bytes() {
    // Many small turns can still decode to a lot of text: the text cap is separate.
    let one = "x".repeat(40_000);
    let turns: Vec<serde_json::Value> = (0..agent::MAX_TURNS).map(|_| json!({"answer": one})).collect();
    let json = serde_json::json!({"version": 1, "conversations": [{"id": "c", "turns": turns}]});
    let bytes = serde_json::to_vec(&json).unwrap();
    let e = agent::AgentData::from_bytes(&bytes).expect_err("too much text");
    assert!(e.contains("limit"), "got {e}");
}

#[test]
fn unknown_future_fields_are_kept() {
    // A newer writer may add fields; this reader keeps them rather than dropping them, so a
    // round trip through an older build does not lose a future feature's data.
    let mut json = json!({"version": 9, "conversations": [{"id": "c", "turns": [], "futureField": {"a": 1}}], "somethingNew": true});
    let data = agent::AgentData::new(json.clone());
    let bytes = save_bytes_full(&plain(), None, &[], &[], Some(&data)).unwrap();
    let loaded = load_bytes_with_versions(&bytes).unwrap();
    let back = loaded.agent.expect("transcript");
    assert_eq!(back.json, json, "the payload is carried as it was");
    // (The test's own value is unchanged: the reader never normalizes a shape it does not own.)
    json["somethingNew"] = json!(false);
    assert_ne!(back.json, json);
}

#[test]
fn a_transcript_survives_a_recovery_copy() {
    // Recovery copies are whole files, so the transcript comes along.
    let bytes = save_bytes_full(&plain(), None, &[], &[], Some(&transcript(2))).unwrap();
    let recovered = load_bytes_with_versions(&bytes).unwrap();
    assert_eq!(recovered.agent, Some(transcript(2)));
    let again = save_bytes_full(&recovered.doc, None, &[], &[], recovered.agent.as_ref()).unwrap();
    let back = load_bytes_with_versions(&again).unwrap();
    assert_eq!(back.agent, Some(transcript(2)), "saving a recovered file keeps the transcript");
}

#[test]
fn a_transcript_does_not_disturb_the_version_history() {
    let d = plain();
    let version = Version::new("v1", "Before", "", 0, std::sync::Arc::new(d.clone()));
    let bytes = save_bytes_full(&d, None, &[version], &[], Some(&transcript(1))).unwrap();
    let loaded = load_bytes_with_versions(&bytes).unwrap();
    assert_eq!(loaded.versions.len(), 1, "the version is still there");
    assert!(loaded.agent.is_some(), "and so is the transcript");
}

/// Replace one archive member's bytes, keeping the rest.
fn replace_member(bytes: &[u8], name: &str, replacement: &[u8]) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut src = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut dst = zip::ZipWriter::new(&mut out);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for i in 0..src.len() {
            let mut f = src.by_index(i).unwrap();
            let member = f.name().to_string();
            let mut data = Vec::new();
            std::io::Read::read_to_end(&mut f, &mut data).unwrap();
            if member == name {
                data = replacement.to_vec();
            }
            dst.start_file(member, opts).unwrap();
            std::io::Write::write_all(&mut dst, &data).unwrap();
        }
        dst.finish().unwrap();
    }
    out.into_inner()
}
