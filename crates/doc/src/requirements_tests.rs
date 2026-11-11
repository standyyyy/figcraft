//! Requirements documents: caps, round-trip, and Markdown escaping.

use super::*;

fn sample() -> RequirementsDocument {
    RequirementsDocument {
        id: "prd-1".into(),
        title: "Checkout redesign".into(),
        summary: "Make paying on a phone fewer steps.".into(),
        goals: vec!["One-screen checkout".into()],
        non_goals: vec!["New payment providers".into()],
        audiences: vec!["Phone buyers".into()],
        requirements: vec![Requirement {
            id: "r-1".into(),
            title: "Remember the last address".into(),
            detail: "Prefill from the previous order.".into(),
            priority: Priority::Must,
            acceptance_criteria: vec!["The saved address appears on return.".into()],
            ..Default::default()
        }],
        acceptance_criteria: vec!["Checkout completes in under 30 s.".into()],
        open_questions: vec!["What about gift orders?".into()],
        ..Default::default()
    }
}

#[test]
fn a_populated_document_round_trips() {
    let d = sample();
    let json = serde_json::to_string(&d).expect("serialize");
    let back: RequirementsDocument = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(d, back);
}

#[test]
fn an_empty_document_serializes_to_no_fields_beyond_its_identity() {
    // Empty requirements must stay compact in an existing file: defaults are skipped.
    let d = RequirementsDocument::new("prd-1", "Title");
    let v = serde_json::to_value(&d).expect("serialize");
    let obj = v.as_object().expect("object");
    assert_eq!(obj.len(), 2, "only id and title should be written: {v}");
    assert_eq!(obj.get("title").and_then(|t| t.as_str()), Some("Title"));
}

#[test]
fn defaults_are_read_from_a_document_that_omits_them() {
    let v = serde_json::json!({"id": "prd-1", "title": "Only a title"});
    let d: RequirementsDocument = serde_json::from_value(v).expect("deserialize");
    assert_eq!(d, RequirementsDocument::new("prd-1", "Only a title"));
    assert!(d.goals.is_empty());
    assert!(d.requirements.is_empty());
}

#[test]
fn camel_case_is_the_wire_format() {
    let v = serde_json::to_value(sample()).expect("serialize");
    assert!(v.get("nonGoals").is_some(), "nonGoals must be camelCase: {v}");
    assert!(v.get("acceptanceCriteria").is_some());
    assert!(v.get("openQuestions").is_some());
    let r = &v["requirements"][0];
    assert!(r.get("acceptanceCriteria").is_some(), "requirement field is camelCase: {r}");
    assert_eq!(r.get("priority").and_then(|p| p.as_str()), Some("must"));
}

#[test]
fn a_blank_title_is_refused() {
    let mut d = sample();
    d.title = "   ".into();
    assert_eq!(d.validate(), Err(RequirementsError::BlankTitle));
}

#[test]
fn duplicate_requirement_ids_are_refused() {
    let mut d = sample();
    let mut second = d.requirements[0].clone();
    second.title = "Another".into();
    d.requirements.push(second);
    assert_eq!(d.validate(), Err(RequirementsError::DuplicateId("r-1".into())));
}

#[test]
fn an_overlong_field_is_refused_and_names_its_cap() {
    let mut d = sample();
    d.requirements[0].title = "x".repeat(MAX_TITLE + 1);
    match d.validate() {
        Err(RequirementsError::TooLong { field, len, max }) => {
            assert_eq!(field, "requirement title");
            assert_eq!(len, MAX_TITLE + 1);
            assert_eq!(max, MAX_TITLE);
        }
        other => panic!("expected TooLong, got {other:?}"),
    }
}

#[test]
fn too_many_items_are_refused() {
    let mut d = sample();
    d.goals = vec!["g".into(); MAX_ITEMS + 1];
    match d.validate() {
        Err(RequirementsError::TooMany { field, len, max }) => {
            assert_eq!(field, "goals");
            assert_eq!(len, MAX_ITEMS + 1);
            assert_eq!(max, MAX_ITEMS);
        }
        other => panic!("expected TooMany, got {other:?}"),
    }
}

#[test]
fn the_document_count_cap_is_a_constant_the_engine_can_check() {
    assert_eq!(MAX_DOCUMENTS, 32);
}

#[test]
fn every_priority_spelling_is_accepted() {
    for (text, want) in [
        ("must", Priority::Must),
        ("MUST", Priority::Must),
        ("must have", Priority::Must),
        ("should", Priority::Should),
        ("could", Priority::Could),
        ("nice to have", Priority::Could),
        ("won't", Priority::Wont),
        ("out of scope", Priority::Wont),
        ("  p2  ", Priority::Could),
    ] {
        assert_eq!(Priority::parse(text), Some(want), "{text} should parse");
    }
    assert_eq!(Priority::parse("banana"), None);
    for p in [Priority::Must, Priority::Should, Priority::Could, Priority::Wont] {
        assert_eq!(Priority::parse(p.as_str()), Some(p), "{} round-trips", p.as_str());
    }
}

