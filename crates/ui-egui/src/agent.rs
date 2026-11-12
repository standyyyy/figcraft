//! The in-app Agent panel: chat with the model, and the requirements documents in this file.
//!
//! The panel is deliberately thin. It holds display state and asks a host for work: it never calls
//! a model, owns no Rig type, and never touches a document field. Edits go through commands
//! (`requirements.upsert` and friends), so undo, collaboration and MCP all see one implementation.
//!
//! The runtime handle stays out of this struct's serializable surface: [`ui::AgentPanel::summary`]
//! is what `ui.inspect` reports, so a status dump never leaks a transcript or a credential.

use figcraft_agent::history::{Conversation, Transcript};
use figcraft_agent::run::{AgentEvent, AgentRun, CancelToken, Limits, Outcome, ToolCall};
use figcraft_doc::RequirementsDocument;
use serde_json::{Value, json};

/// Which tab the Agent panel shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AgentTab {
    #[default]
    Chat,
    Requirements,
}

/// Where a turn is, as the panel draws it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunStatus {
    /// Nothing is running.
    #[default]
    Idle,
    /// The model is answering; text arrives in deltas.
    Streaming,
    /// A tool is running.
    Tool,
    /// The turn finished and its edits are in the document.
    Done,
    /// The turn failed; `error` says why.
    Failed,
    /// The user stopped it.
    Cancelled,
    /// The live document changed while the turn ran; the user may rerun.
    Conflict,
}

/// The Agent panel's display state for one document.
#[derive(Clone, Debug)]
pub struct AgentPanel {
    /// Which tab is shown.
    pub tab: AgentTab,
    /// The conversation this panel is looking at.
    pub conversation: String,
    /// The text in the composer.
    pub draft: String,
    /// The turn being shown (`None`: no turn yet).
    pub current: Option<AgentRun>,
    /// What the current turn is doing.
    pub status: RunStatus,
    /// The tool activity of the current turn, in order.
    pub tools: Vec<ToolCall>,
    /// Non-fatal diagnostics from the running turn, newest last (a shortened history, say).
    pub notices: Vec<String>,
    /// The running turn's reasoning, streamed.
    ///
    /// Working material, not the answer: shown so a person can see how the model got there, kept out
    /// of the transcript so a later turn does not continue from someone else's notes.
    pub reasoning: String,
    /// Whether the reasoning pane is open. Collapsed by default: most turns do not need it, and it is
    /// the longest text on screen when open.
    pub show_reasoning: bool,
    /// The error text of the last failure, with the action to take.
    pub error: Option<String>,
    /// The undo label the last commit used (shown so a person can find the step).
    pub last_undo_label: Option<String>,
    /// A replay whose commit was refused because the document moved on.
    ///
    /// Kept, not discarded: a turn takes minutes and its commands are absolute (`node.create` with a
    /// parent id), so applying them to the document as it is now is safe and is what a person wants --
    /// the alternative is losing the work and asking again. The panel offers it as a button; nothing
    /// applies it without a click.
    pub recoverable: Option<figcraft_agent::CommitEnvelope>,
    /// The requirements document a turn just wrote, so the panel can point at it.
    ///
    /// A model that writes a PRD answers "已写好了", and the document is in a tab the person is not
    /// looking at -- which reads as "it did not work". Naming it and offering to open it is the
    /// difference between a feature and a mystery.
    pub wrote_requirements: Option<String>,
    /// The requirements document open in the Requirements tab (`None`: the list).
    pub editing: Option<String>,
    /// The Requirements tab's draft fields while a document is open.
    pub requirements_draft: Option<RequirementsDraft>,
    /// The Markdown export result, shown briefly.
    pub export_note: Option<String>,
    /// Whether the requirements window is open, separately from the Agent panel.
    ///
    /// A requirements document is read and reviewed, not glanced at: fourteen requirements with their
    /// interactions, fields and acceptance criteria do not fit a 320 px sidebar, and squeezing them
    /// into one was what made the panel feel dense. The window has the room, the sidebar keeps the
    /// conversation.
    pub window_open: bool,
    /// The window is in its edit form rather than its reading form.
    pub window_editing: bool,
    /// Which requirement is expanded in the reading form; the rest stay one line.
    pub expanded: Option<String>,
}

/// The editable fields of one requirements document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequirementsDraft {
    /// The document's id (`None` while creating one).
    pub id: Option<String>,
    pub title: String,
    pub summary: String,
    /// One item per line, as the panel edits them.
    pub goals: String,
    pub non_goals: String,
    pub audiences: String,
    pub acceptance_criteria: String,
    pub open_questions: String,
    /// One user-flow step per line: `action → response (timing)`.
    pub user_flows: String,
    /// One term per line: `term: meaning`.
    pub glossary: String,
    /// One requirement per line: `title | priority | detail`.
    pub requirements: String,
}

