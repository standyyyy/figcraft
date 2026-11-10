//! The shadow session: where an Agent turn's edits happen before the live document sees them.
//!
//! A turn runs its tool calls against a shadow [`Session`] built from a snapshot of the live
//! document, recording each successful, replayable command. Only when the whole turn succeeds, and
//! the live document is still at the revision the Agent inspected, is the recording replayed as one
//! `command.batch`. So a cancelled, failed or stale turn changes nothing, and a successful one is
//! exactly one undo step.
//!
//! The guard is the point: replaying against a document the Agent never saw would silently drop a
//! person's edit, so the mismatch is refused and the caller offers a rerun.

use figcraft_agent_tools::{Backend, ToolPolicy};
use figcraft_doc::Document;
use figcraft_engine::Session;
use serde_json::{Value, json};

use crate::{AgentError, AgentErrorKind};

/// One recorded command: the canonical id and its normalized parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayStep {
    /// The command id, as the registry knows it (`node.create`, not a tool's alias).
    pub command: String,
    /// The parameters, as the command receives them.
    pub params: Value,
}

/// The shadow document plus the commands that produced it.
pub struct Shadow {
    /// The document as the Agent inspects and edits it.
    session: Session,
    /// The live revision the shadow was taken from; committing requires it to be unchanged.
    base_revision: u64,
    /// The live document's uid, so a commit lands in the file the Agent inspected.
    uid: u64,
    /// The commands that turned the snapshot into the shadow, in order.
    replay: Vec<ReplayStep>,
    /// Whether anything replayable changed (read-only tools leave the replay empty).
    dirty: bool,
}

impl std::fmt::Debug for Shadow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shadow")
            .field("base_revision", &self.base_revision)
            .field("uid", &self.uid)
            .field("steps", &self.replay.len())
            .field("dirty", &self.dirty)
            .finish()
    }
}

impl Shadow {
    /// Take a snapshot of the live document to run the turn against.
    ///
    /// The snapshot carries the document, its page and its selection, so a tool that reads the
    /// view (a screenshot, a selection-dependent command) sees what the Agent was told about.
    pub fn capture(live: &Session) -> Result<Self, AgentError> {
        let ds = live.ds().map_err(|e| AgentError::new(AgentErrorKind::Configuration, e.to_string()))?;
        let base_revision = ds.revision;
        let uid = ds.uid;
        // A session of its own, holding one document copy: edits here never touch the live one.
        // The page and selection come along, so a tool that reads the view sees what the Agent was
        // told about.
        let mut doc = (*ds.doc).clone();
        doc.current_page = Some(ds.page);
        let mut session = Session::with_document(doc);
        session.set_selection(ds.selection.clone());
        Ok(Self { session, base_revision, uid, replay: Vec::new(), dirty: false })
    }

    /// The shadow session, which tools run against.
    pub fn session(&mut self) -> &mut Session {
        &mut self.session
    }

    /// The shadow document, for a tool that inspects it.
    pub fn doc(&self) -> Result<&Document, AgentError> {
        self.session.doc().map_err(|e| AgentError::new(AgentErrorKind::Tool, e.to_string()))
    }

    /// Run one tool against the shadow, recording what it changed.
    ///
    /// Read-only tools (`inspect_document`, `screenshot`, `list_commands`) are not recorded: they
    /// do not change the document, so replaying them would be noise.
    pub fn call(&mut self, policy: ToolPolicy, name: &str, args: &Value) -> figcraft_agent_tools::ToolResult {
        let before = self.replay.len();
        let session = &mut self.session;
        let replay = &mut self.replay;
        let result = figcraft_agent_tools::call_tool(&mut ShadowBackend { session, replay }, policy, name, args);
        // A mutation tool records itself through the backend; anything else leaves the log alone.
        if self.replay.len() > before {
            self.dirty = true;
        }
        result
    }

    /// Record a command the turn ran, if it is one that changes the document.
    ///
    /// The command id is canonical: a tool's friendly name never reaches the log, so replay does not
    /// depend on the tool catalog surviving a version change.
    pub fn record(&mut self, step: ReplayStep) {
        self.replay.push(step);
        self.dirty = true;
    }

    /// The recorded commands, in order.
    pub fn replay(&self) -> &[ReplayStep] {
        &self.replay
    }

    /// Whether the turn changed anything replayable.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// The live revision the snapshot was taken from.
    pub fn base_revision(&self) -> u64 {
        self.base_revision
    }

    /// The document the snapshot came from.
    pub fn uid(&self) -> u64 {
        self.uid
    }

    /// Whether `live` is still the document the shadow was taken from, at the same revision.
    fn still_current(&self, live: &Session) -> bool {
        match live.ds() {
            Ok(ds) => ds.uid == self.uid && ds.revision == self.base_revision,
            Err(_) => false,
        }
    }

    /// Commit the turn to `live` as one undo step.
    ///
    /// `label` names the step (`AI: <the request>`). Nothing is replayed when the turn changed
    /// nothing, when the live document moved on, or when the shadow's own replay produced different
    /// results than the batch did: an unresolvable difference rolls the batch back.
    pub fn commit(self, live: &mut Session, label: &str) -> Result<Commit, AgentError> {
        if !self.dirty || self.replay.is_empty() {
            return Ok(Commit::Unchanged);
        }
        if !self.still_current(live) {
            return Err(AgentError::new(
                AgentErrorKind::Conflict,
                "the document changed while the Agent worked, so its changes were not applied".to_string(),
            ));
        }
        let commands: Vec<Value> = self.replay.iter().map(|s| json!({"command": s.command, "params": s.params})).collect();
        let batch = json!({"label": label, "commands": commands});
        match live.execute("command.batch", &batch) {
            Ok(_) => Ok(Commit::Applied { steps: self.replay.len(), label: label.to_string() }),
            // The batch rolls itself back, so nothing landed and the turn can be retried.
            Err(e) => Err(AgentError::new(AgentErrorKind::Conflict, format!("the changes could not be applied together: {e}"))),
        }
    }
}

