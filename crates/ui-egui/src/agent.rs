//! The in-app Agent panel: chat with the model, and the requirements documents in this file.
//!
//! The panel is deliberately thin. It holds display state and asks a host for work: it never calls
//! a model, owns no Rig type, and never touches a document field. Edits go through commands
//! (`requirements.upsert` and friends), so undo, collaboration and MCP all see one implementation.
//!
//! The runtime handle stays out of this struct's serializable surface: [`ui::AgentPanel::summary`]
//! is what `ui.inspect` reports, so a status dump never leaks a transcript or a credential.

use figcraft_agent::history::{Conversation, Transcript};
use figcraft_agent::run::{AgentEvent, AgentRun, CancelToken, Limits, Outcome};
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

/// One line of tool activity, as the transcript shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolRow {
    pub name: String,
    pub ok: bool,
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
    pub tools: Vec<ToolRow>,
    /// The error text of the last failure, with the action to take.
    pub error: Option<String>,
    /// The undo label the last commit used (shown so a person can find the step).
    pub last_undo_label: Option<String>,
    /// The requirements document open in the Requirements tab (`None`: the list).
    pub editing: Option<String>,
    /// The Requirements tab's draft fields while a document is open.
    pub requirements_draft: Option<RequirementsDraft>,
    /// The Markdown export result, shown briefly.
    pub export_note: Option<String>,
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
            error: None,
            last_undo_label: None,
            editing: None,
            requirements_draft: None,
            export_note: None,
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
        self.tools.clear();
        self.error = None;
        self.last_undo_label = None;
        self.export_note = None;
    }

    /// Apply one runtime event.
    ///
    /// The panel decides nothing from an error string: `Failed` carries the classified kind, and a
    /// revision conflict is its own status because the offer is to rerun, not to fix a setting.
    pub fn apply(&mut self, event: &AgentEvent, limits: &Limits) {
        match event {
            AgentEvent::Started { .. } => self.status = RunStatus::Streaming,
            AgentEvent::TextDelta { .. } => {
                if let Some(run) = self.current.as_mut() {
                    run.apply(event, limits);
                }
                self.status = RunStatus::Streaming;
            }
            AgentEvent::ToolStarted { name, .. } => {
                self.status = RunStatus::Tool;
                self.tools.push(ToolRow { name: name.clone(), ok: false });
            }
            AgentEvent::ToolFinished { name, ok, .. } => {
                if let Some(last) = self.tools.iter_mut().rev().find(|r| &r.name == name) {
                    last.ok = *ok;
                } else {
                    self.tools.push(ToolRow { name: name.clone(), ok: *ok });
                }
                if let Some(run) = self.current.as_mut() {
                    run.apply(event, limits);
                }
                self.status = RunStatus::Tool;
            }
            AgentEvent::Committed { undo_label, .. } => {
                if let Some(run) = self.current.as_mut() {
                    run.apply(event, limits);
                }
                self.status = RunStatus::Done;
                self.last_undo_label = Some(undo_label.clone());
            }
            AgentEvent::Failed { kind, message, .. } => {
                if let Some(run) = self.current.as_mut() {
                    run.apply(event, limits);
                }
                self.status = if *kind == figcraft_agent::AgentErrorKind::Conflict { RunStatus::Conflict } else { RunStatus::Failed };
                self.error = Some(format!("{message} {}", kind.action()));
            }
            AgentEvent::Cancelled { .. } => {
                if let Some(run) = self.current.as_mut() {
                    run.apply(event, limits);
                }
                self.status = RunStatus::Cancelled;
            }
        }
    }

    /// A short, stable summary for `ui.inspect`: no transcript, no credential.
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
            "editingRequirements": self.editing.is_some(),
        })
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
        Self { documents, max: v.get("max").and_then(Value::as_u64).unwrap_or(32) as usize }
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
    pub fn finish(&mut self) {
        self.token = None;
    }
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

