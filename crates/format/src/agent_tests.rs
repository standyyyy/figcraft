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

// ---------- hostile archives ----------

/// Every transcript-local failure still opens the document, without the transcript and with one
/// actionable issue. None of these may panic, and none may allocate the declared size.
fn opens_without_transcript(bytes: &[u8], what: &str) -> Vec<String> {
    let loaded = crate::load_bytes_with_versions(bytes).unwrap_or_else(|e| panic!("{what}: the document must still open, got {e}"));
    assert!(loaded.agent.is_none(), "{what}: the transcript must be skipped");
    assert!(!loaded.issues.is_empty(), "{what}: an issue must say why");
    assert!(!loaded.doc.pages.is_empty(), "{what}: the design document survived");
    loaded.issues
}

#[test]
fn a_transcript_that_declares_an_oversize_is_skipped_before_reading() {
    // The declared size is checked first: a member claiming to be enormous is refused without the
    // reader allocating anything for it.
    let bytes = crate::tests_support::archive_with_transcript_bytes(&[b'x'; 64]).expect("archive");
    // Rebuild with a declared oversize by hiding the real bytes behind a huge entry is not possible
    // through the zip API without writing them, so this covers the reachable half: a member larger
    // than the limit in fact.
    let big = vec![b'{'; crate::agent::MAX_AGENT_BYTES + 1];
    let bytes_big = crate::tests_support::archive_with_transcript_bytes(&big).expect("archive");
    let issues = opens_without_transcript(&bytes_big, "an oversized member");
    assert!(issues.iter().any(|i| i.contains("limit")), "the issue names the limit: {issues:?}");
    // And the small malformed one is refused as malformed, not as oversize.
    let issues = opens_without_transcript(&bytes, "a malformed member");
    assert!(issues.iter().any(|i| i.contains("JSON") || i.contains("skipped")), "got {issues:?}");
}

#[test]
fn a_transcript_of_exactly_the_limit_is_read() {
    // The boundary is inclusive: a member at the limit is data, one byte over is refused. This is why
    // the read asks for limit + 1 bytes rather than trusting the header.
    let mut json = String::from("{\"version\":1,\"conversations\":[],\"pad\":\"");
    let tail = "\"}";
    let pad = crate::agent::MAX_AGENT_BYTES - json.len() - tail.len();
    json.push_str(&"a".repeat(pad));
    json.push_str(tail);
    assert_eq!(json.len(), crate::agent::MAX_AGENT_BYTES, "exactly at the limit");

    let bytes = crate::tests_support::archive_with_transcript_bytes(json.as_bytes()).expect("archive");
    let loaded = crate::load_bytes_with_versions(&bytes).expect("the document opens");
    assert!(loaded.agent.is_some(), "a member exactly at the limit is read");
}

#[test]
fn a_non_utf8_transcript_in_the_archive_is_skipped() {
    let bytes = crate::tests_support::archive_with_transcript_bytes(&[0xFF, 0xFE, 0x00]).expect("archive");
    let issues = opens_without_transcript(&bytes, "non-UTF-8");
    assert!(issues.iter().any(|i| i.contains("UTF-8")), "the issue says so: {issues:?}");
}

#[test]
fn a_truncated_transcript_is_skipped() {
    let bytes = crate::tests_support::archive_with_transcript_bytes(b"{\"version\":1,\"conversa").expect("archive");
    opens_without_transcript(&bytes, "a truncated member");
}

#[test]
fn a_transcript_with_too_many_conversations_is_skipped() {
    // A file from a hand-edit or a hostile writer: the cap is enforced on read, not only on write.
    let many: Vec<serde_json::Value> = (0..crate::agent::MAX_CONVERSATIONS + 1).map(|i| serde_json::json!({"id": format!("c{i}")})).collect();
    let json = serde_json::json!({"version": 1, "conversations": many}).to_string();
    let bytes = crate::tests_support::archive_with_transcript_bytes(json.as_bytes()).expect("archive");
    let issues = opens_without_transcript(&bytes, "too many conversations");
    assert!(issues.iter().any(|i| i.contains("conversation")), "got {issues:?}");
}

#[test]
fn a_transcript_with_too_many_turns_is_skipped() {
    let turns: Vec<serde_json::Value> = (0..crate::agent::MAX_TURNS + 1).map(|i| serde_json::json!({"turn": i})).collect();
    let json = serde_json::json!({"version": 1, "conversations": [{"id": "c", "turns": turns}]}).to_string();
    let bytes = crate::tests_support::archive_with_transcript_bytes(json.as_bytes()).expect("archive");
    opens_without_transcript(&bytes, "too many turns");
}

#[test]
fn a_transcript_with_unknown_future_fields_round_trips() {
    // A newer writer adds a field this build does not know. It must survive a save, so a downgrade
    // does not silently delete someone's data.
    let json = serde_json::json!({
        "version": 1,
        "futureFeature": {"nested": [1, 2, 3]},
        "conversations": [{"id": "c1", "turns": [], "alsoNew": true}],
    })
    .to_string();
    let bytes = crate::tests_support::archive_with_transcript_bytes(json.as_bytes()).expect("archive");
    let loaded = crate::load_bytes_with_versions(&bytes).expect("opens");
    let agent = loaded.agent.expect("the transcript is read");
    assert_eq!(agent.json["futureFeature"]["nested"], serde_json::json!([1, 2, 3]), "unknown fields are kept");

    // Write it back and read again: the unknown data is still there.
    let out = crate::save_bytes_full(&loaded.doc, None, &[], &[], Some(&agent)).expect("save");
    let again = crate::load_bytes_with_versions(&out).expect("opens");
    let agent2 = again.agent.expect("read");
    assert_eq!(agent2.json["futureFeature"]["nested"], serde_json::json!([1, 2, 3]), "and survive a round trip");
    assert_eq!(agent2.json["conversations"][0]["alsoNew"], serde_json::json!(true));
}

#[test]
fn a_normal_transcript_round_trips_unchanged() {
    // The happy path, so the hardening above is not achieved by refusing everything.
    let json = serde_json::json!({"version": 1, "conversations": [{"id": "c1", "turns": [{"turn": 1, "request": "draw"}]}]});
    let data = crate::agent::AgentData::new(json.clone());
    let bytes = crate::tests_support::archive_with_transcript_bytes(&serde_json::to_vec(&json).expect("json")).expect("archive");
    let loaded = crate::load_bytes_with_versions(&bytes).expect("opens");
    assert_eq!(loaded.agent.expect("read"), data, "read back exactly");
    assert!(loaded.issues.is_empty(), "and no complaint: {:?}", loaded.issues);
}

#[test]
fn a_file_without_the_member_has_no_transcript_and_no_issue() {
    // An older file: no member at all is not damage.
    let mut doc = figcraft_embedded_agent_test_doc();
    doc.repair();
    let bytes = crate::save_bytes_full(&doc, None, &[], &[], None).expect("save");
    let loaded = crate::load_bytes_with_versions(&bytes).expect("opens");
    assert!(loaded.agent.is_none());
    assert!(loaded.issues.is_empty(), "nothing to complain about: {:?}", loaded.issues);
}

/// A minimal document for the tests above.
fn figcraft_embedded_agent_test_doc() -> crate::Document {
    let mut doc = crate::Document::default();
    doc.repair();
    doc
}