impl RequirementsDraft {
    /// A draft from a saved document.
    pub fn from_document(d: &RequirementsDocument) -> Self {
        Self {
            id: Some(d.id.clone()),
            title: d.title.clone(),
            summary: d.summary.clone(),
            goals: d.goals.join("\n"),
            non_goals: d.non_goals.join("\n"),
            audiences: d.audiences.join("\n"),
            acceptance_criteria: d.acceptance_criteria.join("\n"),
            open_questions: d.open_questions.join("\n"),
            user_flows: d
                .user_flows
                .iter()
                .map(|s| {
                    let mut line = s.action.clone();
                    if !s.response.is_empty() {
                        line.push_str(" → ");
                        line.push_str(&s.response);
                    }
                    if !s.timing.is_empty() {
                        line.push_str(&format!(" ({})", s.timing));
                    }
                    line
                })
                .collect::<Vec<_>>()
                .join("\n"),
            glossary: d.glossary.join("\n"),
            requirements: d
                .requirements
                .iter()
                .map(|r| {
                    if r.detail.is_empty() {
                        format!("{} | {}", r.title, r.priority.as_str())
                    } else {
                        format!("{} | {} | {}", r.title, r.priority.as_str(), r.detail)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    /// An empty draft for a new document.
    pub fn new() -> Self {
        Self {
            id: None,
            title: String::new(),
            summary: String::new(),
            goals: String::new(),
            non_goals: String::new(),
            audiences: String::new(),
            acceptance_criteria: String::new(),
            open_questions: String::new(),
            user_flows: String::new(),
            glossary: String::new(),
            requirements: String::new(),
        }
    }

    /// The command parameters this draft becomes.
    ///
    /// The panel never writes a document field: it produces parameters for `requirements.upsert`,
    /// which validates, allocates ids and does the write as one undo step.
    pub fn to_params(&self) -> Value {
        fn lines(s: &str) -> Vec<String> {
            s.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
        }
        let requirements: Vec<Value> = lines(&self.requirements)
            .iter()
            .map(|line| {
                let mut parts = line.split('|').map(str::trim);
                let title = parts.next().unwrap_or("").to_string();
                let priority = parts.next().filter(|p| !p.is_empty()).unwrap_or("must").to_string();
                let detail = parts.collect::<Vec<_>>().join(" | ");
                if detail.is_empty() {
                    json!({"title": title, "priority": priority})
                } else {
                    json!({"title": title, "priority": priority, "detail": detail})
                }
            })
            .collect();
        let mut params = json!({
            "title": self.title.trim(),
            "summary": self.summary.trim(),
            "goals": lines(&self.goals),
            "nonGoals": lines(&self.non_goals),
            "audiences": lines(&self.audiences),
            "requirements": requirements,
            "acceptanceCriteria": lines(&self.acceptance_criteria),
            "openQuestions": lines(&self.open_questions),
            "userFlows": lines(&self.user_flows).iter().map(|l| {
                // `action → response (timing)`: the arrow separates the two, the parentheses the timing.
                let (head, timing) = match l.rfind('(') {
                    Some(i) if l.trim_end().ends_with(')') => (&l[..i], l[i + 1..l.len() - 1].trim()),
                    _ => (l.as_str(), ""),
                };
                let (action, response) = match head.find('→') {
                    Some(i) => (head[..i].trim(), head[i + '→'.len_utf8()..].trim()),
                    None => (head.trim(), ""),
                };
                json!({"action": action, "response": response, "timing": timing})
            }).collect::<Vec<_>>(),
            "glossary": lines(&self.glossary),
        });
        if let Some(id) = self.id.as_ref().filter(|i| !i.is_empty()) {
            params["id"] = json!(id);
        }
        params
    }

    /// Whether the draft can be saved (a title is required).
    pub fn is_saveable(&self) -> bool {
        !self.title.trim().is_empty()
    }
}

impl Default for RequirementsDraft {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentPanel {
    /// A new panel for conversation `conversation`.
    pub fn new(conversation: impl Into<String>) -> Self {
        Self {
            tab: AgentTab::Chat,
            conversation: conversation.into(),
            draft: String::new(),
            current: None,
            status: RunStatus::Idle,
            tools: Vec::new(),
            notices: Vec::new(),
            reasoning: String::new(),
            show_reasoning: false,
            error: None,
            last_undo_label: None,
            recoverable: None,
            wrote_requirements: None,
            editing: None,
            requirements_draft: None,
            export_note: None,
            window_open: false,
            window_editing: false,
            expanded: None,
        }
    }

    /// Whether a turn is in flight (the send button is disabled and the stop button shown).
    pub fn is_running(&self) -> bool {
        matches!(self.status, RunStatus::Streaming | RunStatus::Tool)
    }

    /// Whether the composer may send: there is text and nothing is running.
    pub fn can_send(&self) -> bool {
        !self.draft.trim().is_empty() && !self.is_running()
    }

    /// Start showing a new turn.
    pub fn begin_turn(&mut self, turn: u64, request: &str) {
        self.current = Some(AgentRun::new(turn, request));
        self.status = RunStatus::Streaming;
        self.wrote_requirements = None;
        self.tools.clear();
        self.notices.clear();
        self.reasoning.clear();
        self.error = None;
        self.last_undo_label = None;
        self.export_note = None;
    }

    /// Apply one runtime event.
    ///
    /// The panel decides nothing from an error string: `Failed` carries the classified kind, and a
    /// revision conflict is its own status because the offer is to rerun, not to fix a setting.
    pub fn summary(&self) -> Value {
        json!({
            "tab": match self.tab {
                AgentTab::Chat => "chat",
                AgentTab::Requirements => "requirements",
            },
            "status": match self.status {
                RunStatus::Idle => "idle",
                RunStatus::Streaming => "streaming",
                RunStatus::Tool => "tool",
                RunStatus::Done => "done",
                RunStatus::Failed => "failed",
                RunStatus::Cancelled => "cancelled",
                RunStatus::Conflict => "conflict",
            },
            "running": self.is_running(),
            "canSend": self.can_send(),
            "tools": self.tools.len(),
            "hasError": self.error.is_some(),
            // The message itself: "conflict" with no reason leaves a person with nothing to do, and a
            // bug report with nothing to diagnose.
            "error": self.error,
            "editingRequirements": self.editing.is_some(),
            "windowOpen": self.window_open,
            "windowEditing": self.window_editing,
        })
    }

    /// Show how the turn ended, from the terminal that says so.
    ///
    /// The panel's own status and the run's recorded outcome both come from this one value, so they
    /// cannot disagree. What the *document* should do about it is the app's decision, not the panel's.
    pub fn settle(&mut self, terminal: &figcraft_agent::TurnTerminal, undo_label: Option<&str>) {
        if let Some(run) = self.current.as_mut() {
            run.settle(terminal);
        }
        self.status = match terminal {
            figcraft_agent::TurnTerminal::Success { .. } => RunStatus::Done,
            figcraft_agent::TurnTerminal::Cancelled { .. } => RunStatus::Cancelled,
            // A failure says what happened *and* what to do about it: the kind knows the action, and a
            // message alone leaves a person with nowhere to go.
            figcraft_agent::TurnTerminal::Failed { kind, message, .. } => {
                self.error = Some(format!("{message} {}", kind.action()));
                // A revision conflict is its own status: the offer is to rerun, not to fix a setting.
                if *kind == figcraft_agent::AgentErrorKind::Conflict { RunStatus::Conflict } else { RunStatus::Failed }
            }
            figcraft_agent::TurnTerminal::LimitReached { kind, message, .. } => {
                self.error = Some(format!("{message} {}", kind.action()));
                RunStatus::Failed
            }
        };
        if let Some(label) = undo_label {
            self.last_undo_label = Some(label.to_string());
        }
    }

    pub fn apply(&mut self, event: &AgentEvent, limits: &Limits) {
        match event {
            AgentEvent::Started { .. } => self.status = RunStatus::Streaming,
            // Reasoning grows while the model works; it is not the answer and does not touch the run.
            AgentEvent::ReasoningDelta { text, .. } => {
                self.reasoning.push_str(text);
                // Bounded: a long deliberation must not grow the buffer without limit.
                if self.reasoning.chars().count() > 16_384 {
                    let keep: String = self.reasoning.chars().skip(self.reasoning.chars().count() - 16_384).collect();
                    self.reasoning = keep;
                }
            }
            AgentEvent::TextDelta { .. } => {
                if let Some(run) = self.current.as_mut() {
                    run.apply(event, limits);
                }
                self.status = RunStatus::Streaming;
            }
            AgentEvent::ToolStarted { name, .. } => {
                self.status = RunStatus::Tool;
                self.tools.push(ToolCall { name: name.clone(), ok: false });
            }
            AgentEvent::ToolFinished { name, ok, .. } => {
                if let Some(last) = self.tools.iter_mut().rev().find(|r| &r.name == name) {
                    last.ok = *ok;
                } else {
                    self.tools.push(ToolCall { name: name.clone(), ok: *ok });
                }
                if let Some(run) = self.current.as_mut() {
                    run.apply(event, limits);
                }
                self.status = RunStatus::Tool;
            }
            // A diagnostic: shown, but it changes neither the turn's status nor its record. The turn
            // is still running, and a notice is not a failure.
            AgentEvent::Notice { message, .. } => {
                self.notices.push(message.clone());
                if self.notices.len() > 8 {
                    self.notices.remove(0);
                }
            }
        }
    }
}

/// The chat transcript the panel draws: messages, newest last.
///
/// Kept separate from [`AgentPanel`] so a frame can borrow the conversation immutably while the
/// panel is borrowed mutably.
#[derive(Clone, Debug, Default)]
pub struct ChatView {
    /// Finished turns plus the running one.
    pub runs: Vec<AgentRun>,
    /// The summary compaction wrote, if earlier turns were elided.
    pub summary: Option<String>,
}

impl ChatView {
    /// Build the view from a conversation.
    pub fn of(conversation: &Conversation) -> Self {
        Self { runs: conversation.turns.clone(), summary: conversation.summary.clone() }
    }

    /// Whether there is nothing to show.
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty() && self.summary.is_none()
    }

    /// Whether a turn may be retried: it failed or conflicted, in a way another attempt could fix.
    pub fn can_retry(&self, status: RunStatus) -> bool {
        matches!(status, RunStatus::Failed | RunStatus::Conflict | RunStatus::Cancelled) && !self.runs.is_empty()
    }
}

/// The requirements the panel lists, from the command that owns them.
#[derive(Clone, Debug, Default)]
pub struct RequirementsList {
    /// One entry per document: id, title and counts.
    pub documents: Vec<RequirementsEntry>,
    /// The cap, so the panel can say when it is reached.
    pub max: usize,
    /// The document the window is showing, in full.
    ///
    /// The list gives counts; the reading form needs the requirements themselves. One load per frame
    /// of the open document is what makes the window readable without a second command per field.
    pub open_document: Option<RequirementsDocument>,
}

/// One requirements document in the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequirementsEntry {
    pub id: String,
    pub title: String,
    pub requirements: usize,
    pub acceptance_criteria: usize,
}

impl RequirementsList {
    /// Read the list from a `requirements.list` result.
    pub fn from_result(v: &Value) -> Self {
        let documents = v
            .get("documents")
            .and_then(Value::as_array)
            .map(|docs| {
                docs.iter()
                    .filter_map(|d| {
                        Some(RequirementsEntry {
                            id: d.get("id")?.as_str()?.to_string(),
                            title: d.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
                            requirements: d.get("requirements").and_then(Value::as_u64).unwrap_or(0) as usize,
                            acceptance_criteria: d.get("acceptanceCriteria").and_then(Value::as_u64).unwrap_or(0) as usize,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { documents, max: v.get("max").and_then(Value::as_u64).unwrap_or(32) as usize, open_document: None }
    }

    /// Whether the cap is reached, so creating another is refused before trying.
    pub fn is_full(&self) -> bool {
        self.documents.len() >= self.max
    }
}

/// What the panel asks its host to do.
///
/// The panel produces requests; the app performs them. That keeps the egui frame from calling a
/// model or blocking, and makes the whole state machine testable without a window.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// Run a turn for `request`.
    Send { request: String },
    /// Stop the running turn.
    Cancel,
    /// Run the last request again (a failure or a revision conflict).
    Retry,
    /// Start a new conversation.
    NewConversation,
    /// Load a requirements document into the editor.
    OpenRequirements { id: String },
    /// Save the draft through `requirements.upsert`.
    SaveRequirements { params: Value },
    /// Delete a requirements document.
    DeleteRequirements { id: String },
    /// Export a requirements document as Markdown.
    ExportRequirements { id: String },
    /// Leave the requirements editor.
    CloseRequirements,
    /// Show the Requirements tab.
    ShowRequirements,
    /// Apply a replay whose commit was refused because the document had moved on.
    ///
    /// A person's decision, never automatic: the document changed, so what the Agent built may be out
    /// of date, but it is their work to keep or drop.
    ApplyRefused,
}

impl AgentPanel {
    /// What the user asked for by clicking, if anything.
    ///
    /// Called once per frame after the panel draws; the app takes the requests and performs them, so
    /// no work happens inside the draw.
    pub fn take_requests(&mut self, requests: &mut Vec<Request>) {
        for r in requests.drain(..) {
            match &r {
                Request::Send { request } => {
                    self.draft.clear();
                    self.status = RunStatus::Streaming;
                    self.error = None;
                    self.last_undo_label = None;
                    self.tools.clear();
                    let _ = request;
                }
                Request::CloseRequirements => {
                    self.editing = None;
                    self.requirements_draft = None;
                }
                Request::OpenRequirements { id } => self.editing = Some(id.clone()),
                _ => {}
            }
        }
    }
}

/// The cancellation token for the running turn, kept out of `ui.inspect`.
#[derive(Clone, Debug, Default)]
pub struct Running {
    pub token: Option<CancelToken>,
}

impl Running {
    /// Start tracking a turn's cancellation.
    pub fn begin(&mut self) -> CancelToken {
        let token = CancelToken::new();
        self.token = Some(token.clone());
        token
    }

    /// Cancel the running turn, if any.
    pub fn cancel(&mut self) -> bool {
        match self.token.take() {
            Some(t) => {
                t.cancel();
                true
            }
            None => false,
        }
    }

    /// The turn ended.
    pub fn finish(&mut self) {}
}

/// The transcript a panel shows for its conversation, from a file's transcript.
pub fn conversation_view(transcript: &Transcript, id: &str) -> ChatView {
    match transcript.conversation(id) {
        Some(c) => ChatView::of(c),
        None => ChatView::default(),
    }
}

/// The conversation id a document uses: one per file, so reopening resumes the same chat.
pub fn conversation_id(doc_uid: u64) -> String {
    format!("doc-{doc_uid}")
}

impl AgentPanel {
    /// Show the newest turn from a transcript as this panel's current one.
    ///
    /// A turn that was running when the file was saved has `outcome: None`; it reopens as idle with
    /// its request shown, not as a turn that is still streaming -- nothing is running after a reopen.
    pub fn settle_unfinished(&mut self, transcript: &figcraft_agent::history::Transcript) {
        self.tools.clear();
        self.error = None;
        self.last_undo_label = None;
        let newest = transcript.conversations.iter().flat_map(|c| c.turns.iter()).next_back();
        match newest {
            Some(run) => {
                self.current = Some(run.clone());
                self.status = status_of(Some(run));
            }
            None => {
                self.current = None;
                self.status = RunStatus::Idle;
            }
        }
    }
}

/// The starting status of a turn read back from a file: an unfinished one is interrupted, not
/// running.
pub fn status_of(run: Option<&AgentRun>) -> RunStatus {
    match run.and_then(|r| r.outcome) {
        Some(Outcome::Completed | Outcome::Committed) => RunStatus::Done,
        Some(Outcome::Conflict) => RunStatus::Conflict,
        Some(Outcome::Failed) => RunStatus::Failed,
        Some(Outcome::Cancelled) => RunStatus::Cancelled,
        Some(Outcome::Interrupted) | None => RunStatus::Idle,
    }
}

// ---------- drawing ----------

use crate::a11y::Tip as _;
use crate::icons::Icon;
use crate::theme::Tokens;
use crate::widgets;
use egui::{Align, Align2, CornerRadius, Id, Layout, Rect, Sense, Stroke, Ui, pos2, vec2};

/// What a connection test is doing, kept across frames.
///
/// Persisted rather than a local: the test runs on a thread, so its result arrives on a later frame
/// than the click that started it. A local would be gone by then, and the button would appear to do
/// nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AgentConnectionState {
    /// Nothing tested since the settings last changed.
    #[default]
    Idle,
    /// A test is running; the button is disabled so it cannot be started twice.
    Testing,
    /// The provider answered.
    Ready,
    /// It did not, and this is why.
    Failed(String),
    /// The person stopped it.
    Cancelled,
}

impl AgentConnectionState {
    /// From what the host reported.
    pub fn from_status(status: figcraft_agent::runtime::ConnectionStatus) -> Self {
        match status {
            figcraft_agent::runtime::ConnectionStatus::Ready => Self::Ready,
            figcraft_agent::runtime::ConnectionStatus::Failed(why) => Self::Failed(why),
            figcraft_agent::runtime::ConnectionStatus::Cancelled => Self::Cancelled,
        }
    }

    /// What to show beside the button, if anything.
    pub fn message(&self) -> Option<String> {
        match self {
            Self::Idle => None,
            Self::Testing => Some(crate::i18n::tr("Testing…").to_string()),
            Self::Ready => Some(crate::i18n::tr("The provider answered.").to_string()),
            Self::Failed(why) => Some(why.clone()),
            Self::Cancelled => Some(crate::i18n::tr("The test was stopped.").to_string()),
        }
    }

    /// Whether a test is running.
    pub fn is_testing(&self) -> bool {
        matches!(self, Self::Testing)
    }
}

/// The panel's own width, so it can sit beside the canvas like the other panels.
pub const AGENT_PANEL_WIDTH: f32 = 320.0;
/// The narrowest the panel is drawn: below this, controls would overlap.
pub const AGENT_PANEL_MIN: f32 = 260.0;

/// State the panel needs from the app to draw, gathered before the mutable borrow.
#[derive(Clone, Debug, Default)]
pub struct PanelInput {
    /// Whether a provider is configured (the send button is disabled and Preferences offered).
    pub configured: bool,
    /// The requirements documents in this file.
    pub requirements: RequirementsList,
    /// The chat to show.
    pub chat: ChatView,
}

/// What a drawn frame asks the app to do.
#[derive(Clone, Debug, Default)]
pub struct PanelOutput {
    /// The requests the user made.
    pub requests: Vec<Request>,
}

impl PanelOutput {
    /// Add a request.
    pub fn push(&mut self, r: Request) {
        self.requests.push(r);
    }
}

/// Draw the Agent panel into `ui`.
///
/// No work happens here: the panel only records what the user asked for in [`PanelOutput`], which the
/// app performs after the frame. That keeps a model call and a native file dialog out of a draw.
pub fn panel(ui: &mut Ui, state: &mut AgentPanel, input: &PanelInput) -> PanelOutput {
    let t = Tokens::get(ui.ctx());
    let mut out = PanelOutput::default();
    let full = ui.max_rect();
    let width = full.width().max(AGENT_PANEL_MIN);

    // Header: the two tabs, so switching is one click.
    let (head, _) = ui.allocate_exact_size(vec2(width, 56.0), Sense::hover());
    ui.painter().text(
        pos2(head.min.x + 16.0, head.min.y + 20.0),
        Align2::LEFT_CENTER,
        crate::i18n::tr("Agent"),
        crate::theme::semibold(13.0),
        t.text,
    );
    // A new conversation, on the right of the header: it was reachable once and the rewrite left it
    // without a control, which left the request type with a handler and no way to send it.
    if state.tab == AgentTab::Chat {
        let r = Rect::from_min_size(pos2(head.max.x - 96.0, head.min.y + 12.0), vec2(80.0, 20.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(Layout::left_to_right(Align::Center)));
        let new = child.add(egui::Button::new(egui::RichText::new(crate::i18n::tr("New")).size(10.0)).min_size(vec2(80.0, 20.0)));
        if new.on_hover_text(crate::i18n::tr("New conversation")).clicked() {
            out.push(Request::NewConversation);
        }
    }
    let mut x = head.min.x + 16.0;
    for (tab, label) in [(AgentTab::Chat, "Chat"), (AgentTab::Requirements, "Requirements")] {
        let w = if tab == AgentTab::Chat { 52.0 } else { 116.0 };
        let r = Rect::from_min_size(pos2(x, head.min.y + 30.0), vec2(w, 22.0));
        let resp = ui.interact(r, Id::unique(("agent-tab", w as u32)), Sense::click()).tip(crate::i18n::tr(label));
        crate::a11y::tab(&resp, crate::i18n::tr(label), state.tab == tab);
        if state.tab == tab {
            ui.painter().rect_filled(r, CornerRadius::same(4), t.selected_row);
            ui.painter().text(r.center(), Align2::CENTER_CENTER, crate::i18n::tr(label), crate::theme::semibold(11.0), t.accent_text);
        } else {
            ui.painter().text(r.center(), Align2::CENTER_CENTER, crate::i18n::tr(label), crate::theme::regular(11.0), t.text_secondary);
        }
        if resp.clicked() {
            state.tab = tab;
            if tab == AgentTab::Chat {
                state.export_note = None;
            }
        }
        x += w + 6.0;
    }
    ui.painter().line_segment([pos2(full.min.x, head.max.y), pos2(full.max.x, head.max.y)], Stroke::new(1.0, t.border));

    let body = Rect::from_min_max(pos2(full.min.x, head.max.y), full.max);
    match state.tab {
        AgentTab::Chat => draw_chat(ui, state, input, &t, body, &mut out),
        AgentTab::Requirements => draw_requirements(ui, state, input, &t, body, &mut out),
    }
    out
}

/// The Chat tab: transcript, tool rows, composer and the send/stop controls.
fn draw_chat(ui: &mut Ui, state: &mut AgentPanel, input: &PanelInput, t: &Tokens, body: Rect, out: &mut PanelOutput) {
    // The composer's height comes from what it holds: a two-row field, the button row and the margins.
    // It was a hard 104 px, which took a fifth of a short panel for a field that needed 84, and still
    // showed only two lines of a long request.
    const COMPOSER: f32 = 92.0;
    let transcript = Rect::from_min_max(body.min, pos2(body.max.x, body.max.y - COMPOSER));
    let composer = Rect::from_min_max(pos2(body.min.x, transcript.max.y), body.max);

    // A window is not opaque by itself: its frame paints the border and the rounded corners, and
    // anything inside that does not paint shows the canvas through. The transcript has to say it is
    // part of the panel, or a conversation sits over a see-through hole.
    ui.painter().rect_filled(transcript, CornerRadius::ZERO, t.panel);

    // The transcript scrolls. It is the one part of this panel that grows without limit, and drawing it
    // to fixed geometry meant a long conversation simply ran off the bottom with no way to reach it.
    let mut area = ui.new_child(egui::UiBuilder::new().max_rect(transcript).layout(Layout::top_down(Align::Min)));
    // An explicit height, for the same reason as the requirements form: a `new_child` Ui does not
    // report the room its rect offers, so the scroll area saw an unbounded height and a long
    // conversation ran off the bottom with no way to reach it.
    let transcript_height = (transcript.height() - 4.0).max(32.0);
    egui::ScrollArea::vertical().max_height(transcript_height).auto_shrink([false, false]).stick_to_bottom(true).show(&mut area, |ui| {
        // All four sides: content against the panel's border reads as clipped.
        egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 12, top: 12, bottom: 10 }).show(ui, |ui| {
            // Room to breathe between turns: 6 packed a long conversation into a wall of text, which is
            // most of why the panel read as cramped.
            ui.spacing_mut().item_spacing.y = 10.0;

            // The summary compaction wrote, so an elided history is visible rather than surprising.
            if let Some(summary) = &input.chat.summary {
                bubble(ui, t, crate::i18n::tr("Earlier turns summarized"), summary, Kind::Note);
            }

            if input.chat.is_empty() && state.current.is_none() {
                // An empty panel has to say what to do next, in text dark enough to read. One dim line in
                // the middle of a large empty area reads as "this is broken", not "type below".
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new(crate::i18n::tr("Nothing here yet")).size(13.0).color(t.text));
                    ui.add_space(6.0);
                    let hint = if input.configured {
                        crate::i18n::tr("Describe what to build, then press Send")
                    } else {
                        crate::i18n::tr("Set the provider in Preferences first")
                    };
                    ui.label(egui::RichText::new(hint).size(11.0).color(t.text_secondary));
                    if input.configured {
                        ui.add_space(14.0);
                        // A first thing to try: an empty box with no example is where people stop.
                        ui.label(egui::RichText::new(crate::i18n::tr("Try")).size(10.0).color(t.text_tertiary));
                        for example in [
                            crate::i18n::tr("Draw a card with a title and a button"),
                            crate::i18n::tr("Make the selected frame blue"),
                            crate::i18n::tr("Write requirements for this screen"),
                        ] {
                            if ui
                                .add(egui::Label::new(egui::RichText::new(example).size(10.0).color(t.text_secondary)).sense(Sense::click()))
                                .clicked()
                            {
                                out.push(Request::Send { request: example.to_string() });
                            }
                        }
                    }
                });
            }

            // Finished turns, oldest first.
            for run in &input.chat.runs {
                bubble(ui, t, &run.request, &run.request, Kind::Mine);
                tools(ui, t, &run.tools, false);
                if !run.answer.is_empty() {
                    bubble(ui, t, "", &run.answer, Kind::Answer);
                }
                if let Some(outcome) = run.outcome {
                    outcome_line(ui, t, outcome);
                }
            }

            // The turn in flight.
            if let Some(run) = state.current.clone() {
                let running = state.is_running();
                if !state.reasoning.is_empty() {
                    reasoning(ui, state, t);
                }
                if !run.answer.is_empty() {
                    bubble(ui, t, "", &run.answer, Kind::Answer);
                }
                tools(ui, t, &state.tools, running);
                // What it is doing right now, animated. A turn runs for minutes with nothing on screen
                // saying so, and a panel that looks idle while a model is working reads as broken.
                if running {
                    working(ui, t, state);
                }
            } else if !state.tools.is_empty() {
                tools(ui, t, &state.tools, false);
            }

            for note in &state.notices {
                ui.label(egui::RichText::new(note).size(9.5).color(t.text_tertiary));
            }
            // A turn that wrote a requirements document says so and offers to show it: the answer reads
            // "写好了" otherwise, and the document is in a tab the person is not looking at.
            if let Some(written) = &state.wrote_requirements {
                egui::Frame::NONE.fill(t.field).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::symmetric(9, 7)).show(ui, |ui| {
                    ui.label(egui::RichText::new(crate::i18n::tr("Wrote a requirements document")).size(10.0).color(t.text));
                    ui.label(egui::RichText::new(written.as_str()).size(9.5).color(t.text_secondary));
                    ui.add_space(2.0);
                    if ui.add(egui::Button::new(crate::i18n::tr("Open Requirements")).min_size(vec2(120.0, 22.0))).clicked() {
                        out.push(Request::ShowRequirements);
                    }
                });
            }
            if let Some(error) = &state.error {
                egui::Frame::NONE.fill(t.field).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::symmetric(9, 7)).show(ui, |ui| {
                    ui.label(egui::RichText::new(error).size(10.0).color(t.text));
                    // The work is still here: a refused replay can be applied on request. Without this the
                    // only path is running the request again and waiting the same minutes.
                    if state.recoverable.is_some() {
                        ui.add_space(4.0);
                        if ui.add(egui::Button::new(crate::i18n::tr("Apply what it built")).min_size(vec2(120.0, 22.0))).clicked() {
                            out.push(Request::ApplyRefused);
                        }
                    }
                });
            }
        });
    });

    // The composer, pinned below the scroll area so it never scrolls away.
    draw_composer(ui, state, input, t, composer, out);
}

/// What a bubble is: who it belongs to, or that it is neither.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// The person's own words.
    Mine,
    /// The model's answer.
    Answer,
    /// Context the panel added: a summary, a notice.
    Note,
}

/// One message, as native egui widgets so its text wraps, can be selected and copied, and takes the
/// room it needs.
///
/// The panel used to paint every message with `painter().text()` at a height guessed from a character
/// count. Long answers were elided mid-sentence and could not be selected, which is a poor way to read
/// something a model wrote for you.
fn bubble(ui: &mut Ui, t: &Tokens, title: &str, text: &str, kind: Kind) {
    let (fill, stroke, align, text_colour, corner) = match kind {
        Kind::Mine => (t.field, None, Align::Max, t.text, CornerRadius { nw: 8, ne: 8, sw: 8, se: 2 }),
        Kind::Answer => (t.panel, Some(t.border), Align::Min, t.text, CornerRadius { nw: 8, ne: 8, sw: 2, se: 8 }),
        Kind::Note => (t.field, None, Align::Min, t.text_tertiary, CornerRadius::same(6)),
    };
    let width = ui.available_width();
    let max = if kind == Kind::Mine { width * 0.78 } else { width - 8.0 };
    ui.with_layout(Layout::top_down(align), |ui| {
        ui.set_max_width(max);
        let mut frame = egui::Frame::NONE.fill(fill).corner_radius(corner).inner_margin(egui::Margin::symmetric(11, 9));
        if let Some(stroke) = stroke {
            frame = frame.stroke(Stroke::new(1.0, stroke));
        }
        frame.show(ui, |ui| {
            ui.set_max_width(max - 22.0);
            if !title.is_empty() && title != text {
                ui.label(egui::RichText::new(title).size(11.0).color(text_colour));
            }
            // `Label` with wrapping is what makes a long answer readable and selectable.
            ui.add(egui::Label::new(egui::RichText::new(text).size(11.0).color(text_colour)).wrap());
        });
    });
}

/// A round icon button: filled with the accent when it will act, quiet when it will not.
///
/// Drawn rather than assembled from a `Button`, because the shape is the message: a circle at the end
/// of the composer is the one control a person reaches for without reading it.
fn round_button(ui: &mut Ui, t: &Tokens, icon: Icon, enabled: bool) -> egui::Response {
    let size = 26.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let hovered = enabled && resp.hovered();
    let fill = if !enabled {
        t.field
    } else if hovered {
        t.accent.gamma_multiply(0.85)
    } else {
        t.accent
    };
    ui.painter().circle_filled(rect.center(), size / 2.0, fill);
    let tint = if enabled { t.accent_on } else { t.text_tertiary };
    crate::icons::paint(ui.painter(), Rect::from_center_size(rect.center(), vec2(14.0, 14.0)), icon, tint);
    // A disabled control says why it is disabled; a click is only reported when it can act.
    if enabled { resp } else { resp.on_hover_text(crate::i18n::tr("Set the provider in Preferences")) }
}

/// The "still working" line: a spinner and what the turn is doing at the moment.
///
/// The spinner is egui's, so it animates and requests its own repaint: a static dot would be
/// indistinguishable from a frozen window.
fn working(ui: &mut Ui, t: &Tokens, state: &AgentPanel) {
    ui.horizontal(|ui| {
        ui.add(egui::Spinner::new().size(11.0).color(t.accent));
        // The most recent tool says what it is on: no tool yet means the model is still reading the
        // request, which is the longest wait and the one most worth naming.
        let label = match state.tools.last() {
            Some(row) if row.ok => crate::i18n::tr("Working…"),
            Some(row) => tool_label(&row.name),
            None if !state.reasoning.is_empty() => crate::i18n::tr("Thinking…"),
            None => crate::i18n::tr("Reading the request…"),
        };
        ui.label(egui::RichText::new(label).size(9.5).color(t.text_secondary));
    });
}

/// What a tool call is called in the panel, in the interface's language.
///
/// The catalog names its tools in English because a model reads those definitions; a person reading
/// the panel should not have to. An unknown name falls back to the id, so a tool added to the catalog
/// still shows something rather than nothing.
fn tool_label(name: &str) -> &'static str {
    match name {
        "batch" => crate::i18n::tr("Edit several things at once"),
        "create_node" => crate::i18n::tr("Add a layer"),
        "set_properties" => crate::i18n::tr("Change properties"),
        "run_command" => crate::i18n::tr("Run a command"),
        "list_commands" => crate::i18n::tr("Look up commands"),
        "inspect_document" => crate::i18n::tr("Look at the document"),
        "get_node" => crate::i18n::tr("Look at a layer"),
        "select" => crate::i18n::tr("Select layers"),
        "screenshot" => crate::i18n::tr("Take a screenshot"),
        "type_text" => crate::i18n::tr("Type text"),
        "inspect_ui" => crate::i18n::tr("Look at the interface"),
        "list_requirements" => crate::i18n::tr("List requirements"),
        "get_requirements" => crate::i18n::tr("Read requirements"),
        "upsert_requirements" => crate::i18n::tr("Write requirements"),
        "render_requirements_markdown" => crate::i18n::tr("Render requirements"),
        _ => crate::i18n::tr("Working"),
    }
}