/// What a commit did.
#[derive(Clone, Debug, PartialEq)]
pub enum Commit {
    /// The turn only read the document; nothing to apply.
    Unchanged,
    /// The recorded commands became one undo step.
    Applied { steps: usize, label: String },
}

/// The backend a shadow runs tools against: it runs commands on the shadow session and records
/// each one that changes the document.
struct ShadowBackend<'a> {
    session: &'a mut Session,
    replay: &'a mut Vec<ReplayStep>,
}

impl ShadowBackend<'_> {
    /// A `command.batch` from a model: run each step on the shadow and record it, so the commit
    /// builds its own batch rather than nesting one.
    fn call_batch(&mut self, params: &Value) -> Result<Value, String> {
        let steps = params.get("commands").and_then(Value::as_array).ok_or("command.batch needs `commands`")?;
        if steps.len() > MAX_EXPANDED_BATCH {
            return Err(format!("a batch of {} steps is over the {MAX_EXPANDED_BATCH} the Agent accepts", steps.len()));
        }
        let mut out = Vec::with_capacity(steps.len());
        for step in steps {
            let command = step.get("command").and_then(Value::as_str).ok_or("a batch step needs `command`")?.to_string();
            let cmd_params = step.get("params").cloned().unwrap_or(Value::Null);
            if command == "command.batch" {
                return Err("a batch may not hold another batch".into());
            }
            let result = self.session.execute(&command, &cmd_params).map_err(|e| e.to_string())?;
            if figcraft_engine::cmd::find_command(&command).is_some_and(|c| c.history) {
                self.replay.push(ReplayStep { command, params: cmd_params });
            }
            out.push(result);
        }
        Ok(Value::Array(out))
    }
}

/// The most steps one model batch may expand to, so a runaway answer cannot grow the replay without
/// bound.
const MAX_EXPANDED_BATCH: usize = 200;

impl Backend for ShadowBackend<'_> {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "engine.execute" => {
                let command = params.get("command").and_then(Value::as_str).ok_or("engine.execute needs a `command`")?.to_string();
                let cmd_params = params.get("params").cloned().unwrap_or(Value::Null);
                // `command.batch` is a batch of commands: record the inner ones, which is what the
                // commit replays (a nested batch would only make the undo label less useful).
                if command == "command.batch" {
                    return self.call_batch(&cmd_params);
                }
                let result = self.session.execute(&command, &cmd_params).map_err(|e| e.to_string())?;
                // Only a command that edits the document is worth replaying; an inspection leaves
                // the document exactly as it found it.
                if figcraft_engine::cmd::find_command(&command).is_some_and(|c| c.history) {
                    self.replay.push(ReplayStep { command, params: cmd_params });
                }
                Ok(result)
            }
            "engine.commands" => Ok(Value::Array(
                figcraft_engine::all_commands()
                    .iter()
                    .map(|c| {
                        json!({
                            "id": c.id,
                            "label": c.label,
                            "params": c.params,
                            "menu": c.menu(),
                            "shortcut": c.shortcut(),
                            "undoable": c.history,
                        })
                    })
                    .collect(),
            )),
            // The inspection methods are control-channel methods, not commands: the shadow answers
            // them the way a headless session does, against its own document.
            "document.inspect" => {
                let depth = params.get("depth").and_then(Value::as_u64).unwrap_or(8).min(64) as usize;
                let options = figcraft_engine::inspect::InspectOptions::parse(&params).map_err(|e| e.to_string())?;
                let all_pages = params.get("allPages").and_then(Value::as_bool).unwrap_or(false);
                Ok(figcraft_engine::inspect::document_with_options(self.session, depth, all_pages, &options))
            }
            "node.get" => {
                let id = params.get("id").and_then(figcraft_engine::params::id_of).ok_or("`id` required (a node id)")?;
                let depth = params.get("depth").and_then(Value::as_u64).unwrap_or(1).min(64) as usize;
                let doc = self.session.doc().map_err(|e| e.to_string())?;
                if !doc.contains(id) {
                    return Err(format!("no such node {id}"));
                }
                let options = figcraft_engine::inspect::InspectOptions::parse(&params).map_err(|e| e.to_string())?;
                Ok(figcraft_engine::inspect::node_json_with_options(doc, id, depth, &options))
            }
            "ui.inspect" => Ok(json!({
                "backend": "agent-shadow",
                "status": self.session.status(),
                "tool": self.session.tool_info(),
            })),
            "events.poll" => {
                // The shadow has no other writer, so a poll never waits.
                let after = params.get("after").and_then(Value::as_u64).unwrap_or(0);
                let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(100).clamp(1, 1000) as usize;
                let topics = figcraft_engine::events::topics(params.get("topics")).map_err(|e| e.to_string())?;
                let result = self.session.events.poll(after, &topics, limit, std::time::Duration::ZERO);
                serde_json::to_value(result).map_err(|e| e.to_string())
            }
            other => Err(format!("`{other}` is not available to the Agent's shadow document")),
        }
    }

    fn has_ui(&self) -> bool {
        false
    }

    fn describe(&self) -> String {
        "the Agent's shadow document".into()
    }
}

/// The undo label a turn records: `AI: <the request>`, shortened to stay readable.
pub fn undo_label(request: &str) -> String {
    const MAX: usize = 60;
    let one_line: String = request.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= MAX {
        return format!("AI: {one_line}");
    }
    let cut: String = one_line.chars().take(MAX).collect();
    format!("AI: {cut}…")
}