/// The starting status of a turn read back from a file: an unfinished one is interrupted, not
/// running.
pub fn status_of(run: Option<&AgentRun>) -> RunStatus {
    match run.and_then(|r| r.outcome) {
        Some(Outcome::Committed) => RunStatus::Done,
        Some(Outcome::Failed) => RunStatus::Failed,
        Some(Outcome::Cancelled) => RunStatus::Cancelled,
        Some(Outcome::Interrupted) | None => RunStatus::Idle,
    }
}

// ---------- drawing ----------

use crate::a11y::Tip as _;
use crate::theme::Tokens;
use crate::widgets;
use egui::{Align, Align2, CornerRadius, Id, Layout, Rect, Sense, Stroke, Ui, Vec2, pos2, vec2};

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
    const COMPOSER: f32 = 104.0;
    let transcript = Rect::from_min_max(body.min, pos2(body.max.x, body.max.y - COMPOSER));
    let composer = Rect::from_min_max(pos2(body.min.x, transcript.max.y), body.max);

    ui.painter().rect_filled(transcript, CornerRadius::ZERO, t.panel);
    let mut y = transcript.min.y + 12.0;

    // The summary compaction wrote, so an elided history is visible rather than surprising.
    if input.chat.summary.is_some() {
        let r = Rect::from_min_size(pos2(transcript.min.x + 12.0, y), vec2(transcript.width() - 24.0, 46.0));
        let resp = ui.interact(r, Id::unique("agent-summary"), Sense::hover()).tip(crate::i18n::tr("Earlier turns were summarized"));
        ui.painter().rect_filled(r, CornerRadius::same(6), t.field);
        ui.painter().text(
            pos2(r.min.x + 8.0, r.min.y + 10.0),
            Align2::LEFT_CENTER,
            crate::i18n::tr("Earlier turns summarized"),
            crate::theme::regular(10.0),
            t.text_tertiary,
        );
        let _ = resp;
        y += 54.0;
    }

    if input.chat.is_empty() && state.current.is_none() {
        // Empty state: what the panel is for, one line, no marketing copy.
        ui.painter().text(
            pos2(transcript.center().x, transcript.min.y + 60.0),
            Align2::CENTER_CENTER,
            if input.configured { crate::i18n::tr("Ask for a change to this file") } else { crate::i18n::tr("Set the provider in Preferences") },
            crate::theme::regular(12.0),
            t.text_tertiary,
        );
    }

    // Finished turns: the request, then the answer.
    for run in &input.chat.runs {
        y = draw_message(ui, t, transcript, y, &run.request, true);
        if !run.answer.is_empty() {
            y = draw_message(ui, t, transcript, y, &run.answer, false);
        }
        if y > transcript.max.y {
            break;
        }
    }
    // The running turn's answer, streamed.
    if let Some(run) = state.current.as_ref().filter(|r| !r.answer.is_empty()) {
        y = draw_message(ui, t, transcript, y, &run.answer, false);
    }
    // Tool activity, compact: one row per call.
    for row in &state.tools {
        if y + 18.0 > transcript.max.y {
            break;
        }
        let r = Rect::from_min_size(pos2(transcript.min.x + 12.0, y), vec2(transcript.width() - 24.0, 18.0));
        ui.painter().rect_filled(r, CornerRadius::same(4), t.field);
        let mark = if row.ok { "✓" } else { "…" };
        ui.painter().text(
            pos2(r.min.x + 6.0, r.center().y),
            Align2::LEFT_CENTER,
            mark,
            crate::theme::regular(10.0),
            if row.ok { t.accent_text } else { t.text_tertiary },
        );
        ui.painter().text(pos2(r.min.x + 20.0, r.center().y), Align2::LEFT_CENTER, &row.name, crate::theme::regular(10.0), t.text_secondary);
        y += 22.0;
    }
    // The error, with the action the runtime classified.
    if let Some(error) = &state.error {
        let h = 40.0;
        if y + h <= transcript.max.y {
            let r = Rect::from_min_size(pos2(transcript.min.x + 12.0, y), vec2(transcript.width() - 24.0, h));
            ui.painter().rect_filled(r, CornerRadius::same(6), t.field);
            ui.painter().text(
                pos2(r.min.x + 8.0, r.min.y + 10.0),
                Align2::LEFT_CENTER,
                widgets::elide_font(ui, error, r.width() - 16.0, crate::theme::regular(10.0)),
                crate::theme::regular(10.0),
                t.text,
            );
        }
    }
    // The last undo label, so the step can be found in the history.
    if let Some(label) = &state.last_undo_label {
        let ly = composer.min.y - 16.0;
        ui.painter().text(
            pos2(transcript.min.x + 12.0, ly),
            Align2::LEFT_CENTER,
            widgets::elide_font(ui, label, transcript.width() - 24.0, crate::theme::regular(9.0)),
            crate::theme::regular(9.0),
            t.text_tertiary,
        );
    }

    // Composer: a multiline field, then the send/stop/retry row.
    ui.painter().rect_filled(composer, CornerRadius::ZERO, t.panel);
    ui.painter().line_segment([composer.min, pos2(composer.max.x, composer.min.y)], Stroke::new(1.0, t.border));
    let field = Rect::from_min_size(pos2(composer.min.x + 12.0, composer.min.y + 10.0), vec2(composer.width() - 24.0, 48.0));
    let mut text = state.draft.clone();
    let edit = egui::TextEdit::multiline(&mut text).hint_text(crate::i18n::tr("Describe a change…")).desired_width(field.width()).desired_rows(2);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(field).layout(Layout::top_down(Align::LEFT)));
    let resp = child.add(edit);
    if resp.changed() {
        state.draft = text;
    }
    crate::a11y::text(&resp, crate::i18n::tr("Message to the Agent"), &state.draft);

    let row = Rect::from_min_size(pos2(composer.min.x + 12.0, field.max.y + 6.0), vec2(composer.width() - 24.0, 24.0));
    let send_w = 64.0;
    let send = Rect::from_min_size(row.min, vec2(send_w, 22.0));
    let can_send = state.can_send() && input.configured;
    let send_resp = ui.interact(send, Id::unique("agent-send"), Sense::click()).tip(if input.configured {
        crate::i18n::tr("Send")
    } else {
        crate::i18n::tr("Set the provider in Preferences")
    });
    ui.painter().rect_filled(send, CornerRadius::same(5), if can_send { t.accent } else { t.field });
    ui.painter().text(
        send.center(),
        Align2::CENTER_CENTER,
        crate::i18n::tr("Send"),
        crate::theme::semibold(11.0),
        if can_send { t.accent_on } else { t.text_tertiary },
    );
    if send_resp.clicked() && can_send {
        out.push(Request::Send { request: state.draft.clone() });
    }

    // Stop while running; Retry after a failure, conflict or cancellation.
    let second = Rect::from_min_size(pos2(send.max.x + 6.0, row.min.y), vec2(send_w, 22.0));
    if state.is_running() {
        let resp = ui.interact(second, Id::unique("agent-stop"), Sense::click()).tip(crate::i18n::tr("Stop"));
        ui.painter().rect_filled(second, CornerRadius::same(5), t.field);
        ui.painter().text(second.center(), Align2::CENTER_CENTER, crate::i18n::tr("Stop"), crate::theme::semibold(11.0), t.text);
        if resp.clicked() {
            out.push(Request::Cancel);
        }
    } else if input.chat.can_retry(state.status) {
        let resp = ui.interact(second, Id::unique("agent-retry"), Sense::click()).tip(crate::i18n::tr("Run it again"));
        ui.painter().rect_filled(second, CornerRadius::same(5), t.field);
        ui.painter().text(second.center(), Align2::CENTER_CENTER, crate::i18n::tr("Retry"), crate::theme::semibold(11.0), t.text);
        if resp.clicked() {
            out.push(Request::Retry);
        }
    }
    // New conversation, small and to the right.
    let new_r = Rect::from_min_size(pos2(second.max.x + 6.0, row.min.y), vec2(52.0, 22.0));
    let resp = ui.interact(new_r, Id::unique("agent-new"), Sense::click()).tip(crate::i18n::tr("New conversation"));
    ui.painter().text(new_r.center(), Align2::CENTER_CENTER, crate::i18n::tr("New"), crate::theme::regular(10.0), t.text_secondary);
    if resp.clicked() {
        out.push(Request::NewConversation);
    }
}

