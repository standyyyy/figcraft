//! Structured requirements documents: list, read, write, delete and export as Markdown.
//!
//! Requirements are ordinary document data (see [`figcraft_doc::RequirementsDocument`]), so every
//! edit here goes through the command registry, is undoable, and reaches MCP, the in-app Agent and a
//! live session the same way. Ids are allocated by the document (`PRD:n`), never randomly, so
//! replaying the same commands produces the same ids.
//!
//! Export writes a file only through the engine's guarded file-access module, never `std::fs`, so
//! the automation roots apply to it like to every other write.

use figcraft_doc::{MAX_DOCUMENTS, Priority, Requirement, RequirementsDocument};
use serde_json::{Value, json};

use super::CommandSpec;
use crate::Session;
use crate::automation::file_access;
use crate::params::{get, req_string, string};
use crate::{Result, bad, spec};

/// The id a caller named: the explicit `id`, else the `title` looked up.
///
/// An exact id wins over a title, so a document titled like another document's id is still
/// reachable.
fn document_index(s: &Session, p: &Value) -> Result<usize> {
    let doc = s.doc()?;
    if let Some(id) = string(p, "id").filter(|i| !i.trim().is_empty()) {
        if let Some(i) = doc.requirements.iter().position(|r| r.id == id) {
            return Ok(i);
        }
        return Err(bad("requirements", format!("no requirements document `{id}`")));
    }
    let title = req_string("requirements", p, "title")?;
    doc.requirements.iter().position(|r| r.title == title).ok_or_else(|| bad("requirements", format!("no requirements document titled `{title}`")))
}

fn list_of(p: &Value, key: &str) -> std::result::Result<Vec<String>, String> {
    let Some(v) = get(p, key) else { return Ok(Vec::new()) };
    let Some(arr) = v.as_array() else {
        return Err(format!("`{key}` must be an array of strings"));
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(s) = item.as_str() else {
            return Err(format!("`{key}` must be an array of strings"));
        };
        out.push(s.to_string());
    }
    Ok(out)
}

