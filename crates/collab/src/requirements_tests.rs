//! Requirements documents sync as keyed document entries, like styles and comments.

use figcraft_doc::{Document, Priority, Requirement, RequirementsDocument};

use crate::change::{Change, DocProp, Entry};
use crate::replica::Mode;
use crate::{Replica, diff};

fn doc_with(id: &str, title: &str) -> (Document, RequirementsDocument) {
    let mut doc = Document::new("File");
    let mut prd = RequirementsDocument::new(id, title);
    prd.goals.push("A goal".into());
    prd.requirements.push(Requirement { id: "r-1".into(), title: "Do the thing".into(), priority: Priority::Must, ..Requirement::default() });
    doc.requirements.push(prd.clone());
    (doc, prd)
}

/// Diff an edited document against the replica it was made from.
fn changes(base: &Document, edited: &Document) -> Vec<Change> {
    diff(&Replica::new(base.clone()), edited).expect("diff").changes
}

#[test]
fn adding_a_requirements_document_diffs_to_one_put() {
    let a = Document::new("File");
    let (b, _) = doc_with("PRD:1", "Checkout");
    let changes = changes(&a, &b);
    assert!(
        changes.iter().any(|c| matches!(c, Change::Put { entry: Entry::Requirements(k), .. } if k == "PRD:1")),
        "the new document syncs as a keyed put: {changes:?}"
    );
    // A document appended at the end needs no separate order write: the puts and removes leave
    // exactly that order. (`List::diff` only writes the order when they would not.)
    assert!(
        !changes.iter().any(|c| matches!(c, Change::SetDoc { prop: DocProp::RequirementsOrder, .. })),
        "an append needs no order change: {changes:?}"
    );
}

#[test]
fn editing_one_document_syncs_that_entry_only() {
    let (a, _) = doc_with("PRD:1", "Checkout");
    let (mut b, _) = doc_with("PRD:1", "Checkout");
    b.requirements.push(RequirementsDocument::new("PRD:2", "Search"));
    b.requirements[0].summary = "Now with a summary".into();

    let changes = changes(&a, &b);
    let mut puts: Vec<&str> = changes
        .iter()
        .filter_map(|c| match c {
            Change::Put { entry: Entry::Requirements(k), .. } => Some(k.as_str()),
            _ => None,
        })
        .collect();
    puts.sort_unstable();
    assert_eq!(puts, ["PRD:1", "PRD:2"], "only the changed and the new document ship");
    // The unchanged first document's payload is the edited one; the second is new.
    assert!(
        !changes.iter().any(|c| matches!(c, Change::SetDoc { prop: DocProp::Requirements, .. })),
        "a per-entry change should not ship the whole list: {changes:?}"
    );
}

#[test]
fn removing_a_document_diffs_to_one_remove() {
    let (mut a, _) = doc_with("PRD:1", "Checkout");
    a.requirements.push(RequirementsDocument::new("PRD:2", "Search"));
    let (b, _) = doc_with("PRD:1", "Checkout");

    let changes = changes(&a, &b);
    assert!(
        changes.iter().any(|c| matches!(c, Change::Remove { entry: Entry::Requirements(k) } if k == "PRD:2")),
        "the removal syncs as a keyed remove: {changes:?}"
    );
}

#[test]
fn a_requirements_change_applies_to_the_other_replica() {
    let (a, _) = doc_with("PRD:1", "Checkout");
    let (mut b, _) = doc_with("PRD:1", "Checkout");
    b.requirements[0].title = "Checkout v2".into();
    b.requirements[0].open_questions.push("Gift orders?".into());

    let d = diff(&Replica::new(a), &b).expect("diff");
    let mut peer = d.replica;
    for c in &d.changes {
        peer.apply(std::slice::from_ref(c), Mode::Strict).expect("apply");
    }
    assert_eq!(peer.document().requirements, b.requirements, "the peer holds the same requirements");
}

#[test]
fn concurrent_edits_to_different_documents_both_land() {
    let (base, _) = doc_with("PRD:1", "One");
    let mut left = base.clone();
    left.requirements.push(RequirementsDocument::new("PRD:2", "Left's document"));
    let mut right = base.clone();
    right.requirements.push(RequirementsDocument::new("PRD:3", "Right's document"));

    let left_changes = changes(&base, &left);
    let right_changes = changes(&base, &right);

    let mut peer = Replica::new(base);
    for c in &left_changes {
        peer.apply(std::slice::from_ref(c), Mode::Strict).expect("apply left");
    }
    for c in &right_changes {
        peer.apply(std::slice::from_ref(c), Mode::Strict).expect("apply right");
    }
    let ids: Vec<&str> = peer.document().requirements.iter().map(|r| r.id.as_str()).collect();
    assert!(ids.contains(&"PRD:1"));
    assert!(ids.contains(&"PRD:2"), "left's document survived: {ids:?}");
    assert!(ids.contains(&"PRD:3"), "right's document survived: {ids:?}");
}

#[test]
fn two_edits_to_the_same_document_leave_it_consistent() {
    // The same entry written twice stays one entry with one id: the keyed list never duplicates.
    let (base, _) = doc_with("PRD:1", "Checkout");
    let mut left = base.clone();
    left.requirements[0].summary = "left".into();
    let mut right = base.clone();
    right.requirements[0].summary = "right".into();

    let mut peer = Replica::new(base);
    for c in &changes(&left.clone(), &left) {
        peer.apply(std::slice::from_ref(c), Mode::Strict).expect("apply left");
    }
    for c in &changes(&right.clone(), &right) {
        peer.apply(std::slice::from_ref(c), Mode::Strict).expect("apply right");
    }
    let docs = &peer.document().requirements;
    assert_eq!(docs.len(), 1, "one document, not two: {docs:?}");
    assert_eq!(docs[0].id, "PRD:1");
}

#[test]
fn an_unchanged_file_syncs_nothing() {
    let mut a = Document::new("File");
    let page = a.first_page().expect("page");
    a.create(page, figcraft_doc::NodeKind::Rectangle, "R", 0.0, 0.0, 10.0, 10.0).expect("create");
    let b = a.clone();
    let changes = changes(&a, &b);
    assert!(changes.is_empty(), "an unchanged file syncs nothing: {changes:?}");
}

#[test]
fn requirements_do_not_leak_into_an_unrelated_diff() {
    // A document-level change that is not about requirements must not ship them.
    let (a, _) = doc_with("PRD:1", "Checkout");
    let mut b = a.clone();
    b.name = "Renamed file".into();

    let changes = changes(&a, &b);
    assert!(
        changes.iter().all(|c| !matches!(c, Change::Put { entry: Entry::Requirements(_), .. })),
        "renaming the file must not resend requirements: {changes:?}"
    );
    assert!(changes.iter().any(|c| matches!(c, Change::SetDoc { prop: DocProp::Name, .. })), "only the name changed: {changes:?}");
}