/// The tool calls of a turn, as quiet one-line strips.
fn tools(ui: &mut Ui, t: &Tokens, rows: &[ToolCall], running: bool) {
    if rows.is_empty() {
        return;
    }
    ui.with_layout(Layout::top_down(Align::Min), |ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.spacing_mut().item_spacing.x = 5.0;
        for (i, row) in rows.iter().enumerate() {
            let is_last = i + 1 == rows.len();
            let mark = if !row.ok {
                "·"
            } else if running && is_last {
                "◐"
            } else {
                "✓"
            };
            let colour = if row.ok { t.accent_text } else { t.text_tertiary };
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(mark).size(9.0).color(colour));
                ui.label(egui::RichText::new(tool_label(&row.name)).size(9.0).color(t.text_secondary));
            });
        }
    });
    ui.add_space(2.0);
}

/// The model's reasoning, behind a disclosure.
fn reasoning(ui: &mut Ui, state: &mut AgentPanel, t: &Tokens) {
    let arrow = if state.show_reasoning { "▾" } else { "▸" };
    let head = format!("{arrow}  {}", crate::i18n::tr("Thinking"));
    let label = ui.add(egui::Label::new(egui::RichText::new(head).size(9.5).color(t.text_tertiary)).sense(Sense::click()));
    if label.on_hover_text(crate::i18n::tr("Show how the model got there")).clicked() {
        state.show_reasoning = !state.show_reasoning;
    }
    if !state.show_reasoning {
        return;
    }
    // Bounded: a long deliberation is scrollable context, not the answer, so it is capped and the
    // newest part is shown.
    let shown: String = state.reasoning.chars().rev().take(6000).collect::<Vec<_>>().into_iter().rev().collect();
    egui::Frame::NONE.fill(t.field).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
        egui::ScrollArea::vertical().max_height(220.0).auto_shrink([false, true]).id_salt("agent-reasoning-scroll").show(ui, |ui| {
            ui.add(egui::Label::new(egui::RichText::new(shown).size(9.5).color(t.text_tertiary)).wrap());
        });
    });
}