/// One message bubble: `mine` is the user's request, otherwise the model's answer.
fn draw_message(ui: &mut Ui, t: &Tokens, area: Rect, y: f32, text: &str, mine: bool) -> f32 {
    let pad = 8.0;
    let w = (area.width() - 24.0).min(if mine { area.width() * 0.82 } else { area.width() - 24.0 });
    // Rough height from the wrapped line count; the panel only draws, it does not measure precisely.
    let font = crate::theme::regular(11.0);
    let line_h = 15.0;
    let shown = widgets::elide_font(ui, text, w - pad * 2.0, font.clone());
    let lines = ((shown.chars().count() as f32 * 6.0) / (w - pad * 2.0)).ceil().max(1.0);
    let h = lines * line_h + pad * 2.0;
    if y + h > area.max.y {
        return area.max.y + 1.0;
    }
    let x = area.min.x + 12.0;
    let r = Rect::from_min_size(pos2(x, y), vec2(w, h));
    ui.painter().rect_filled(r, CornerRadius::same(6), if mine { t.field } else { t.panel });
    if !mine {
        ui.painter().rect_stroke(r, CornerRadius::same(6), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    }
    ui.painter().text(pos2(r.min.x + pad, r.min.y + pad), Align2::LEFT_TOP, &shown, font, if mine { t.text_secondary } else { t.text });
    y + h + 8.0
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

    // The list.
    if input.requirements.documents.is_empty() {
        ui.painter().text(
            pos2(body.center().x, body.min.y + 48.0),
            Align2::CENTER_CENTER,
            crate::i18n::tr("No requirements yet"),
            crate::theme::regular(12.0),
            t.text_tertiary,
        );
    }
    for doc in &input.requirements.documents {
        if y + 40.0 > body.max.y - 40.0 {
            break;
        }
        let r = Rect::from_min_size(pos2(body.min.x + 12.0, y), vec2(body.width() - 24.0, 36.0));
        let resp = ui.interact(r, Id::unique(("req-row", &doc.id)), Sense::click()).tip(crate::i18n::tr("Open"));
        if resp.hovered() {
            ui.painter().rect_filled(r, CornerRadius::same(5), t.hover);
        }
        ui.painter().text(
            pos2(r.min.x + 8.0, r.min.y + 10.0),
            Align2::LEFT_CENTER,
            widgets::elide_font(ui, &doc.title, r.width() - 16.0, crate::theme::semibold(11.0)),
            crate::theme::semibold(11.0),
            t.text,
        );
        let counts = format!("{} · {}", doc.requirements, doc.acceptance_criteria);
        ui.painter().text(pos2(r.min.x + 8.0, r.min.y + 26.0), Align2::LEFT_CENTER, counts, crate::theme::regular(9.0), t.text_tertiary);
        if resp.clicked() {
            out.push(Request::OpenRequirements { id: doc.id.clone() });
        }
        y += 40.0;
    }

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
    y: &mut f32,
    out: &mut PanelOutput,
) {
    let field_w = body.width() - 24.0;
    let draw_field = |ui: &mut Ui, y: &mut f32, label: &str, value: &mut String, height: f32| {
        if *y + height + 16.0 > body.max.y - 44.0 {
            return;
        }
        ui.painter().text(
            pos2(body.min.x + 12.0, *y + 6.0),
            Align2::LEFT_CENTER,
            crate::i18n::tr(label),
            crate::theme::regular(9.0),
            t.text_tertiary,
        );
        *y += 14.0;
        let r = Rect::from_min_size(pos2(body.min.x + 12.0, *y), vec2(field_w, height));
        ui.painter().rect_filled(r, CornerRadius::same(4), t.field);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink(4.0)).layout(Layout::top_down(Align::LEFT)));
        child.add(egui::TextEdit::multiline(value).desired_width(r.width() - 8.0).desired_rows((height / 16.0) as usize).frame(egui::Frame::NONE));
        *y += height + 6.0;
    };
    draw_field(ui, y, "Title", &mut draft.title, 24.0);
    draw_field(ui, y, "Summary", &mut draft.summary, 40.0);
    draw_field(ui, y, "Goals (one per line)", &mut draft.goals, 40.0);
    draw_field(ui, y, "Non-goals", &mut draft.non_goals, 32.0);
    draw_field(ui, y, "Requirements (title | priority | detail)", &mut draft.requirements, 56.0);
    draw_field(ui, y, "Acceptance criteria", &mut draft.acceptance_criteria, 32.0);
    draw_field(ui, y, "Open questions", &mut draft.open_questions, 32.0);

    // Actions: save is disabled without a title, because the command would refuse it.
    let row = Rect::from_min_size(pos2(body.min.x + 12.0, body.max.y - 34.0), vec2(field_w, 24.0));
    let saveable = draft.is_saveable() && !state.is_running();
    let save = Rect::from_min_size(row.min, vec2(60.0, 22.0));
    let resp = ui.interact(save, Id::unique("req-save"), Sense::click()).tip(crate::i18n::tr("Save"));
    ui.painter().rect_filled(save, CornerRadius::same(5), if saveable { t.accent } else { t.field });
    ui.painter().text(
        save.center(),
        Align2::CENTER_CENTER,
        crate::i18n::tr("Save"),
        crate::theme::semibold(11.0),
        if saveable { t.accent_on } else { t.text_tertiary },
    );
    if resp.clicked() && saveable {
        out.push(Request::SaveRequirements { params: draft.to_params() });
    }
    // Export needs a saved document (an unsaved one has no id to export).
    let export = Rect::from_min_size(pos2(save.max.x + 6.0, row.min.y), vec2(60.0, 22.0));
    let can_export = draft.id.is_some();
    let resp = ui.interact(export, Id::unique("req-export"), Sense::click()).tip(crate::i18n::tr("Export as Markdown"));
    ui.painter().rect_filled(export, CornerRadius::same(5), t.field);
    ui.painter().text(
        export.center(),
        Align2::CENTER_CENTER,
        crate::i18n::tr("Export"),
        crate::theme::semibold(11.0),
        if can_export { t.text } else { t.text_tertiary },
    );
    if let (true, Some(id)) = (resp.clicked() && can_export, draft.id.clone()) {
        out.push(Request::ExportRequirements { id });
    }
    // Delete, only for a saved document.
    let del = Rect::from_min_size(pos2(export.max.x + 6.0, row.min.y), vec2(58.0, 22.0));
    let resp = ui.interact(del, Id::unique("req-delete"), Sense::click()).tip(crate::i18n::tr("Delete"));
    ui.painter().text(del.center(), Align2::CENTER_CENTER, crate::i18n::tr("Delete"), crate::theme::regular(10.0), t.text_secondary);
    if resp.clicked() {
        if let Some(id) = draft.id.clone() {
            out.push(Request::DeleteRequirements { id });
        } else {
            out.push(Request::CloseRequirements);
        }
    }
    // Back to the list.
    let back = Rect::from_min_size(pos2(del.max.x + 6.0, row.min.y), vec2(44.0, 22.0));
    let resp = ui.interact(back, Id::unique("req-back"), Sense::click()).tip(crate::i18n::tr("Back"));
    ui.painter().text(back.center(), Align2::CENTER_CENTER, crate::i18n::tr("Back"), crate::theme::regular(10.0), t.text_secondary);
    if resp.clicked() {
        out.push(Request::CloseRequirements);
    }
    if let Some(note) = &state.export_note {
        ui.painter().text(
            pos2(body.min.x + 12.0, body.max.y - 42.0),
            Align2::LEFT_CENTER,
            widgets::elide_font(ui, note, field_w, crate::theme::regular(9.0)),
            crate::theme::regular(9.0),
            t.text_tertiary,
        );
    }
    let _ = input;
    let _ = Vec2::ZERO;
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
    let size = vec2(AGENT_PANEL_WIDTH.max(AGENT_PANEL_MIN).min(screen.width() - 80.0), (screen.height() - 160.0).clamp(320.0, 720.0));
    let mut open = true;
    let mut output = PanelOutput::default();
    egui::Window::new(crate::i18n::tr("Agent"))
        .id(Id::unique("agent_window"))
        .title_bar(false)
        .collapsible(false)
        .resizable(true)
        .default_size(size)
        .min_size(vec2(AGENT_PANEL_MIN, 320.0))
        .anchor(Align2::RIGHT_TOP, vec2(-16.0, 64.0))
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
    app.ui.agent_turn = app.ui.agent_turn.wrapping_add(1);
    let turn = app.ui.agent_turn;
    if let Some(panel) = app.ui.agent.as_mut() {
        panel.begin_turn(turn, request);
    }
    let host = match app.ui.agent_host.as_mut() {
        Some(h) => h,
        None => return,
    };
    let token = host.start(crate::agent_host::HostRequest::Start(Box::new(crate::agent_host::TurnRequest {
        turn,
        request: request.to_string(),
        snapshot,
        limits: figcraft_agent::run::Limits::default(),
        config,
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
    let Some(host) = app.ui.agent_host.as_mut() else { return };
    let events = host.drain();
    let terminal = host.take_terminal();
    if events.is_empty() && terminal.is_none() {
        return;
    }
    let limits = figcraft_agent::run::Limits::default();
    for event in &events {
        if let Some(panel) = app.ui.agent.as_mut() {
            panel.apply(event, &limits);
        }
    }
    // The terminal value ends the turn. Only a success can carry an envelope, so a failure,
    // cancellation or limit reaches `commit_turn` as no envelope at all.
    if let Some(terminal) = terminal {
        app.ui.agent_running.finish();
        apply_terminal(app, &terminal);
    }
}

/// Report a turn's ending in the panel and, for a success, apply its envelope.
///
/// The envelope is rechecked here, at the one place that dispatches the batch: the turn id must be the
/// one the panel is showing (a late result from an older turn must not land), and the document uid and
/// revision must still be the ones the turn was built from. Checking at the snapshot instead would
/// leave a window for the document to change between the check and the write.
fn apply_terminal(app: &mut crate::FigApp, terminal: &figcraft_agent::TurnTerminal) {
    match terminal {
        figcraft_agent::TurnTerminal::Success { commit: Some(envelope), .. } => commit_turn(app, envelope),
        figcraft_agent::TurnTerminal::Success { commit: None, .. } => {
            // A read-only turn: it succeeded and changed nothing.
            if let Some(panel) = app.ui.agent.as_mut() {
                panel.status = RunStatus::Done;
            }
        }
        figcraft_agent::TurnTerminal::LimitReached { message, .. } => {
            if let Some(panel) = app.ui.agent.as_mut() {
                panel.status = RunStatus::Failed;
                panel.error = Some(message.clone());
            }
        }
        figcraft_agent::TurnTerminal::Failed { message, .. } => {
            if let Some(panel) = app.ui.agent.as_mut() {
                panel.status = RunStatus::Failed;
                panel.error = Some(message.clone());
            }
        }
        figcraft_agent::TurnTerminal::Cancelled { .. } => {
            if let Some(panel) = app.ui.agent.as_mut() {
                panel.status = RunStatus::Cancelled;
            }
        }
    }
}

/// Apply a success's envelope as exactly one undo step, after rechecking the turn and the document.
///
/// Three checks, all at the write: the turn is the one the panel shows (a late reply from a previous
/// turn would otherwise apply into a newer one), the file is the one the Agent inspected, and the
/// revision is exactly the one it was built from. Any mismatch applies nothing and says why.
fn commit_turn(app: &mut crate::FigApp, envelope: &figcraft_agent::CommitEnvelope) {
    if envelope.is_empty() {
        return;
    }
    // The panel's current turn: a stale success must not land in a newer turn.
    let shown = app.ui.agent.as_ref().and_then(|p| p.current.as_ref()).map(|r| r.turn);
    if shown != Some(envelope.turn) {
        set_conflict(app, "the Agent finished an older request, so its changes were not applied. Run the request again.");
        return;
    }
    let Ok(ds) = app.session.ds() else {
        set_conflict(app, "the file the Agent worked in is no longer open, so its changes were not applied.");
        return;
    };
    let (uid, revision) = (ds.uid, ds.revision);
    if let Some(reason) = envelope.refusal(uid, revision) {
        set_conflict(app, &format!("{reason}, so its changes were not applied. Run the request again."));
        return;
    }
    let batch = serde_json::json!({ "label": envelope.undo_label, "commands": envelope.batch_commands() });
    match app.session.execute("command.batch", &batch) {
        Ok(_) => {
            if let Some(panel) = app.ui.agent.as_mut() {
                panel.last_undo_label = Some(envelope.undo_label.clone());
                panel.status = RunStatus::Done;
            }
        }
        // The batch rolls itself back, so nothing landed.
        Err(e) => set_conflict(app, &format!("the Agent's changes could not be applied together: {e}")),
    }
}

/// A turn whose result cannot be applied: its own status, because the offer is to rerun.
fn set_conflict(app: &mut crate::FigApp, message: &str) {
    if let Some(panel) = app.ui.agent.as_mut() {
        panel.status = RunStatus::Conflict;
        panel.error = Some(message.to_string());
    }
}

/// Read the panel's inputs: whether a provider is configured, the requirements list and the chat.
pub(crate) fn panel_input(app: &mut crate::FigApp) -> PanelInput {
    // Whether the provider is usable, read from preferences every frame so a change in Preferences
    // takes effect at once. The stored flag is kept in step for the rest of the app to read.
    let configured = figcraft_agent::AgentConfig::from_prefs(&app.session.prefs.agent).is_ok();
    app.ui.agent_configured = configured;
    // The list comes from the command that owns requirements, so the panel and MCP agree.
    let requirements = match app.session.execute("requirements.list", &serde_json::json!({})) {
        Ok(v) => RequirementsList::from_result(&v),
        Err(_) => RequirementsList::default(),
    };
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
            Request::ExportRequirements { id } => match app.session.execute("requirements.exportMarkdown", &serde_json::json!({"id": id})) {
                Ok(v) => {
                    let note = v.get("markdown").and_then(|m| m.as_str()).map(|md| {
                        let first = md.lines().next().unwrap_or("").to_string();
                        format!("{} ({} bytes)", first, md.len())
                    });
                    if let Some(p) = app.ui.agent.as_mut() {
                        p.export_note = note;
                    }
                }
                Err(e) => {
                    if let Some(p) = app.ui.agent.as_mut() {
                        p.error = Some(format!("{e}"));
                    }
                }
            },
            Request::Send { request } => start_turn(app, &request),
            Request::Retry => {
                // Rerun the last request: the panel kept it in the run it is showing.
                let request = app.ui.agent.as_ref().and_then(|p| p.current.as_ref()).map(|r| r.request.clone());
                if let Some(request) = request {
                    start_turn(app, &request);
                }
            }
            Request::CloseRequirements => {}
        }
    }
}