#[test]
fn repair_caps_an_overlong_document_instead_of_refusing_it() {
    // A file written when a cap was larger, or edited by hand, must still open.
    let mut d = sample();
    d.title = "t".repeat(MAX_DOC_TITLE + 50);
    d.goals = vec!["g".to_string(); MAX_ITEMS + 10];
    d.requirements[0].detail = "d".repeat(MAX_DETAIL + 10);
    assert!(d.validate().is_err(), "the input really is invalid");

    let fixed = d.repair();
    assert!(fixed.validate().is_ok(), "repaired document should validate: {:?}", fixed.validate());
    assert_eq!(fixed.title.chars().count(), MAX_DOC_TITLE);
    assert_eq!(fixed.goals.len(), MAX_ITEMS);
    assert_eq!(fixed.requirements[0].detail.chars().count(), MAX_DETAIL);
}

#[test]
fn repair_clears_duplicate_requirement_ids() {
    let mut d = sample();
    let mut second = d.requirements[0].clone();
    second.title = "Another".into();
    d.requirements.push(second);
    let fixed = d.repair();
    assert!(fixed.validate().is_ok());
    assert_eq!(fixed.requirements[0].id, "r-1");
    assert!(fixed.requirements[1].id.is_empty(), "the later duplicate loses its id");
}

#[test]
fn repair_cuts_on_a_character_boundary() {
    // Multi-byte characters must not be split, and must not be dropped whole.
    let mut d = sample();
    d.title = "中".repeat(MAX_DOC_TITLE + 5);
    let fixed = d.repair();
    assert_eq!(fixed.title.chars().count(), MAX_DOC_TITLE);
    assert!(fixed.title.chars().all(|c| c == '中'), "every character survives intact");
}

#[test]
fn markdown_has_the_expected_shape() {
    let md = sample().to_markdown();
    assert!(md.starts_with("# Checkout redesign\n"), "title is an H1:\n{md}");
    assert!(md.contains("## Goals\n\n- One-screen checkout\n"));
    assert!(md.contains("## Non-goals\n\n- New payment providers\n"));
    assert!(md.contains("## Audiences\n\n- Phone buyers\n"));
    assert!(md.contains("## Requirements\n"));
    assert!(md.contains("### Remember the last address"));
    assert!(md.contains("_Priority: must_"));
    assert!(md.contains("**Acceptance criteria**"));
    assert!(md.contains("## Open questions\n\n- What about gift orders?"));
    assert!(md.ends_with('\n'));
}

#[test]
fn markdown_omits_empty_sections() {
    let md = RequirementsDocument::new("prd-1", "Only a title").to_markdown();
    assert_eq!(md, "# Only a title\n");
    assert!(!md.contains("## Goals"));
    assert!(!md.contains("## Requirements"));
}

#[test]
fn markdown_escapes_text_that_would_otherwise_format() {
    // A requirement is content, not Markdown: `*`, `#`, `_`, `[` and friends must not turn into
    // emphasis, a heading or a link.
    let mut d = RequirementsDocument::new("prd-1", "A *bold* claim");
    d.summary = "See [the doc](https://example.com) and `code` and _emph_.".into();
    d.goals = vec!["# not a heading".into()];
    let md = d.to_markdown();
    assert!(md.contains(r"# A \*bold\* claim"), "the title's asterisks are escaped:\n{md}");
    assert!(md.contains(r"\[the doc\]"), "brackets are escaped:\n{md}");
    assert!(md.contains(r"\`code\`"), "backticks are escaped:\n{md}");
    assert!(md.contains(r"\_emph\_"), "underscores are escaped:\n{md}");
    assert!(md.contains(r"- \# not a heading"), "a leading # is escaped:\n{md}");
}

#[test]
fn markdown_keeps_a_multiline_title_on_one_line() {
    let d = RequirementsDocument::new("prd-1", "First line\nsecond line");
    let md = d.to_markdown();
    assert_eq!(md, "# First line second line\n", "a newline would break the heading");
}

#[test]
fn markdown_escapes_a_line_break_inside_a_bullet_as_it_is() {
    let mut d = RequirementsDocument::new("prd-1", "T");
    d.goals = vec!["first\nsecond".into()];
    let md = d.to_markdown();
    assert!(md.contains("- first\nsecond\n"), "bullets keep their line breaks:\n{md}");
}