/// What became of a turn, as one quiet line.
fn outcome_line(ui: &mut Ui, t: &Tokens, outcome: Outcome) {
    let (label, colour) = match outcome {
        Outcome::Committed => (crate::i18n::tr("Applied · one undo step"), t.text_tertiary),
        Outcome::Completed => (crate::i18n::tr("Answered · no changes"), t.text_tertiary),
        Outcome::Conflict => (crate::i18n::tr("Not applied · the file changed"), t.accent),
        Outcome::Failed => (crate::i18n::tr("Failed"), t.accent),
        Outcome::Cancelled => (crate::i18n::tr("Stopped"), t.text_tertiary),
        Outcome::Interrupted => (crate::i18n::tr("Interrupted"), t.text_tertiary),
    };
    ui.label(egui::RichText::new(label).size(9.0).color(colour));
}

/// The composer, pinned under the transcript.
///
/// Native widgets: a `TextEdit` that wraps, real buttons that show their own hover and disabled states,
/// and Enter to send (Shift+Enter for a newline) -- what anyone expects of a chat box.
fn draw_composer(ui: &mut Ui, state: &mut AgentPanel, input: &PanelInput, t: &Tokens, area: Rect, out: &mut PanelOutput) {
    // The composer is part of the window too: without a fill the field sits over whatever is behind the
    // panel, and the divider line would be floating in the canvas.
    ui.painter().rect_filled(area, CornerRadius::ZERO, t.panel);
    ui.painter().line_segment([area.min, pos2(area.max.x, area.min.y)], Stroke::new(1.0, t.border));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area.shrink2(vec2(12.0, 8.0))).layout(Layout::top_down(Align::Min)));
    let ui = &mut child;
    ui.spacing_mut().item_spacing.y = 6.0;

    let mut text = state.draft.clone();
    let edit = egui::TextEdit::multiline(&mut text).hint_text(crate::i18n::tr("Describe a change…")).desired_width(f32::INFINITY).desired_rows(3);
    let resp = ui.add(edit);
    if resp.changed() {
        state.draft = text;
    }
    crate::a11y::text(&resp, crate::i18n::tr("Message to the Agent"), &state.draft);

    let can_send = state.can_send() && input.configured;
    // Enter sends, Shift+Enter breaks the line: the keyboard habit every chat box has.
    let submitted = resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);

    ui.horizontal(|ui| {
        // One round button that says what it will do: an arrow when a message can go, a pause bar
        // while one is running. The same spot, the same size, so the muscle memory works.
        if state.is_running() {
            if round_button(ui, t, Icon::Pause, true).on_hover_text(crate::i18n::tr("Stop")).clicked() {
                out.push(Request::Cancel);
            }
        } else {
            let send = round_button(ui, t, Icon::Send, can_send);
            let send =
                send.on_hover_text(if input.configured { crate::i18n::tr("Send") } else { crate::i18n::tr("Set the provider in Preferences") });
            if (send.clicked() || submitted) && can_send {
                out.push(Request::Send { request: state.draft.clone() });
            }
        }
        if !state.is_running() && input.chat.can_retry(state.status) {
            let retry = ui.add(egui::Button::new(crate::i18n::tr("Retry")).min_size(vec2(56.0, 24.0)));
            if retry.on_hover_text(crate::i18n::tr("Run it again")).clicked() {
                out.push(Request::Retry);
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(label) = &state.last_undo_label {
                ui.label(egui::RichText::new(label.as_str()).size(9.0).color(t.text_tertiary));
            }
        });
    });
}

/// The Requirements tab: the list, or one document's fields.
fn draw_requirements(ui: &mut Ui, state: &mut AgentPanel, input: &PanelInput, t: &Tokens, body: Rect, out: &mut PanelOutput) {
    ui.painter().rect_filled(body, CornerRadius::ZERO, t.panel);
    let mut y = body.min.y + 12.0;

    if state.requirements_draft.is_some() {
        // Take the draft out so the editor can borrow the panel's other fields at the same time.
        let mut draft = match state.requirements_draft.take() {
            Some(d) => d,
            None => return,
        };
        draw_requirements_editor(ui, state, input, t, body, &mut draft, &mut y, out);
        // The editor either keeps the draft (an edit in progress) or asks to close it, in which
        // case `take_requests` clears `editing` after this frame.
        if state.requirements_draft.is_none() {
            state.requirements_draft = Some(draft);
        }
        return;
    }

    // The list, with what each document holds: a row that says only "14 · 47" makes the person open
    // every one to find what they are after.
    if input.requirements.documents.is_empty() {
        ui.add_space(28.0);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("No requirements yet")).size(12.0).color(t.text));
            ui.add_space(6.0);
            ui.label(egui::RichText::new(crate::i18n::tr("Ask the Agent to write one, or press New below")).size(10.0).color(t.text_secondary));
        });
    }
    let mut area = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_max(pos2(body.min.x, y), pos2(body.max.x, body.max.y - 40.0)))
            .layout(Layout::top_down(Align::Min)),
    );
    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("requirements-list").show(&mut area, |ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        for doc in &input.requirements.documents {
            let r = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), 44.0));
            let resp = ui.interact(r, Id::unique(("req-row", &doc.id)), Sense::click());
            let resp = resp.on_hover_text(crate::i18n::tr("Open"));
            if resp.hovered() {
                ui.painter().rect_filled(r, CornerRadius::same(5), t.hover);
            }
            ui.painter().text(
                pos2(r.min.x + 9.0, r.min.y + 11.0),
                Align2::LEFT_CENTER,
                widgets::elide_font(ui, &doc.title, r.width() - 70.0, crate::theme::semibold(11.5)),
                crate::theme::semibold(11.5),
                t.text,
            );
            // The id, so a person can name the document to the Agent.
            ui.painter().text(pos2(r.max.x - 9.0, r.min.y + 11.0), Align2::RIGHT_CENTER, &doc.id, crate::theme::regular(9.0), t.text_tertiary);
            let counts =
                format!("{} {} · {} {}", doc.requirements, crate::i18n::tr("requirements"), doc.acceptance_criteria, crate::i18n::tr("criteria"),);
            ui.painter().text(pos2(r.min.x + 9.0, r.min.y + 30.0), Align2::LEFT_CENTER, counts, crate::theme::regular(9.0), t.text_tertiary);
            if resp.clicked() {
                out.push(Request::OpenRequirements { id: doc.id.clone() });
            }
            ui.add_space(44.0);
        }
    });
    let _ = y;

    // New document: refused before trying when the file is full.
    let new_r = Rect::from_min_size(pos2(body.min.x + 12.0, body.max.y - 34.0), vec2(body.width() - 24.0, 24.0));
    let full = input.requirements.is_full();
    let resp = ui.interact(new_r, Id::unique("req-new"), Sense::click()).tip(if full {
        crate::i18n::tr("This file holds the most documents it can")
    } else {
        crate::i18n::tr("New requirements document")
    });
    ui.painter().rect_filled(new_r, CornerRadius::same(5), if full { t.field } else { t.accent });
    ui.painter().text(
        new_r.center(),
        Align2::CENTER_CENTER,
        crate::i18n::tr("New document"),
        crate::theme::semibold(11.0),
        if full { t.text_tertiary } else { t.accent_on },
    );
    if resp.clicked() && !full {
        state.requirements_draft = Some(RequirementsDraft::new());
    }
    let _ = y;
}

/// One requirements document's fields: save, export and delete.
fn draw_requirements_editor(
    ui: &mut Ui,
    state: &mut AgentPanel,
    input: &PanelInput,
    t: &Tokens,
    body: Rect,
    draft: &mut RequirementsDraft,
    _y: &mut f32,
    out: &mut PanelOutput,
) {
    // A fixed action bar at the bottom and a scrolling form above it. The old version painted every
    // field at a hand-computed y with `painter().text()`, so a long title ran across the field below
    // it and the Back button -- drawn at `body.max.y - 34`, below seven fields -- fell off the panel
    // as soon as the form was taller than the window.
    const BAR: f32 = 78.0;
    let form = Rect::from_min_max(body.min, pos2(body.max.x, body.max.y - BAR));
    let bar = Rect::from_min_max(pos2(body.min.x, form.max.y), body.max);

    // The scroll area is given an explicit height. A `new_child` Ui does not report the room its rect
    // offers, so a `ScrollArea` inside one got an unbounded available height and never scrolled: the
    // form simply drew past the bottom of the panel. `max_height` is the height the form actually has.
    // `allocate_new_ui` gives the child a real interaction context, which a bare `new_child` does
    // not: without it the `ScrollArea` never saw the pointer over itself, so the wheel did nothing.
    let form_height = (form.height() - 4.0).max(32.0);
    // `scope_builder` allocates the space it uses back into the parent, so the child has a real rect
    // and interaction context. A bare `new_child` does neither, and a `ScrollArea` inside one never
    // saw the pointer over itself -- which is why the wheel did nothing at all.
    ui.scope_builder(egui::UiBuilder::new().max_rect(form), |ui| {
        egui::ScrollArea::vertical().max_height(form_height).auto_shrink([false, false]).id_salt("requirements-editor").show(ui, |ui| {
            // A frame is what insets on all four sides: `add_space` before the content only moved it down,
            // so the labels still sat against the panel's left border.
            egui::Frame::NONE.inner_margin(egui::Margin { left: 14, right: 14, top: 12, bottom: 10 }).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 9.0;
                let width = (ui.available_width() - 6.0).max(80.0);

                /// A titled section holding its own fields.
                ///
                /// The heading and its one-line explanation are what turn a column of boxes into a document
                /// with parts; a reader can find the part they want without reading all of it.
                fn group(ui: &mut Ui, t: &Tokens, title: &str, note: &str, add: impl FnOnce(&mut Ui, f32), width: f32) {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(title).size(11.5).strong().color(t.text));
                    ui.label(egui::RichText::new(note).size(9.0).color(t.text_tertiary));
                    ui.add_space(4.0);
                    // A rule under the heading, so a long section has a visible start.
                    let (r, _) = ui.allocate_exact_size(vec2(width, 1.0), Sense::hover());
                    ui.painter().line_segment([r.left_center(), r.right_center()], Stroke::new(1.0, t.border));
                    ui.add_space(6.0);
                    add(ui, width);
                }

                /// A labelled multi-line field: a label, then the box, both in the flow.
                fn field(ui: &mut Ui, t: &Tokens, label: &str, value: &mut String, rows: usize, width: f32) {
                    ui.label(egui::RichText::new(label).size(9.5).color(t.text_tertiary));
                    ui.add(
                        egui::TextEdit::multiline(value)
                            .desired_width(width)
                            .desired_rows(rows)
                            .frame(egui::Frame::NONE.fill(t.field).corner_radius(CornerRadius::same(4)).inner_margin(egui::Margin::symmetric(6, 5))),
                    );
                }

                // Grouped, not a single column of boxes: a requirements document has sections, and a
                // form that runs eleven fields together is read as one undifferentiated block.
                group(
                    ui,
                    t,
                    crate::i18n::tr("Overview"),
                    crate::i18n::tr("What this document is about"),
                    |ui, width| {
                        field(ui, t, crate::i18n::tr("Title"), &mut draft.title, 1, width);
                        field(ui, t, crate::i18n::tr("Summary"), &mut draft.summary, 2, width);
                    },
                    width,
                );
                group(
                    ui,
                    t,
                    crate::i18n::tr("Scope"),
                    crate::i18n::tr("What this does and does not do, and for whom"),
                    |ui, width| {
                        field(ui, t, crate::i18n::tr("Goals (one per line)"), &mut draft.goals, 3, width);
                        field(ui, t, crate::i18n::tr("Non-goals"), &mut draft.non_goals, 2, width);
                        field(ui, t, crate::i18n::tr("Audiences"), &mut draft.audiences, 2, width);
                    },
                    width,
                );
                group(
                    ui,
                    t,
                    crate::i18n::tr("Requirements"),
                    crate::i18n::tr("One per line: title, priority, detail"),
                    |ui, width| {
                        field(ui, t, crate::i18n::tr("Requirements (title, priority, detail)"), &mut draft.requirements, 7, width);
                    },
                    width,
                );
                group(
                    ui,
                    t,
                    crate::i18n::tr("Behaviour"),
                    crate::i18n::tr("How it behaves: flows, states and the data it handles"),
                    |ui, width| {
                        field(ui, t, crate::i18n::tr("User flows (action -> response (timing))"), &mut draft.user_flows, 4, width);
                        field(ui, t, crate::i18n::tr("Glossary (term: meaning)"), &mut draft.glossary, 3, width);
                    },
                    width,
                );
                group(
                    ui,
                    t,
                    crate::i18n::tr("Verification"),
                    crate::i18n::tr("How it is known to be done, and what is still open"),
                    |ui, width| {
                        field(ui, t, crate::i18n::tr("Acceptance criteria"), &mut draft.acceptance_criteria, 3, width);
                        field(ui, t, crate::i18n::tr("Open questions"), &mut draft.open_questions, 2, width);
                    },
                    width,
                );
                if let Some(note) = &state.export_note {
                    ui.label(egui::RichText::new(note.as_str()).size(9.0).color(t.text_tertiary));
                }
            });
        });
    });

    // The action bar: part of the panel, so it is always on screen however long the form is.
    ui.painter().rect_filled(bar, CornerRadius::ZERO, t.panel);
    ui.painter().line_segment([bar.min, pos2(bar.max.x, bar.min.y)], Stroke::new(1.0, t.border));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(10.0, 7.0))).layout(Layout::top_down(Align::Min)));
    let ui = &mut child;
    ui.spacing_mut().item_spacing.y = 5.0;

    // Back is first and always present: leaving a form must never depend on the form's height.
    if ui.add(egui::Button::new(crate::i18n::tr("Back to the list")).min_size(vec2(0.0, 22.0))).clicked() {
        out.push(Request::CloseRequirements);
    }
    ui.horizontal(|ui| {
        let saveable = draft.is_saveable() && !state.is_running();
        if ui.add_enabled(saveable, egui::Button::new(crate::i18n::tr("Save")).min_size(vec2(60.0, 22.0))).clicked() {
            out.push(Request::SaveRequirements { params: draft.to_params() });
        }
        // Export needs a saved document: an unsaved one has no id.
        let can_export = draft.id.is_some();
        let export = ui.add_enabled(can_export, egui::Button::new(crate::i18n::tr("Export")).min_size(vec2(60.0, 22.0)));
        if export.on_hover_text(crate::i18n::tr("Export as Markdown")).clicked()
            && let Some(id) = draft.id.clone()
        {
            out.push(Request::ExportRequirements { id });
        }
        if ui.add(egui::Button::new(crate::i18n::tr("Delete")).min_size(vec2(58.0, 22.0))).clicked() {
            match draft.id.clone() {
                Some(id) => out.push(Request::DeleteRequirements { id }),
                None => out.push(Request::CloseRequirements),
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(id) = &draft.id {
                ui.label(egui::RichText::new(id.as_str()).size(9.0).color(t.text_tertiary));
            }
        });
    });
    let _ = input;
}