/// The requirements from a `requirements` array (a whole document's worth or one requirement).
fn requirements_of(p: &Value) -> std::result::Result<Vec<Requirement>, String> {
    let Some(v) = get(p, "requirements") else { return Ok(Vec::new()) };
    let Some(arr) = v.as_array() else {
        return Err("`requirements` must be an array".into());
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(obj) = item.as_object() else {
            return Err("each entry of `requirements` must be an object".into());
        };
        let title = obj.get("title").and_then(Value::as_str).unwrap_or("").to_string();
        let id = obj.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        let detail = obj.get("detail").and_then(Value::as_str).unwrap_or("").to_string();
        let priority = match obj.get("priority").and_then(Value::as_str) {
            Some(name) => Priority::parse(name).ok_or_else(|| format!("`priority` is not a priority: `{name}`"))?,
            None => Priority::default(),
        };
        let criteria = match obj.get("acceptanceCriteria") {
            Some(Value::Array(a)) => a.iter().map(|c| c.as_str().unwrap_or("").to_string()).collect(),
            Some(_) => return Err("`acceptanceCriteria` must be an array of strings".into()),
            None => Vec::new(),
        };
        out.push(Requirement { id, title, detail, priority, acceptance_criteria: criteria });
    }
    Ok(out)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!("requirements.list", "List requirements documents", "{}", false, |s, _p| {
            let doc = s.doc()?;
            let documents: Vec<Value> = doc
                .requirements
                .iter()
                .map(|r| {
                    json!({
                        "id": r.id,
                        "title": r.title,
                        "requirements": r.requirements.len(),
                        "acceptanceCriteria": r.acceptance_criteria.len() + r.requirements.iter().map(|x| x.acceptance_criteria.len()).sum::<usize>(),
                    })
                })
                .collect();
            Ok(json!({"documents": documents, "max": MAX_DOCUMENTS}))
        }),
        spec!("requirements.get", "Read a requirements document", "{id? (exact id, preferred) | title?}", false, |s, p| {
            let i = document_index(s, p)?;
            let doc = s.doc()?;
            let r = doc.requirements.get(i).ok_or_else(|| bad("requirements.get", "no requirements document"))?;
            Ok(json!({"document": r, "markdown": r.to_markdown()}))
        }),
        spec!(
            "requirements.upsert",
            "Write a requirements document",
            "{id? (an existing document to update; omit to create), title, summary?, goals?, nonGoals?, audiences?, requirements? [{id?, title, detail?, priority?, acceptanceCriteria?}], acceptanceCriteria?, openQuestions?}",
            true,
            |s, p| {
                let title = req_string("requirements.upsert", p, "title")?;
                // Which document this is: an explicit id updates it, no id creates one.
                let existing = match string(p, "id") {
                    Some(id) if !id.trim().is_empty() => Some(document_index(s, p)?),
                    _ => None,
                };
                let mut doc = RequirementsDocument {
                    // A created document takes an id from the document; an update keeps its own.
                    id: existing.and_then(|i| s.doc().ok().and_then(|d| d.requirements.get(i).map(|r| r.id.clone()))).unwrap_or_default(),
                    title,
                    summary: string(p, "summary").unwrap_or_default(),
                    goals: list_of(p, "goals").map_err(|e| bad("requirements.upsert", e))?,
                    non_goals: list_of(p, "nonGoals").map_err(|e| bad("requirements.upsert", e))?,
                    audiences: list_of(p, "audiences").map_err(|e| bad("requirements.upsert", e))?,
                    requirements: requirements_of(p).map_err(|e| bad("requirements.upsert", e))?,
                    acceptance_criteria: list_of(p, "acceptanceCriteria").map_err(|e| bad("requirements.upsert", e))?,
                    open_questions: list_of(p, "openQuestions").map_err(|e| bad("requirements.upsert", e))?,
                };
                // Requirement ids the caller left out are ours to allocate, deterministically.
                if existing.is_none() {
                    if s.doc()?.requirements.len() >= MAX_DOCUMENTS {
                        return Err(bad("requirements.upsert", format!("this file already holds {MAX_DOCUMENTS} requirements documents")));
                    }
                    doc.id = s.doc_mut()?.new_requirements_id();
                }
                for (n, r) in doc.requirements.iter_mut().enumerate() {
                    if r.id.trim().is_empty() {
                        r.id = format!("r-{}", n + 1);
                    }
                }
                doc.validate().map_err(|e| bad("requirements.upsert", e.to_string()))?;
                let id = doc.id.clone();
                match existing {
                    Some(i) => {
                        let slot = s.doc_mut()?.requirements.get_mut(i).ok_or_else(|| bad("requirements.upsert", "no requirements document"))?;
                        *slot = doc.clone();
                    }
                    None => s.doc_mut()?.requirements.push(doc.clone()),
                }
                Ok(json!({"id": id, "document": doc}))
            }
        ),
        spec!("requirements.delete", "Delete a requirements document", "{id? | title}", true, |s, p| {
            let i = document_index(s, p)?;
            let removed = {
                let doc = s.doc_mut()?;
                if i >= doc.requirements.len() {
                    return Err(bad("requirements.delete", "no requirements document"));
                }
                doc.requirements.remove(i)
            };
            Ok(json!({"id": removed.id, "title": removed.title}))
        }),
        spec!(
            "requirements.exportMarkdown",
            "Export requirements as Markdown",
            "{id? | title, path? (write it there; omit to return the text)}",
            false,
            |s, p| {
                let i = document_index(s, p)?;
                let markdown = s
                    .doc()?
                    .requirements
                    .get(i)
                    .map(RequirementsDocument::to_markdown)
                    .ok_or_else(|| bad("requirements.exportMarkdown", "no requirements document"))?;
                match string(p, "path").filter(|path| !path.trim().is_empty()) {
                    Some(path) => {
                        // Every write goes through the guarded module, so the automation roots
                        // apply: an agent confined to a folder cannot write elsewhere.
                        file_access::write(std::path::Path::new(&path), markdown.as_bytes())
                            .map_err(|e| bad("requirements.exportMarkdown", e.to_string()))?;
                        Ok(json!({"path": path, "bytes": markdown.len()}))
                    }
                    None => Ok(json!({"markdown": markdown, "bytes": markdown.len()})),
                }
            }
        ),
        spec!("requirements.priorityNames", "Requirements priority names", "{}", false, |_s, _p| {
            Ok(json!({
                "priorities": [
                    Priority::Must.as_str(),
                    Priority::Should.as_str(),
                    Priority::Could.as_str(),
                    Priority::Wont.as_str(),
                ]
            }))
        }),
    ]
}

#[cfg(test)]
#[path = "../requirements_tests.rs"]
mod tests;
