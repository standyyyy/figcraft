//! The structured requirements a design file can carry.
//!
//! These are ordinary document data: they save and reopen with the file, take part in undo, sync in
//! a live session, and appear in branches and version history. They are not chat state.
//!
//! Every string and list is capped, so a model or a collaborator cannot grow a document without
//! bound. The caps are part of the format: a file that exceeds one is repaired on open rather than
//! refused, so an over-long document never stops a file from opening.

use serde::{Deserialize, Serialize};

/// Requirements documents one file may hold.
pub const MAX_DOCUMENTS: usize = 32;
/// Items in one list of a requirements document (goals, requirements, acceptance criteria, …).
pub const MAX_ITEMS: usize = 200;
/// Characters in one requirement's title.
pub const MAX_TITLE: usize = 200;
/// Characters in one requirement's detail text.
pub const MAX_DETAIL: usize = 4000;
/// Characters in a short field (a goal, a non-goal, an audience, a criterion, a question).
pub const MAX_SHORT: usize = 1000;
/// Characters in a document's title and summary.
pub const MAX_DOC_TITLE: usize = 200;
/// Characters in a document's summary.
pub const MAX_SUMMARY: usize = 4000;

/// How important a requirement is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Priority {
    /// Must ship for the release to be usable.
    #[default]
    Must,
    /// Should ship; its absence needs a reason.
    Should,
    /// Nice to have.
    Could,
    /// Explicitly out of scope for now.
    Wont,
}

impl Priority {
    /// The wire and display name (`must`, `should`, `could`, `wont`).
    pub fn as_str(self) -> &'static str {
        match self {
            Priority::Must => "must",
            Priority::Should => "should",
            Priority::Could => "could",
            Priority::Wont => "wont",
        }
    }

    /// Parse a priority name as a model or a person writes it, case-insensitively.
    ///
    /// `won't`, `will not`, `out of scope` and `must not` are the usual ways the same intent is
    /// spelled, so they are accepted rather than rejected as invalid input.
    pub fn parse(s: &str) -> Option<Self> {
        let t = s.trim().to_lowercase();
        match t.as_str() {
            "must" | "must have" | "required" | "p0" => Some(Priority::Must),
            "should" | "should have" | "p1" => Some(Priority::Should),
            "could" | "nice to have" | "optional" | "p2" => Some(Priority::Could),
            "wont" | "won't" | "will not" | "wont have" | "out of scope" | "must not" | "p3" => Some(Priority::Wont),
            _ => None,
        }
    }
}

/// One requirement: what to build, how important it is, and how to know it is done.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Requirement {
    /// Stable id, allocated by the engine; never random and never wall-clock based.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// What the requirement asks for.
    pub title: String,
    /// The detail behind it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    #[serde(default)]
    pub priority: Priority,
    /// How it is verified.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance_criteria: Vec<String>,
    /// What the person does and what happens: one line per interaction, `trigger: response`.
    ///
    /// A requirement without this says what to build and not how it behaves, which is the half a
    /// developer cannot infer -- "a search box" does not say whether typing filters as you go or on
    /// Enter, and both are a day of work to change later.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interactions: Vec<String>,
    /// The data this requirement shows or collects: one line per field, `name: type, rule`.
    ///
    /// Without it a screen with a form does not say what goes in the form: the field names, their
    /// types, whether each is required, and how long it may be.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
    /// What is out of reach at this level: one line per dependency or constraint.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<String>,
}

/// One step of a user's path through the product: who, what they do, and what they see.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserStep {
    /// What the person does, in their own terms.
    pub action: String,
    /// What the system does in response: the screen, the state, the message.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub response: String,
    /// When it happens: a waiting time, an animation length, a timeout.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub timing: String,
}

/// A structured requirements document embedded in a design file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequirementsDocument {
    /// Stable id, allocated by the engine.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub goals: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub non_goals: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audiences: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requirements: Vec<Requirement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance_criteria: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_questions: Vec<String>,
    /// A person's path through the product, step by step, with how long each takes.
    ///
    /// The document said what to build; this says what happens in what order, which is what a flow
    /// review is about and what a screen list cannot express.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub user_flows: Vec<UserStep>,
    /// Terms the document uses and what they mean here, one line per term (`term: meaning`).
    ///
    /// Every product has words that mean something narrower inside it; without this each reader
    /// assumes their own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub glossary: Vec<String>,
}