// ---------- the window ----------

/// Show the Agent panel while it is open (call once per frame).
///
/// The window form follows the other side panels (Variables, Dev compare): a titled window over the
/// canvas rather than a column, so the Agent does not take canvas width away from a design.
pub fn show(app: &mut crate::FigApp, ctx: &egui::Context) {
    let Some(mut st) = app.ui.agent.take() else { return };
    // Gather what the panel reads before borrowing it mutably: the draw must not touch the session.
    let input = panel_input(app);
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    // The panel takes the height it can: leaving 96 px for the menu bar and the window's own margins
    // rather than a hard 720 cap. The cap cut a fifth off the conversation area on a 900 px screen,
    // which is exactly the part a person scrolls through.
    let height = (screen.height() - 96.0).clamp(320.0, 1600.0);
    let size = vec2(AGENT_PANEL_WIDTH.max(AGENT_PANEL_MIN).min(screen.width() - 80.0), height);
    let mut open = true;
    let mut output = PanelOutput::default();
    egui::Window::new(crate::i18n::tr("Agent"))
        .id(Id::unique("agent_window"))
        .title_bar(false)
        .collapsible(false)
        .resizable(true)
        .default_size(size)
        .min_size(vec2(AGENT_PANEL_MIN, 320.0))
        // No `.anchor()`: an anchored window cannot be dragged, and a panel that covers the canvas is
        // one a person needs to move out of the way. egui remembers where it was dragged to, so the
        // starting place is a `default_pos` and not a fixed one.
        .default_pos(pos2(screen.max.x - size.x - 16.0, screen.min.y + 64.0))
        .frame(egui::Frame::NONE.fill(t.panel).stroke(Stroke::new(1.0, t.border_strong)).corner_radius(CornerRadius::same(8)))
        .show(ctx, |ui| {
            // Close button, in the header row the panel draws.
            let head = Rect::from_min_size(ui.max_rect().min, vec2(ui.max_rect().width(), 20.0));
            let close = Rect::from_center_size(pos2(head.max.x - 14.0, head.min.y + 12.0), vec2(18.0, 18.0));
            let resp = ui.interact(close, Id::unique("agent-close"), Sense::click()).tip(crate::i18n::tr("Close"));
            ui.painter().text(close.center(), Align2::CENTER_CENTER, "×", crate::theme::regular(13.0), t.text_secondary);
            if resp.clicked() {
                open = false;
            }
            output = panel(ui, &mut st, &input);
        });
    // Requests are performed after the frame: a model call or a dialog never happens inside a draw.
    app.ui.agent_requests.append(&mut output.requests);
    if open {
        app.ui.agent = Some(st);
    }
}

/// Start a turn: snapshot the document, ask the host to run it, and remember the token.
///
/// The snapshot is what the worker may look at. It carries the document as JSON, its uid and its
/// revision, so the host never borrows the live `Session` and the commit can be refused if the
/// document changed while the Agent worked.
fn start_turn(app: &mut crate::FigApp, request: &str) {
    if app.ui.agent_host.is_none() {
        set_error(app, "this build has no Agent runtime");
        return;
    }
    // A turn already running is not replaced: the panel's Send is disabled then, but a queued
    // request or a keystroke could still arrive.
    if app.ui.agent_host.as_ref().is_some_and(|h| h.status() == crate::agent_host::HostStatus::Running) {
        return;
    }
    // The snapshot and the config are read before the host is borrowed: both need the session.
    let snapshot = match document_snapshot(app) {
        Ok(s) => s,
        Err(message) => {
            set_error(app, &message);
            return;
        }
    };
    // The provider settings come from the engine's preferences, which is where the user set them.
    let prefs = &app.session.prefs.agent;
    let config = match figcraft_agent::AgentConfig::from_prefs(prefs) {
        Ok(c) => c,
        Err(e) => {
            set_error(app, &format!("{e} {}", figcraft_agent::AgentErrorKind::Configuration.action()));
            return;
        }
    };
    // The turn is recorded when it starts, so a crash mid-turn leaves a record that reopens as
    // interrupted rather than as if it never happened.
    let conversation = conversation_id_of(app);
    let next_turn = app.ui.agent_turn.wrapping_add(1);
    begin_transcript_turn(&mut app.ui.agent_transcript, &conversation, next_turn, request);
    app.ui.agent_turn = next_turn;
    // Persist the started turn at once: a file saved while it runs must reopen with it there.
    store_transcript(app);
    let turn = app.ui.agent_turn;
    if let Some(panel) = app.ui.agent.as_mut() {
        panel.begin_turn(turn, request);
    }
    // Read before the host borrow: it needs the session and the transcript.
    let history = continuation_for(app);
    let host = match app.ui.agent_host.as_mut() {
        Some(h) => h,
        None => return,
    };
    // One token per turn: the host runs with it and the panel stops with it, so Stop works the same
    // on either platform.
    let cancel = figcraft_agent::run::CancelToken::new();
    let token = host.start(crate::agent_host::HostRequest::Start(Box::new(crate::agent_host::TurnRequest {
        turn,
        request: request.to_string(),
        snapshot,
        limits: figcraft_agent::run::Limits::default(),
        config,
        history,
        cancel,
    })));
    app.ui.agent_running.token = Some(token);
}

/// The live document as a snapshot the worker can build its shadow from.
pub(crate) fn document_snapshot(app: &mut crate::FigApp) -> Result<crate::agent_host::Snapshot, String> {
    let ds = app.session.ds().map_err(|e| e.to_string())?;
    let (uid, revision) = (ds.uid, ds.revision);
    // The document itself, in the shape the format crate writes: a host rebuilds a `Document` from
    // it. `inspect_document` would not do: its output is a report for a model (paths, history, a
    // dirty flag), not a document, and deserializing it back into one fails.
    let document = figcraft_format::to_json(ds.doc.as_ref()).map_err(|e| e.to_string())?;
    Ok(crate::agent_host::Snapshot { document, uid, revision })
}

fn set_error(app: &mut crate::FigApp, message: &str) {
    if let Some(panel) = app.ui.agent.as_mut() {
        panel.status = RunStatus::Failed;
        panel.error = Some(message.to_string());
    }
}

/// Drain the host's events into the panel, once per frame.
///
/// Called after the draw, so a frame never waits on the worker. When a turn succeeds, its envelope is
/// applied here, on the UI thread, which is the only thread that touches the live document.
pub fn poll_host(app: &mut crate::FigApp) {
    // A tab switch shows that file's conversation. Done here rather than in the draw so the panel
    // never borrows the session while painting.
    sync_transcript_from_document(app);
    let Some(host) = app.ui.agent_host.as_mut() else { return };
    // Bind the app's context the first time: a turn that streams while nothing else happens still
    // repaints. Done here because the context exists from the first frame, after the host is created.
    if let Some(ctx) = app.ui_context.clone() {
        host.set_wake(ctx);
    }
    let messages = host.drain();
    if messages.is_empty() {
        return;
    }
    let limits = figcraft_agent::run::Limits::default();
    // One pass, in order: progress then the ending, exactly as produced. A message about another turn
    // is ignored entirely -- it must not clear this turn's token, touch its transcript record or set
    // a conflict on it.
    for message in messages {
        if message.turn() != app.ui.agent_turn {
            continue;
        }
        match message {
            crate::agent_host::HostMessage::Progress(event) => {
                if let Some(panel) = app.ui.agent.as_mut() {
                    panel.apply(&event, &limits);
                }
            }
            crate::agent_host::HostMessage::Terminal(terminal) => settle_turn(app, &terminal),
            crate::agent_host::HostMessage::ConnectionFinished { request, status } => {
                apply_connection_result(app, request, status);
            }
        }
    }
}

/// Settle the panel from a connection test's result, if it is the one being waited for.
///
/// Split out so the rule has one home: an older test's answer must not settle a newer one, because
/// the settings it tested may already have been edited.
pub fn apply_connection_result(app: &mut crate::FigApp, request: u64, status: figcraft_agent::runtime::ConnectionStatus) {
    if request == app.ui.agent_connection_request {
        app.ui.agent_connection = AgentConnectionState::from_status(status);
        app.ui.agent_connection_cancel = None;
    }
}

/// Settle the turn a terminal belongs to: apply its envelope, then record and persist the outcome.
/// The requirements document a replay created or replaced, if it touched one.
///
/// `requirements.upsert` returns the saved document, and the tool's result carries it; when the replay
/// holds the command, the id is in its parameters (`id`) or is allocated by the command. Naming the
/// document is enough to point at it: the panel opens the list, which shows it.
fn requirements_written(terminal: &figcraft_agent::TurnTerminal) -> Option<String> {
    let figcraft_agent::TurnTerminal::Success { commit: Some(c), .. } = terminal else { return None };
    c.replay.iter().find(|s| s.command == "requirements.upsert").map(|s| {
        s.params
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map_or_else(|| s.params.get("title").and_then(serde_json::Value::as_str).unwrap_or("PRD").to_string(), str::to_string)
    })
}

fn settle_turn(app: &mut crate::FigApp, terminal: &figcraft_agent::TurnTerminal) {
    app.ui.agent_running.finish();
    if let Some(written) = requirements_written(terminal)
        && let Some(panel) = app.ui.agent.as_mut()
    {
        panel.wrote_requirements = Some(written);
    }
    // What actually happened to the document, which is the fact the transcript records.
    let disposition = apply_terminal(app, terminal);
    // Copied out first: recording needs the session (for the conversation id) and the transcript,
    // which cannot be borrowed while the panel is.
    let recorded = app
        .ui
        .agent
        .as_ref()
        .and_then(|panel| panel.current.as_ref().map(|run| (run.turn, run.request.clone(), run.answer.clone(), run.tools.clone())));
    if let Some((turn, request, answer, tools)) = recorded {
        record_turn(app, turn, request, answer, tools, &disposition);
    }
    // Rig's transcript becomes the next turn's history only when the turn's result was kept: a
    // rejected run's messages describe edits that did not land, and replaying them would have the
    // model believe its changes are in the document.
    let kept = matches!(disposition, ApplyDisposition::Completed | ApplyDisposition::Committed);
    if kept
        && let figcraft_agent::TurnTerminal::Success { continuation, .. } = terminal
        && !continuation.is_empty()
        && let Some(convo) = app.ui.agent_transcript.conversations.last_mut()
    {
        convo.continuation = continuation.clone();
    }
    store_transcript(app);
}

/// Report a turn's ending in the panel and, for a success, apply its envelope.
///
/// The envelope is rechecked here, at the one place that dispatches the batch: the turn id must be the
/// one the panel is showing (a late result from an older turn must not land), and the document uid and
/// revision must still be the ones the turn was built from. Checking at the snapshot instead would
/// leave a window for the document to change between the check and the write.
/// What applying a terminal actually did to the live document.
///
/// This is the fact the transcript records. The model's answer is not the same question: a turn can
/// answer successfully and change nothing, and a turn can answer successfully while its edits are
/// refused because the document moved on. Recording "committed" for either would claim an undo entry
/// that does not exist and tell the next turn that rejected edits landed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplyDisposition {
    /// The model answered and changed nothing.
    Completed,
    /// The model answered and its replay became one undo entry.
    Committed,
    /// The model answered but nothing was applied, and this is why.
    Conflict(String),
    /// It failed; nothing was applied.
    Failed,
    /// It was cancelled; nothing was applied.
    Cancelled,
}

impl ApplyDisposition {
    /// The transcript outcome this disposition means.
    pub fn outcome(&self) -> figcraft_agent::run::Outcome {
        match self {
            ApplyDisposition::Completed => figcraft_agent::run::Outcome::Completed,
            ApplyDisposition::Committed => figcraft_agent::run::Outcome::Committed,
            ApplyDisposition::Conflict(_) => figcraft_agent::run::Outcome::Conflict,
            ApplyDisposition::Failed => figcraft_agent::run::Outcome::Failed,
            ApplyDisposition::Cancelled => figcraft_agent::run::Outcome::Cancelled,
        }
    }
}

fn apply_terminal(app: &mut crate::FigApp, terminal: &figcraft_agent::TurnTerminal) -> ApplyDisposition {
    match terminal {
        figcraft_agent::TurnTerminal::Success { commit: Some(envelope), .. } => commit_turn(app, envelope),
        figcraft_agent::TurnTerminal::Success { commit: None, .. } => {
            // A read-only turn: it succeeded and changed nothing. `Completed`, not `Committed`:
            // claiming an undo entry that does not exist is a claim the transcript would repeat.
            if let Some(panel) = app.ui.agent.as_mut() {
                panel.status = RunStatus::Done;
            }
            ApplyDisposition::Completed
        }
        figcraft_agent::TurnTerminal::LimitReached { message, .. } => {
            if let Some(panel) = app.ui.agent.as_mut() {
                panel.status = RunStatus::Failed;
                panel.error = Some(message.clone());
            }
            ApplyDisposition::Failed
        }
        figcraft_agent::TurnTerminal::Failed { message, .. } => {
            if let Some(panel) = app.ui.agent.as_mut() {
                panel.status = RunStatus::Failed;
                panel.error = Some(message.clone());
            }
            ApplyDisposition::Failed
        }
        figcraft_agent::TurnTerminal::Cancelled { .. } => {
            if let Some(panel) = app.ui.agent.as_mut() {
                panel.status = RunStatus::Cancelled;
            }
            ApplyDisposition::Cancelled
        }
    }
}

/// Apply a success's envelope as exactly one undo step, after rechecking the turn and the document.
///
/// Three checks, all at the write: the turn is the one the panel shows (a late reply from a previous
/// turn would otherwise apply into a newer one), the file is the one the Agent inspected, and the
/// revision is exactly the one it was built from. Any mismatch applies nothing and says why.
fn commit_turn(app: &mut crate::FigApp, envelope: &figcraft_agent::CommitEnvelope) -> ApplyDisposition {
    if envelope.is_empty() {
        // A success with an empty envelope is a read-only turn that reported one anyway.
        if let Some(panel) = app.ui.agent.as_mut() {
            panel.status = RunStatus::Done;
        }
        return ApplyDisposition::Completed;
    }
    // The panel's current turn: a stale success must not land in a newer turn.
    let shown = app.ui.agent.as_ref().and_then(|p| p.current.as_ref()).map(|r| r.turn);
    if shown != Some(envelope.turn) {
        let why = "the Agent finished an older request, so its changes were not applied. Run the request again.";
        set_conflict(app, why);
        return ApplyDisposition::Conflict(why.to_string());
    }
    let Ok(ds) = app.session.ds() else {
        let why = "the file the Agent worked in is no longer open, so its changes were not applied.";
        set_conflict(app, why);
        return ApplyDisposition::Conflict(why.to_string());
    };
    let (uid, revision) = (ds.uid, ds.revision);
    if let Some(reason) = envelope.refusal(uid, revision) {
        // Kept so the work is not lost: the replay is absolute commands, so it can still be applied to
        // the document as it is now. The panel shows a button; nothing applies it on its own.
        if let Some(panel) = app.ui.agent.as_mut() {
            panel.recoverable = Some(envelope.clone());
        }
        let why = format!("{reason}, so its changes were not applied. Apply them anyway, or run the request again.");
        set_conflict(app, &why);
        return ApplyDisposition::Conflict(why);
    }
    let batch = serde_json::json!({ "label": envelope.undo_label, "commands": envelope.batch_commands() });
    match app.session.execute("command.batch", &batch) {
        Ok(_) => {
            if let Some(panel) = app.ui.agent.as_mut() {
                panel.last_undo_label = Some(envelope.undo_label.clone());
                panel.status = RunStatus::Done;
            }
            ApplyDisposition::Committed
        }
        // The batch rolls itself back, so nothing landed: this is a conflict, not a commit.
        Err(e) => {
            let why = format!("the Agent's changes could not be applied together: {e}");
            set_conflict(app, &why);
            ApplyDisposition::Conflict(why)
        }
    }
}

/// A turn whose result cannot be applied: its own status, because the offer is to rerun.
fn set_conflict(app: &mut crate::FigApp, message: &str) {
    if let Some(panel) = app.ui.agent.as_mut() {
        panel.status = RunStatus::Conflict;
        panel.error = Some(message.to_string());
    }
}

/// Load the open file's stored transcript into the panel, once per file switch.
///
/// The transcript is file data, so it must follow the tab: switching files shows that file's
/// conversation, not the previous one's. Comparing the uid means this costs nothing per frame.
pub fn sync_transcript_from_document(app: &mut crate::FigApp) {
    let Some(uid) = app.session.ds().ok().map(|d| d.uid) else { return };
    if app.ui.agent_transcript_uid == Some(uid) {
        return;
    }
    // Only a real switch replaces the panel's view. The first binding for a file (uid becomes known)
    // must not clear a turn the panel is already showing: the app can start a turn before any frame
    // has bound a uid, and clearing it there would make the turn uncommittable.
    let switched = app.ui.agent_transcript_uid.is_some_and(|old| old != uid);
    app.ui.agent_transcript_uid = Some(uid);
    let stored = app.session.ds().ok().map(|d| d.agent().clone()).unwrap_or_default();
    app.ui.agent_transcript = transcript_from_agent_data(&stored);
    if switched && let Some(panel) = app.ui.agent.as_mut() {
        // A turn that was running when the file was saved did not finish: it reopens as interrupted,
        // not as a turn that is still streaming.
        panel.settle_unfinished(&app.ui.agent_transcript);
    }
}

/// Write the panel's transcript back to the open file.
///
/// Called after the panel's state changes, so a save carries the conversation and a transcript change
/// marks the file dirty. A document that is gone is not an error: the tab was closed mid-turn.
pub fn store_transcript(app: &mut crate::FigApp) {
    let data = agent_data_from_transcript(&app.ui.agent_transcript);
    if let Ok(ds) = app.session.ds_mut() {
        ds.set_agent(data);
    }
}

/// The stored form of a transcript, for tests that build one directly.
pub fn agent_data_for_test(transcript: &figcraft_agent::history::Transcript) -> figcraft_format::agent::AgentData {
    agent_data_from_transcript(transcript)
}

/// The transcript a file's stored data describes, or an empty one when the data is unusable.
fn transcript_from_agent_data(data: &figcraft_format::agent::AgentData) -> figcraft_agent::history::Transcript {
    serde_json::from_value(data.json.clone()).unwrap_or_default()
}

/// The stored form of a transcript, with the current schema version.
fn agent_data_from_transcript(transcript: &figcraft_agent::history::Transcript) -> figcraft_format::agent::AgentData {
    match serde_json::to_value(transcript) {
        Ok(json) => figcraft_format::agent::AgentData::new(json),
        // Serialization cannot fail for this shape; an empty transcript is the safe answer if it did.
        Err(_) => figcraft_format::agent::AgentData::default(),
    }
}

/// Read the panel's inputs: whether a provider is configured, the requirements list and the chat.
pub(crate) fn panel_input(app: &mut crate::FigApp) -> PanelInput {
    // Whether the provider is usable, read from preferences every frame so a change in Preferences
    // takes effect at once. The stored flag is kept in step for the rest of the app to read.
    let configured = figcraft_agent::AgentConfig::from_prefs(&app.session.prefs.agent).is_ok();
    app.ui.agent_configured = configured;
    // The list comes from the command that owns requirements, so the panel and MCP agree.
    let mut requirements = match app.session.execute("requirements.list", &serde_json::json!({})) {
        Ok(v) => RequirementsList::from_result(&v),
        Err(_) => RequirementsList::default(),
    };
    // The reading form needs the open document in full: the list carries counts only.
    if let Some(id) = app.ui.agent.as_ref().and_then(|p| p.editing.clone())
        && let Ok(v) = app.session.execute("requirements.get", &serde_json::json!({"id": id}))
        && let Some(doc) = v.get("document")
        && let Ok(d) = serde_json::from_value::<RequirementsDocument>(doc.clone())
    {
        requirements.open_document = Some(d);
    }
    let conversation = app.session.ds().ok().map(|d| conversation_id(d.uid)).unwrap_or_else(|| "doc-0".to_string());
    let chat = conversation_view(&app.ui.agent_transcript, &conversation);
    PanelInput { configured, requirements, chat }
}

/// Perform the requests the panel made last frame.
///
/// Each is a command or a host call, never a document write: the panel cannot edit the document
/// itself, so undo, collaboration and MCP see exactly what the UI does.
pub fn perform_requests(app: &mut crate::FigApp) {
    let requests: Vec<Request> = std::mem::take(&mut app.ui.agent_requests);
    for r in requests {
        match r {
            Request::Cancel => {
                app.ui.agent_running.cancel();
            }
            Request::NewConversation => {
                app.ui.agent_transcript = figcraft_agent::history::Transcript::default();
                store_transcript(app);
                if let Some(p) = app.ui.agent.as_mut() {
                    p.current = None;
                    p.tools.clear();
                    p.error = None;
                    p.last_undo_label = None;
                    p.status = RunStatus::Idle;
                }
            }
            Request::OpenRequirements { id } => match app.session.execute("requirements.get", &serde_json::json!({"id": id})) {
                Ok(v) => {
                    let doc: Option<RequirementsDocument> = v.get("document").and_then(|d| serde_json::from_value(d.clone()).ok());
                    if let (Some(doc), Some(p)) = (doc, app.ui.agent.as_mut()) {
                        p.requirements_draft = Some(RequirementsDraft::from_document(&doc));
                        p.editing = Some(id);
                        // Opening a document shows it: the window is where it is read.
                        p.window_open = true;
                        p.expanded = None;
                    }
                }
                Err(e) => {
                    if let Some(p) = app.ui.agent.as_mut() {
                        p.error = Some(format!("{e}"));
                    }
                }
            },
            Request::SaveRequirements { params } => {
                // One command: one undo step, and the same path MCP uses.
                if let (Err(e), Some(p)) = (app.session.execute("requirements.upsert", &params), app.ui.agent.as_mut()) {
                    p.error = Some(format!("{e}"));
                }
            }
            Request::DeleteRequirements { id } => {
                if app.session.execute("requirements.delete", &serde_json::json!({"id": id})).is_ok()
                    && let Some(p) = app.ui.agent.as_mut()
                {
                    p.editing = None;
                    p.requirements_draft = None;
                }
            }
            Request::ExportRequirements { id } => export_markdown(app, &id),
            Request::Send { request } => start_turn(app, &request),
            Request::Retry => {
                // Rerun the last request: the panel kept it in the run it is showing.
                let request = app.ui.agent.as_ref().and_then(|p| p.current.as_ref()).map(|r| r.request.clone());
                if let Some(request) = request {
                    start_turn(app, &request);
                }
            }
            Request::ShowRequirements => {
                if let Some(panel) = app.ui.agent.as_mut() {
                    panel.tab = AgentTab::Requirements;
                    panel.show_reasoning = false;
                }
            }
            Request::ApplyRefused => apply_refused(app),
            Request::CloseRequirements => {}
        }
    }
}

/// The conversation id for the open file.
fn conversation_id_of(app: &mut crate::FigApp) -> String {
    app.session.ds().ok().map(|d| conversation_id(d.uid)).unwrap_or_else(|| "doc-0".to_string())
}

/// Record that a turn started, so a file saved mid-turn reopens with it marked interrupted.
pub(crate) fn begin_transcript_turn(transcript: &mut figcraft_agent::history::Transcript, conversation: &str, turn: u64, request: &str) {
    let convo = match transcript.conversations.iter_mut().find(|c| c.id == conversation) {
        Some(c) => c,
        None => {
            transcript.conversations.push(figcraft_agent::history::Conversation {
                id: conversation.to_string(),
                turns: Vec::new(),
                summary: None,
                continuation: Vec::new(),
            });
            // The stored transcript is capped, so the oldest conversations go when it grows.
            let max = figcraft_format::agent::MAX_CONVERSATIONS;
            while transcript.conversations.len() > max {
                transcript.conversations.remove(0);
            }
            // Just pushed, so this is Some; the match keeps production code free of `expect`.
            match transcript.conversations.last_mut() {
                Some(c) => c,
                None => return,
            }
        }
    };
    convo.turns.push(figcraft_agent::run::AgentRun { turn, request: request.to_string(), answer: String::new(), tools: Vec::new(), outcome: None });
    while convo.turns.len() > figcraft_format::agent::MAX_TURNS {
        convo.turns.remove(0);
    }
}