/// Why a requirements document was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequirementsError {
    /// A title is required and the field is blank.
    BlankTitle,
    /// Two documents (or two requirements in one document) share an id.
    DuplicateId(String),
    /// A field is longer than its cap; the message names the field and the cap.
    TooLong { field: &'static str, len: usize, max: usize },
    /// A list holds more items than its cap.
    TooMany { field: &'static str, len: usize, max: usize },
    /// There are more documents than [`MAX_DOCUMENTS`].
    TooManyDocuments(usize),
}

impl std::fmt::Display for RequirementsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequirementsError::BlankTitle => write!(f, "a requirements document needs a title"),
            RequirementsError::DuplicateId(id) => write!(f, "duplicate id `{id}`"),
            RequirementsError::TooLong { field, len, max } => write!(f, "{field} is {len} characters, over the {max} limit"),
            RequirementsError::TooMany { field, len, max } => write!(f, "{field} holds {len} items, over the {max} limit"),
            RequirementsError::TooManyDocuments(n) => write!(f, "the file would hold {n} requirements documents, over the {MAX_DOCUMENTS} limit"),
        }
    }
}

impl std::error::Error for RequirementsError {}

/// Truncate `s` to at most `max` characters on a character boundary.
fn cap(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// Truncate a list to at most `max` items, keeping the first ones.
fn cap_list(v: &mut Vec<String>, max: usize) {
    if v.len() > max {
        v.truncate(max);
    }
    for s in v.iter_mut() {
        if s.chars().count() > MAX_SHORT {
            *s = cap(s, MAX_SHORT);
        }
    }
}

impl RequirementsDocument {
    /// A new document with `title`, and an `id` the caller owns.
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self { id: id.into(), title: title.into(), ..Self::default() }
    }

    /// Check every cap and the one required field.
    pub fn validate(&self) -> Result<(), RequirementsError> {
        if self.title.trim().is_empty() {
            return Err(RequirementsError::BlankTitle);
        }
        let check = |field: &'static str, value: &str, max: usize| {
            let len = value.chars().count();
            if len > max { Err(RequirementsError::TooLong { field, len, max }) } else { Ok(()) }
        };
        check("title", &self.title, MAX_DOC_TITLE)?;
        check("summary", &self.summary, MAX_SUMMARY)?;
        for (field, list, max) in [
            ("goals", &self.goals, MAX_ITEMS),
            ("nonGoals", &self.non_goals, MAX_ITEMS),
            ("audiences", &self.audiences, MAX_ITEMS),
            ("acceptanceCriteria", &self.acceptance_criteria, MAX_ITEMS),
            ("openQuestions", &self.open_questions, MAX_ITEMS),
        ] {
            if list.len() > max {
                return Err(RequirementsError::TooMany { field, len: list.len(), max });
            }
            for s in list {
                check(field, s, MAX_SHORT)?;
            }
        }
        if self.requirements.len() > MAX_ITEMS {
            return Err(RequirementsError::TooMany { field: "requirements", len: self.requirements.len(), max: MAX_ITEMS });
        }
        let mut seen: Vec<&str> = Vec::new();
        for r in &self.requirements {
            if r.title.trim().is_empty() {
                return Err(RequirementsError::BlankTitle);
            }
            check("requirement title", &r.title, MAX_TITLE)?;
            check("requirement detail", &r.detail, MAX_DETAIL)?;
            if r.acceptance_criteria.len() > MAX_ITEMS {
                return Err(RequirementsError::TooMany { field: "requirement acceptanceCriteria", len: r.acceptance_criteria.len(), max: MAX_ITEMS });
            }
            for s in &r.acceptance_criteria {
                check("requirement acceptance criterion", s, MAX_SHORT)?;
            }
            if !r.id.is_empty() {
                if seen.contains(&r.id.as_str()) {
                    return Err(RequirementsError::DuplicateId(r.id.clone()));
                }
                seen.push(&r.id);
            }
        }
        Ok(())
    }

    /// Bring the document inside every cap, keeping what fits.
    ///
    /// Used when reading a file rather than editing one: a cap that grew smaller between versions,
    /// or a hand-edited file, must not stop the document from opening.
    pub fn repair(self) -> Self {
        let mut d = self;
        d.title = cap(&d.title, MAX_DOC_TITLE);
        d.summary = cap(&d.summary, MAX_SUMMARY);
        for list in [&mut d.goals, &mut d.non_goals, &mut d.audiences, &mut d.acceptance_criteria, &mut d.open_questions] {
            cap_list(list, MAX_ITEMS);
        }
        d.requirements.truncate(MAX_ITEMS);
        for r in &mut d.requirements {
            r.title = cap(&r.title, MAX_TITLE);
            r.detail = cap(&r.detail, MAX_DETAIL);
            cap_list(&mut r.acceptance_criteria, MAX_ITEMS);
        }
        // Duplicate requirement ids would make lookups ambiguous; the later ones lose theirs and
        // are re-identified by the engine on the next edit.
        let mut seen: Vec<String> = Vec::new();
        for r in &mut d.requirements {
            if !r.id.is_empty() {
                if seen.contains(&r.id) {
                    r.id.clear();
                } else {
                    seen.push(r.id.clone());
                }
            }
        }
        d
    }

    /// This document as Markdown.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# ");
        out.push_str(&md_heading(&self.title));
        out.push_str("\n\n");
        if !self.summary.trim().is_empty() {
            out.push_str(&escape_md(&self.summary));
            out.push_str("\n\n");
        }
        for (heading, items) in [("Goals", &self.goals), ("Non-goals", &self.non_goals), ("Audiences", &self.audiences)] {
            push_list(&mut out, heading, items);
        }
        if !self.requirements.is_empty() {
            out.push_str("## Requirements\n\n");
            for r in &self.requirements {
                out.push_str("### ");
                out.push_str(&md_heading(&r.title));
                out.push_str("  \n_Priority: ");
                out.push_str(r.priority.as_str());
                out.push_str("_\n\n");
                if !r.detail.trim().is_empty() {
                    out.push_str(&escape_md(&r.detail));
                    out.push_str("\n\n");
                }
                for (heading, items) in [("Interactions", &r.interactions), ("Fields", &r.fields), ("Constraints", &r.constraints)] {
                    if items.is_empty() {
                        continue;
                    }
                    out.push_str("**");
                    out.push_str(heading);
                    out.push_str("**\n\n");
                    for item in items {
                        out.push_str("- ");
                        out.push_str(&escape_md(item));
                        out.push('\n');
                    }
                    out.push('\n');
                }
                if !r.acceptance_criteria.is_empty() {
                    out.push_str("**Acceptance criteria**\n\n");
                    for c in &r.acceptance_criteria {
                        out.push_str("- ");
                        out.push_str(&escape_md(c));
                        out.push('\n');
                    }
                    out.push('\n');
                }
            }
        }
        if !self.user_flows.is_empty() {
            out.push_str("## User flows\n\n");
            for (i, step) in self.user_flows.iter().enumerate() {
                out.push_str(&format!("{}. ", i + 1));
                out.push_str(&escape_md(&step.action));
                if !step.response.trim().is_empty() {
                    out.push_str(" → ");
                    out.push_str(&escape_md(&step.response));
                }
                if !step.timing.trim().is_empty() {
                    out.push_str(" (");
                    out.push_str(&escape_md(&step.timing));
                    out.push(')');
                }
                out.push('\n');
            }
            out.push('\n');
        }
        push_list(&mut out, "Glossary", &self.glossary);
        push_list(&mut out, "Acceptance criteria", &self.acceptance_criteria);
        push_list(&mut out, "Open questions", &self.open_questions);
        out.trim_end().to_string() + "\n"
    }
}

fn push_list(out: &mut String, heading: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    out.push_str("## ");
    out.push_str(heading);
    out.push_str("\n\n");
    for item in items {
        out.push_str("- ");
        out.push_str(&escape_md(item));
        out.push('\n');
    }
    out.push('\n');
}

/// A heading's text: newlines would break the heading, so they become spaces.
fn md_heading(s: &str) -> String {
    escape_md(&s.replace(['\n', '\r'], " "))
}

/// Escape the Markdown characters that would otherwise change the rendering of a plain value.
///
/// Text a person or a model wrote is content, not formatting: a requirement that mentions `*` or
/// starts with `#` must not turn into emphasis or a heading when the document is exported.
fn escape_md(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '|' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}