/// Record how a finished turn ended, from the panel's own state.
///
/// The outcome is the typed terminal, so a failed, cancelled or limited turn is recorded as such and
/// never as committed -- the transcript and the document agree about what happened.
fn record_turn(
    app: &mut crate::FigApp,
    turn: u64,
    request: String,
    answer: String,
    tools: Vec<figcraft_agent::run::ToolCall>,
    disposition: &ApplyDisposition,
) {
    // The outcome comes from what was applied, not from whether the model answered: a successful
    // answer whose batch was refused is a conflict, and a read-only answer is completed.
    let outcome = disposition.outcome();
    // The conversation the panel is showing, not every conversation with a matching turn number: turn
    // numbers are per conversation, so a search across all of them can write into the wrong one.
    let conversation = conversation_id_of(app);
    let Some(convo) = app.ui.agent_transcript.conversations.iter_mut().find(|c| c.id == conversation) else { return };
    if let Some(run) = convo.turns.iter_mut().find(|r| r.turn == turn) {
        run.request = request;
        run.answer = answer;
        run.tools = tools;
        run.outcome = Some(outcome);
    }
}

/// The continuation messages the open conversation should hand to the next turn.
///
/// Rig's transcript of the last finished turn, so a follow-up sees the tool calls and results rather
/// than only the visible text. Empty when the conversation has not produced a run yet.
fn continuation_for(app: &mut crate::FigApp) -> Vec<serde_json::Value> {
    let conversation = conversation_id_of(app);
    app.ui.agent_transcript.conversations.iter().find(|c| c.id == conversation).map(|c| c.continuation.clone()).unwrap_or_default()
}

/// Apply the replay a conflict refused, because a person asked for it.
///
/// The replay's commands are absolute -- `node.create` with its parent, `node.set` with its values --
/// so applying them to the document as it is now produces the layer the Agent built, whatever else
/// happened to the document meanwhile. The revision guard exists to stop that happening *silently*;
/// this is the same apply with the person's consent, and it is still one `command.batch`.
fn apply_refused(app: &mut crate::FigApp) {
    let Some(envelope) = app.ui.agent.as_ref().and_then(|p| p.recoverable.clone()) else { return };
    let batch = serde_json::json!({ "label": envelope.undo_label, "commands": envelope.batch_commands() });
    match app.session.execute("command.batch", &batch) {
        Ok(_) => {
            if let Some(panel) = app.ui.agent.as_mut() {
                panel.recoverable = None;
                panel.status = RunStatus::Done;
                panel.error = None;
                panel.last_undo_label = Some(envelope.undo_label.clone());
            }
        }
        // The batch rolled itself back, so nothing landed; say so and keep the offer.
        Err(e) => set_conflict(app, &format!("the Agent's changes could not be applied together: {e}")),
    }
}

/// Export a requirements document as a Markdown file, in full.
///
/// The panel used to show the Markdown's first line and byte count and drop the text, so there was
/// nothing to open: an "Export" that produced no file. This writes the whole document through the
/// platform's save dialog, or, where the platform has no paths, hands it over as a download. The
/// filename comes from the document's title with the characters a filesystem rejects removed.
fn export_markdown(app: &mut crate::FigApp, id: &str) {
    let markdown = match app.session.execute("requirements.exportMarkdown", &serde_json::json!({"id": id})) {
        Ok(v) => v.get("markdown").and_then(|m| m.as_str()).map(str::to_string),
        Err(e) => {
            if let Some(p) = app.ui.agent.as_mut() {
                p.error = Some(format!("{e}"));
            }
            return;
        }
    };
    let Some(markdown) = markdown else {
        if let Some(p) = app.ui.agent.as_mut() {
            p.error = Some("the requirements document could not be rendered".to_string());
        }
        return;
    };

    let name = match app.session.execute("requirements.get", &serde_json::json!({"id": id})) {
        Ok(v) => v.get("document").and_then(|d| d.get("title")).and_then(|t| t.as_str()).unwrap_or("requirements").to_string(),
        Err(_) => "requirements".to_string(),
    };
    let bytes = markdown.len();
    let filename = markdown_filename(&name);

    // A browser has no save dialog, so asking for one is a click that produces nothing. Route by what
    // the platform can actually do, the way the other exports do.
    if app.services.downloads() {
        if app.services.download(&filename, markdown.as_bytes()) {
            if let Some(p) = app.ui.agent.as_mut() {
                p.export_note = Some(format!("{filename} ({bytes} bytes)"));
                p.error = None;
            }
        } else if let Some(p) = app.ui.agent.as_mut() {
            // A refused download says so: silence looks like success and the person looks for a file.
            p.error = Some(crate::i18n::tr("the Markdown could not be downloaded").to_string());
            p.export_note = None;
        }
        return;
    }

    app.pick_save(crate::i18n::tr("Export requirements as Markdown"), &filename, &["md"], move |app, path| {
        match figcraft_engine::file_access::write(&path, markdown.as_bytes()) {
            Ok(()) => {
                if let Some(p) = app.ui.agent.as_mut() {
                    p.export_note = Some(format!("{} ({bytes} bytes)", path.display()));
                    p.error = None;
                }
            }
            Err(e) => {
                if let Some(p) = app.ui.agent.as_mut() {
                    // An export that failed says so, and leaves the document alone.
                    p.error = Some(format!("the Markdown could not be written: {e}"));
                }
            }
        }
    });
}

/// A filesystem-safe `.md` name from a title.
///
/// Characters that a path rejects (`/`, `\`, `:`, `*`, `?`, `"`, `<`, `>`, `|`) and control
/// characters become `-`, runs collapse, and the result is bounded so a long title cannot exceed a
/// filename limit. An empty result falls back to `requirements`.
pub fn markdown_filename(title: &str) -> String {
    const MAX: usize = 80;
    let mut out = String::new();
    for ch in title.chars() {
        if ch.is_control() || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            if !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.push(ch);
        }
    }
    let trimmed: String = out.trim_matches(|c: char| c == '-' || c.is_whitespace()).chars().take(MAX).collect();
    if trimmed.is_empty() { "requirements.md".to_string() } else { format!("{trimmed}.md") }
}

// ---------- the requirements window ----------

/// The width of the document list on the left of the requirements window.
const REQ_LIST_W: f32 = 220.0;

/// Show the requirements window while it is open (call once per frame).
///
/// A document is read more often than it is edited, and it is read in parts: the goals, one
/// requirement, its acceptance criteria. Fourteen requirements with their interactions and fields do
/// not fit a sidebar, and a column of text boxes gives no way to see which part is which -- so the
/// window reads first, and the form is a mode.
pub fn requirements_window(app: &mut crate::FigApp, ctx: &egui::Context) {
    let Some(st) = app.ui.agent.take() else { return };
    if !st.window_open {
        app.ui.agent = Some(st);
        return;
    }
    // Put the panel back before reading the session: `panel_input` looks up the open document through
    // `app.ui.agent`, and while the panel is taken it finds nothing -- which left the reading form with
    // no document and a blank window.
    app.ui.agent = Some(st);
    let input = panel_input(app);
    let Some(mut st) = app.ui.agent.take() else { return };
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let size = vec2(1040.0_f32.min(screen.width() - 80.0).max(560.0), 720.0_f32.min(screen.height() - 80.0).max(400.0));
    let mut open = true;
    let mut out = PanelOutput::default();
    egui::Window::new(crate::i18n::tr("Requirements"))
        .id(Id::unique("requirements_window"))
        .title_bar(false)
        .collapsible(false)
        .resizable(true)
        .default_size(size)
        .min_size(vec2(560.0, 400.0))
        .pivot(Align2::CENTER_CENTER)
        .default_pos(screen.center())
        .frame(egui::Frame::NONE.fill(t.panel).stroke(Stroke::new(1.0, t.border_strong)).corner_radius(CornerRadius::same(8)))
        .show(ctx, |ui| {
            ui.set_min_size(vec2(size.x, ui.available_height()));
            if requirements_header(ui, &mut st, &t, &input, &mut out) {
                open = false;
            }
            let body = Rect::from_min_max(pos2(ui.max_rect().min.x, ui.max_rect().min.y + 52.0), ui.max_rect().max);
            if st.window_editing {
                requirements_edit(ui, &mut st, &t, &input, body, &mut out);
            } else {
                requirements_read(ui, &mut st, &t, &input, body, &mut out);
            }
        });
    app.ui.agent_requests.append(&mut out.requests);
    if !open {
        st.window_open = false;
    }
    app.ui.agent = Some(st);
}

/// The window's title bar: what it is, a mode switch, and a close button. True when closing.
fn requirements_header(ui: &mut Ui, st: &mut AgentPanel, t: &Tokens, input: &PanelInput, out: &mut PanelOutput) -> bool {
    let head = Rect::from_min_size(ui.max_rect().min, vec2(ui.max_rect().width(), 52.0));
    ui.painter().rect_filled(head, CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 }, t.tab_bar);
    ui.painter().text(
        pos2(head.min.x + 18.0, head.center().y),
        Align2::LEFT_CENTER,
        crate::i18n::tr("Requirements"),
        crate::theme::semibold(13.0),
        t.text,
    );
    // How much is in the file, so the window says something before a document is opened.
    let summary = format!(
        "{} {} · {} {}",
        input.requirements.documents.len(),
        crate::i18n::tr("documents"),
        input.requirements.documents.iter().map(|d| d.requirements).sum::<usize>(),
        crate::i18n::tr("requirements"),
    );
    ui.painter().text(pos2(head.min.x + 106.0, head.center().y), Align2::LEFT_CENTER, summary, crate::theme::regular(9.5), t.text_tertiary);
    ui.painter().line_segment([pos2(head.min.x, head.max.y), pos2(head.max.x, head.max.y)], Stroke::new(1.0, t.border));

    let mut close = false;
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_size(pos2(head.max.x - 250.0, head.min.y + 14.0), vec2(234.0, 24.0)))
            .layout(Layout::right_to_left(Align::Center)),
    );
    if child.add(egui::Button::new("×").min_size(vec2(22.0, 22.0))).on_hover_text(crate::i18n::tr("Close")).clicked() {
        close = true;
    }
    // Reading or editing, as an explicit pair: a person should know which one they are in.
    let editing = st.window_editing;
    if child.add(egui::Button::new(crate::i18n::tr("Edit")).selected(editing).min_size(vec2(52.0, 22.0))).clicked() {
        st.window_editing = true;
    }
    if child.add(egui::Button::new(crate::i18n::tr("Read")).selected(!editing).min_size(vec2(52.0, 22.0))).clicked() {
        st.window_editing = false;
    }
    let _ = out;
    close
}

/// The left column: every document in the file, the open one marked. True when one was clicked.
fn requirements_list_column(ui: &mut Ui, st: &mut AgentPanel, t: &Tokens, input: &PanelInput, out: &mut PanelOutput) {
    ui.painter().rect_filled(ui.max_rect(), CornerRadius { nw: 0, ne: 0, sw: 8, se: 0 }, t.field);
    let mut area = ui.max_rect();
    area.max.x -= 1.0;
    let (line, _) = ui.allocate_exact_size(vec2(1.0, area.height()), Sense::hover());
    ui.painter().rect_filled(line, CornerRadius::ZERO, t.border);
    ui.scope_builder(egui::UiBuilder::new().max_rect(ui.max_rect().shrink2(vec2(8.0, 10.0))), |ui| {
        egui::ScrollArea::vertical().max_height((ui.available_height() - 4.0).max(32.0)).auto_shrink([false, false]).id_salt("req-window-list").show(
            ui,
            |ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                for doc in &input.requirements.documents {
                    let open = st.editing.as_deref() == Some(doc.id.as_str());
                    let r = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), 40.0));
                    let resp = ui.interact(r, Id::unique(("req-win-row", &doc.id)), Sense::click());
                    if open {
                        ui.painter().rect_filled(r, CornerRadius::same(5), t.selected_row);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(r, CornerRadius::same(5), t.hover);
                    }
                    ui.painter().text(
                        pos2(r.min.x + 8.0, r.min.y + 10.0),
                        Align2::LEFT_CENTER,
                        widgets::elide_font(ui, &doc.title, r.width() - 16.0, crate::theme::semibold(10.5)),
                        crate::theme::semibold(10.5),
                        if open { t.text } else { t.text_secondary },
                    );
                    ui.painter().text(
                        pos2(r.min.x + 8.0, r.min.y + 27.0),
                        Align2::LEFT_CENTER,
                        format!("{} · {} {}", doc.id, doc.requirements, crate::i18n::tr("req")),
                        crate::theme::regular(8.5),
                        t.text_tertiary,
                    );
                    if resp.clicked() {
                        out.push(Request::OpenRequirements { id: doc.id.clone() });
                    }
                    ui.add_space(40.0);
                }
            },
        );
    });
}

/// The reading form: the document as sections, each requirement as a card that expands.
fn requirements_read(ui: &mut Ui, st: &mut AgentPanel, t: &Tokens, input: &PanelInput, body: Rect, out: &mut PanelOutput) {
    let list = Rect::from_min_max(body.min, pos2(body.min.x + REQ_LIST_W, body.max.y));
    let doc_area = Rect::from_min_max(pos2(list.max.x, body.min.y), body.max);
    let mut left = ui.new_child(egui::UiBuilder::new().max_rect(list));
    requirements_list_column(&mut left, st, t, input, out);

    let Some(doc) = input.requirements.open_document.as_ref() else {
        let mut right = ui.new_child(egui::UiBuilder::new().max_rect(doc_area));
        right.centered_and_justified(|ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("Pick a document on the left")).size(11.0).color(t.text_tertiary));
        });
        return;
    };
    ui.scope_builder(egui::UiBuilder::new().max_rect(doc_area.shrink2(vec2(0.0, 0.0))), |ui| {
        egui::ScrollArea::vertical().max_height((doc_area.height() - 4.0).max(32.0)).auto_shrink([false, false]).id_salt("req-window-read").show(
            ui,
            |ui| {
                egui::Frame::NONE.inner_margin(egui::Margin { left: 26, right: 26, top: 18, bottom: 24 }).show(ui, |ui| {
                    ui.set_max_width((doc_area.width() - 60.0).max(200.0));
                    // The title is the one thing on screen at full strength; everything else steps down.
                    ui.label(egui::RichText::new(doc.title.as_str()).size(19.0).strong().color(t.text));
                    ui.add_space(2.0);
                    let meta = format!("{} · {} {}", doc.id, doc.requirements.len(), crate::i18n::tr("requirements"));
                    ui.label(egui::RichText::new(meta).size(9.5).color(t.text_tertiary));
                    if !doc.summary.trim().is_empty() {
                        ui.add_space(10.0);
                        ui.label(egui::RichText::new(doc.summary.as_str()).size(11.0).color(t.text_secondary));
                    }

                    read_list(ui, t, crate::i18n::tr("Goals"), &doc.goals);
                    read_list(ui, t, crate::i18n::tr("Non-goals"), &doc.non_goals);
                    read_list(ui, t, crate::i18n::tr("Audiences"), &doc.audiences);

                    if !doc.user_flows.is_empty() {
                        read_section(ui, t, crate::i18n::tr("User flows"));
                        for (i, step) in doc.user_flows.iter().enumerate() {
                            ui.horizontal_top(|ui| {
                                ui.label(egui::RichText::new(format!("{}.", i + 1)).size(10.5).color(t.text_tertiary));
                                ui.vertical(|ui| {
                                    ui.label(egui::RichText::new(step.action.as_str()).size(10.5).color(t.text));
                                    if !step.response.trim().is_empty() {
                                        ui.label(egui::RichText::new(format!("→ {}", step.response)).size(10.0).color(t.text_secondary));
                                    }
                                    if !step.timing.trim().is_empty() {
                                        ui.label(egui::RichText::new(format!("⏱ {}", step.timing)).size(9.5).color(t.text_tertiary));
                                    }
                                });
                            });
                            ui.add_space(4.0);
                        }
                        ui.add_space(6.0);
                    }

                    if !doc.requirements.is_empty() {
                        read_section(ui, t, crate::i18n::tr("Requirements"));
                        for req in &doc.requirements {
                            read_requirement(ui, st, t, req);
                        }
                    }

                    read_list(ui, t, crate::i18n::tr("Glossary"), &doc.glossary);
                    read_list(ui, t, crate::i18n::tr("Acceptance criteria"), &doc.acceptance_criteria);
                    read_list(ui, t, crate::i18n::tr("Open questions"), &doc.open_questions);
                    ui.add_space(10.0);
                });
            },
        );
    });
}

/// A section heading with the rule that starts it.
fn read_section(ui: &mut Ui, t: &Tokens, title: &str) {
    ui.add_space(16.0);
    ui.label(egui::RichText::new(title).size(12.5).strong().color(t.text));
    ui.add_space(5.0);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().line_segment([r.left_center(), r.right_center()], Stroke::new(1.0, t.border));
    ui.add_space(8.0);
}

/// A bulleted list, or nothing at all when it is empty.
fn read_list(ui: &mut Ui, t: &Tokens, title: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    read_section(ui, t, title);
    for item in items {
        ui.horizontal_top(|ui| {
            ui.label(egui::RichText::new("·").size(10.5).color(t.text_tertiary));
            ui.label(egui::RichText::new(item.as_str()).size(10.5).color(t.text_secondary));
        });
    }
}

/// One requirement as a card: a title line with its priority, opening to show what it holds.
///
/// Collapsed, fourteen requirements read as fourteen lines; open, one of them shows its detail,
/// interactions, fields and acceptance criteria. Showing all of it at once is what made the sidebar
/// unreadable: every line had the same weight and none of it stood out.
fn read_requirement(ui: &mut Ui, st: &mut AgentPanel, t: &Tokens, req: &figcraft_doc::Requirement) {
    let open = st.expanded.as_deref() == Some(req.id.as_str());
    let fill = if open { t.field } else { t.panel };
    egui::Frame::NONE
        .fill(fill)
        .stroke(Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(11, 8))
        .show(ui, |ui| {
            let head = ui.horizontal(|ui| {
                // The priority as a colour, which is faster to scan than the word.
                let colour = match req.priority {
                    figcraft_doc::Priority::Must => t.danger,
                    figcraft_doc::Priority::Should => t.warning,
                    figcraft_doc::Priority::Could => t.text_tertiary,
                    figcraft_doc::Priority::Wont => t.border,
                };
                let (dot, _) = ui.allocate_exact_size(vec2(7.0, 7.0), Sense::hover());
                ui.painter().circle_filled(dot.center(), 3.0, colour);
                let label = ui
                    .add(egui::Label::new(egui::RichText::new(req.title.as_str()).size(11.0).strong().color(t.text)).sense(Sense::click()))
                    .on_hover_text(crate::i18n::tr("Open"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(egui::RichText::new(req.priority.as_str()).size(8.5).color(t.text_tertiary));
                });
                label
            });
            if head.inner.clicked() {
                st.expanded = if open { None } else { Some(req.id.clone()) };
            }
            if !open {
                return;
            }
            if !req.detail.trim().is_empty() {
                ui.add_space(5.0);
                ui.label(egui::RichText::new(req.detail.as_str()).size(10.0).color(t.text_secondary));
            }
            for (heading, items) in [
                (crate::i18n::tr("Interactions"), &req.interactions),
                (crate::i18n::tr("Fields"), &req.fields),
                (crate::i18n::tr("Constraints"), &req.constraints),
                (crate::i18n::tr("Acceptance criteria"), &req.acceptance_criteria),
            ] {
                if items.is_empty() {
                    continue;
                }
                ui.add_space(7.0);
                ui.label(egui::RichText::new(heading).size(9.0).color(t.text_tertiary));
                for item in items {
                    ui.horizontal_top(|ui| {
                        ui.label(egui::RichText::new("·").size(9.5).color(t.text_tertiary));
                        ui.label(egui::RichText::new(item.as_str()).size(9.5).color(t.text_secondary));
                    });
                }
            }
        });
    ui.add_space(6.0);
}

/// The editing form inside the window: the same grouped fields as the sidebar had, with the room the
/// window gives them.
fn requirements_edit(ui: &mut Ui, st: &mut AgentPanel, t: &Tokens, input: &PanelInput, body: Rect, out: &mut PanelOutput) {
    let list = Rect::from_min_max(body.min, pos2(body.min.x + REQ_LIST_W, body.max.y));
    let form_area = Rect::from_min_max(pos2(list.max.x, body.min.y), body.max);
    let mut left = ui.new_child(egui::UiBuilder::new().max_rect(list));
    requirements_list_column(&mut left, st, t, input, out);

    let Some(mut draft) = st.requirements_draft.take() else {
        let mut right = ui.new_child(egui::UiBuilder::new().max_rect(form_area));
        right.centered_and_justified(|ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("Pick a document on the left")).size(11.0).color(t.text_tertiary));
        });
        return;
    };
    const BAR: f32 = 46.0;
    let form = Rect::from_min_max(form_area.min, pos2(form_area.max.x, form_area.max.y - BAR));
    let bar = Rect::from_min_max(pos2(form_area.min.x, form.max.y), form_area.max);
    let height = (form.height() - 4.0).max(32.0);
    ui.scope_builder(egui::UiBuilder::new().max_rect(form), |ui| {
        egui::ScrollArea::vertical().max_height(height).auto_shrink([false, false]).id_salt("req-window-edit").show(ui, |ui| {
            egui::Frame::NONE.inner_margin(egui::Margin { left: 26, right: 26, top: 16, bottom: 14 }).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 14.0;
                let width = (ui.available_width() - 6.0).max(120.0);
                fn field(ui: &mut Ui, t: &Tokens, label: &str, value: &mut String, rows: usize, width: f32) {
                    ui.label(egui::RichText::new(label).size(9.5).color(t.text_tertiary));
                    ui.add_space(3.0);
                    ui.add(
                        egui::TextEdit::multiline(value)
                            .desired_width(width)
                            .desired_rows(rows)
                            .frame(egui::Frame::NONE.fill(t.field).corner_radius(CornerRadius::same(4)).inner_margin(egui::Margin::symmetric(6, 5))),
                    );
                }
                fn group(ui: &mut Ui, t: &Tokens, title: &str, note: &str, add: impl FnOnce(&mut Ui, f32), width: f32) {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(title).size(11.5).strong().color(t.text));
                    ui.label(egui::RichText::new(note).size(9.0).color(t.text_tertiary));
                    ui.add_space(4.0);
                    let (r, _) = ui.allocate_exact_size(vec2(width, 1.0), Sense::hover());
                    ui.painter().line_segment([r.left_center(), r.right_center()], Stroke::new(1.0, t.border));
                    ui.add_space(6.0);
                    add(ui, width);
                }
                group(
                    ui,
                    t,
                    crate::i18n::tr("Overview"),
                    crate::i18n::tr("What this document is about"),
                    |ui, width| {
                        field(ui, t, crate::i18n::tr("Title"), &mut draft.title, 1, width);
                        field(ui, t, crate::i18n::tr("Summary"), &mut draft.summary, 2, width);
                    },
                    width,
                );
                group(
                    ui,
                    t,
                    crate::i18n::tr("Scope"),
                    crate::i18n::tr("What this does and does not do, and for whom"),
                    |ui, width| {
                        field(ui, t, crate::i18n::tr("Goals (one per line)"), &mut draft.goals, 3, width);
                        field(ui, t, crate::i18n::tr("Non-goals"), &mut draft.non_goals, 2, width);
                        field(ui, t, crate::i18n::tr("Audiences"), &mut draft.audiences, 2, width);
                    },
                    width,
                );
                group(
                    ui,
                    t,
                    crate::i18n::tr("Requirements"),
                    crate::i18n::tr("One per line: title, priority, detail"),
                    |ui, width| {
                        field(ui, t, crate::i18n::tr("Requirements (title, priority, detail)"), &mut draft.requirements, 8, width);
                    },
                    width,
                );
                group(
                    ui,
                    t,
                    crate::i18n::tr("Behaviour"),
                    crate::i18n::tr("How it behaves: flows, states and the data it handles"),
                    |ui, width| {
                        field(ui, t, crate::i18n::tr("User flows (action -> response (timing))"), &mut draft.user_flows, 5, width);
                        field(ui, t, crate::i18n::tr("Glossary (term: meaning)"), &mut draft.glossary, 3, width);
                    },
                    width,
                );
                group(
                    ui,
                    t,
                    crate::i18n::tr("Verification"),
                    crate::i18n::tr("How it is known to be done, and what is still open"),
                    |ui, width| {
                        field(ui, t, crate::i18n::tr("Acceptance criteria"), &mut draft.acceptance_criteria, 3, width);
                        field(ui, t, crate::i18n::tr("Open questions"), &mut draft.open_questions, 2, width);
                    },
                    width,
                );
            });
        });
    });

    ui.painter().rect_filled(bar, CornerRadius::ZERO, t.panel);
    ui.painter().line_segment([bar.min, pos2(bar.max.x, bar.min.y)], Stroke::new(1.0, t.border));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(26.0, 10.0))).layout(Layout::left_to_right(Align::Center)));
    let ui = &mut child;
    let saveable = draft.is_saveable() && !st.is_running();
    if ui.add_enabled(saveable, egui::Button::new(crate::i18n::tr("Save")).min_size(vec2(64.0, 24.0))).clicked() {
        out.push(Request::SaveRequirements { params: draft.to_params() });
    }
    let can_export = draft.id.is_some();
    let export = ui.add_enabled(can_export, egui::Button::new(crate::i18n::tr("Export")).min_size(vec2(64.0, 24.0)));
    if export.on_hover_text(crate::i18n::tr("Export as Markdown")).clicked()
        && let Some(id) = draft.id.clone()
    {
        out.push(Request::ExportRequirements { id });
    }
    if ui.add(egui::Button::new(crate::i18n::tr("Delete")).min_size(vec2(60.0, 24.0))).clicked() {
        match draft.id.clone() {
            Some(id) => out.push(Request::DeleteRequirements { id }),
            None => out.push(Request::CloseRequirements),
        }
    }
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if let Some(id) = &draft.id {
            ui.label(egui::RichText::new(id.as_str()).size(9.0).color(t.text_tertiary));
        }
    });
    st.requirements_draft = Some(draft);
}
