//! The model transport: an OpenAI-compatible chat-completions call with tool calling.
//!
//! This is the seam the rest of the crate was built for. A turn asks for one model response, the
//! response either answers in text or asks for tools, and the loop repeats until the model answers
//! or a limit is reached.
//!
//! It speaks the wire format directly rather than through a framework's agent loop, for three
//! reasons that matter here: the request shape is small and stable (chat-completions is the lowest
//! common denominator every local server offers), tool calls arrive as plain JSON the catalog already
//! validates, and the whole thing is a blocking function a worker thread can call, so cancellation is
//! a flag rather than a future that has to be dropped at the right moment.
//!
//! Nothing here logs a request or a response body: both can contain the document.

use serde_json::{Value, json};

use crate::AgentErrorKind;
use crate::config::AgentConfig;

/// The HTTP client, built once per turn.
///
/// `ureq` is used because FigCraft already ships it (through the font store) and it is blocking,
/// which is what a worker thread wants: no runtime to drive, and a timeout is a socket timeout.
pub struct Transport {
    agent: ureq::Agent,
}

impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Transport")
    }
}

impl Transport {
    /// Build a transport with `timeout_seconds` per request.
    pub fn new(timeout_seconds: u32) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(u64::from(timeout_seconds))))
            .build()
            .into();
        Self { agent }
    }

    /// One chat-completions request.
    ///
    /// Returns the assistant message ([`Message`]) or a classified failure. The credential, when
    /// configured, is sent as a bearer header and never appears in an error.
    pub fn complete(&self, config: &AgentConfig, credential: Option<&str>, body: &Value) -> Result<Message, TransportError> {
        let url = format!("{}/{}", config.base_url(), config.route().path());
        let mut req = self.agent.post(&url).header("Content-Type", "application/json");
        if let Some(key) = credential.filter(|k| !k.is_empty()) {
            req = req.header("Authorization", &format!("Bearer {key}"));
        }
        let sent = req.send(serde_json::to_string(body).unwrap_or_default().as_str());
        let response = match sent {
            Ok(r) => r,
            Err(ureq::Error::StatusCode(code)) => {
                // A status error carries no body on this path; the classification still tells the
                // user what to do.
                return Err(TransportError::from(crate::gateway::classify_http(code, "")));
            }
            Err(e) => {
                let message = e.to_string();
                // A timeout and a refused connection are different actions for the user.
                let kind = if message.to_lowercase().contains("time") {
                    AgentErrorKind::Timeout
                } else {
                    AgentErrorKind::Unreachable
                };
                return Err(TransportError::new(kind, format!("the provider could not be reached: {message}")));
            }
        };
        let mut response = response;
        let text = response.body_mut().read_to_string().map_err(|e| {
            TransportError::new(AgentErrorKind::Protocol, format!("the provider's reply could not be read: {e}"))
        })?;
        parse_message(&text)
    }
}

/// A provider failure, already classified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportError {
    pub kind: AgentErrorKind,
    pub message: String,
}

impl TransportError {
    pub fn new(kind: AgentErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}

impl From<crate::gateway::ProviderError> for TransportError {
    fn from(e: crate::gateway::ProviderError) -> Self {
        Self { kind: e.kind, message: e.message }
    }
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for TransportError {}

/// One assistant message: text, tool calls, or both.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Message {
    /// The text the model wrote.
    pub text: String,
    /// The tools it asked for, in order.
    pub tool_calls: Vec<ToolCall>,
    /// The model name the server answered under, which may differ from the one asked for.
    pub model: Option<String>,
}

/// One tool call the model asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    /// The provider's id, echoed back with the result.
    pub id: String,
    /// The catalog tool name.
    pub name: String,
    /// The arguments, already parsed. A malformed argument string is kept as JSON `null` so the model
    /// is told its call was unusable instead of the turn dying.
    pub arguments: Value,
    /// The raw argument string, for the error message when it did not parse.
    pub raw_arguments: String,
}

impl Message {
    /// Whether the model asked for tools.
    pub fn wants_tools(&self) -> bool {
        !self.tool_calls.is_empty()
    }
}

/// Parse a chat-completions reply.
///
/// The model name is taken from the reply rather than assumed: a provider may answer under an alias,
/// which the plan requires accepting without validation.
pub fn parse_message(text: &str) -> Result<Message, TransportError> {
    let v: Value = serde_json::from_str(text).map_err(|e| TransportError::new(AgentErrorKind::Protocol, format!("the provider's reply was not JSON: {e}")))?;
    // An error object from a server that returned 200 with a problem.
    if let Some(err) = v.get("error") {
        let message = err.get("message").and_then(Value::as_str).unwrap_or("the provider reported an error");
        return Err(TransportError::new(AgentErrorKind::Protocol, summarize(message)));
    }
    let choice = v.get("choices").and_then(Value::as_array).and_then(|c| c.first()).ok_or_else(|| {
        TransportError::new(AgentErrorKind::Protocol, "the provider's reply had no choices")
    })?;
    let msg = choice.get("message").ok_or_else(|| TransportError::new(AgentErrorKind::Protocol, "the provider's reply had no message"))?;
    let text_out = msg.get("content").and_then(Value::as_str).unwrap_or("").to_string();
    let mut tool_calls = Vec::new();
    for call in msg.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
        let id = call.get("id").and_then(Value::as_str).unwrap_or("call").to_string();
        let function = call.get("function");
        let name = function.and_then(|f| f.get("name")).and_then(Value::as_str).unwrap_or("").to_string();
        let raw = function.and_then(|f| f.get("arguments")).and_then(Value::as_str).unwrap_or("").to_string();
        // A string that is not JSON leaves `arguments` null: the tool call reports that, which is
        // information the model can act on.
        let arguments = serde_json::from_str(&raw).unwrap_or(Value::Null);
        tool_calls.push(ToolCall { id, name, arguments, raw_arguments: raw });
    }
    Ok(Message { text: text_out, tool_calls, model: v.get("model").and_then(Value::as_str).map(str::to_string) })
}

/// The request for one round of a turn.
///
/// Built from the conversation and the tool definitions, which the catalog owns; this function only
/// shapes them into the wire format.
pub fn request_body(config: &AgentConfig, messages: &[Value], tools: &[Value], stream: bool) -> Value {
    let mut body = json!({
        "model": config.model(),
        "messages": messages,
        "stream": stream,
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.to_vec());
        body["tool_choice"] = json!("auto");
    }
    body
}

/// The catalog's tool definitions as the wire format's `tools`.
pub fn wire_tools(definitions: &[Value]) -> Vec<Value> {
    definitions
        .iter()
        .filter_map(|d| {
            let name = d.get("name")?.as_str()?;
            let description = d.get("description").and_then(Value::as_str).unwrap_or("");
            let parameters = d.get("inputSchema").cloned().unwrap_or_else(|| json!({"type": "object", "properties": {}}));
            Some(json!({"type": "function", "function": {"name": name, "description": description, "parameters": parameters}}))
        })
        .collect()
}

/// A short, single-line summary of a provider message: enough to act on, never the whole body.
fn summarize(s: &str) -> String {
    const MAX: usize = 200;
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let mut out: String = line.chars().take(MAX).collect();
    if line.chars().count() > MAX {
        out.push('…');
    }
    out
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;

// ---------- the turn loop ----------

/// How a turn ended, whichever way it went.
pub enum TurnEnd {
    /// The model answered, and the shadow holds the edits (possibly none).
    Answered(String),
    /// A tool was refused or a limit was hit; the message says which.
    Stopped(AgentErrorKind, String),
    /// The turn was cancelled.
    Cancelled,
}

/// Run one turn: ask the model, run the tools it asks for, repeat until it answers.
///
/// This is the loop the plan requires not to be rewritten per surface: the same function serves the
/// desktop worker and a browser host. It speaks plain chat-completions, so a tool call arrives as
/// JSON the catalog already validates, and the one `command.batch` at commit time is still the only
/// way a document changes.
///
/// `emit` receives each event as it happens; `limits` and `cancel` stop a runaway turn. A tool error
/// is fed back to the model, not fatal, so it can try another way.
pub fn run_turn(
    config: &AgentConfig,
    credential: Option<&str>,
    transport: &Transport,
    tools: &crate::tools::ShadowTools,
    turn: u64,
    request: &str,
    limits: &crate::run::Limits,
    cancel: &crate::run::CancelToken,
    emit: &mut dyn FnMut(crate::run::AgentEvent),
) -> TurnEnd {
    use crate::run::AgentEvent;

    let definitions = tools.definitions();
    let names: Vec<String> = definitions.iter().map(|d| d.name().to_string()).collect();
    let defs_json: Vec<Value> = figcraft_agent_tools::tool_definitions(figcraft_agent_tools::ToolPolicy::EmbeddedAgent);
    let wire = wire_tools(&defs_json);

    // The conversation: the request, then each round's assistant message and tool results.
    let mut messages: Vec<Value> = vec![json!({"role": "user", "content": request})];
    let mut rounds = 0u32;
    let mut calls = 0u32;

    loop {
        if cancel.is_cancelled() {
            return TurnEnd::Cancelled;
        }
        if limits.rounds_exhausted(rounds) {
            return TurnEnd::Stopped(AgentErrorKind::Model, format!("the turn used its {} rounds without an answer", limits.max_rounds));
        }
        rounds += 1;

        let body = request_body(config, &messages, &wire, false);
        let message = match transport.complete(config, credential, &body) {
            Ok(m) => m,
            Err(e) => return TurnEnd::Stopped(e.kind, e.message),
        };

        if cancel.is_cancelled() {
            return TurnEnd::Cancelled;
        }

        // Text arrives as one piece (the request is not streamed in this build); the panel shows it
        // the same way it shows a streamed answer.
        if !message.text.is_empty() {
            emit(AgentEvent::TextDelta { turn, text: message.text.clone() });
        }

        if !message.wants_tools() {
            return TurnEnd::Answered(message.text);
        }

        // The assistant turn, then one tool result per call: the shape the wire format expects.
        let calls_json: Vec<Value> = message
            .tool_calls
            .iter()
            .map(|c| json!({"id": c.id, "type": "function", "function": {"name": c.name, "arguments": c.raw_arguments}}))
            .collect();
        messages.push(json!({"role": "assistant", "content": message.text, "tool_calls": calls_json}));

        for call in &message.tool_calls {
            if cancel.is_cancelled() {
                return TurnEnd::Cancelled;
            }
            if limits.tool_calls_exhausted(calls) {
                return TurnEnd::Stopped(AgentErrorKind::Tool, format!("the turn used its {} tool calls", limits.max_tool_calls));
            }
            calls += 1;

            // A call the catalog does not offer is refused here, so a guessed name never runs.
            if !names.iter().any(|n| n == &call.name) {
                let text = format!("Error: no such tool `{}`", call.name);
                emit(AgentEvent::ToolFinished { turn, name: call.name.clone(), ok: false });
                messages.push(json!({"role": "tool", "tool_call_id": call.id, "content": text}));
                continue;
            }

            emit(AgentEvent::ToolStarted { turn, name: call.name.clone() });
            let args = if call.arguments.is_null() {
                // A malformed argument string: tell the model its call was unusable.
                json!({})
            } else {
                call.arguments.clone()
            };
            let result = if call.arguments.is_null() {
                figcraft_agent_tools::ToolResult::error(format!("the arguments were not valid JSON: {}", call.raw_arguments))
            } else {
                tools.call(&call.name, &args)
            };
            emit(AgentEvent::ToolFinished { turn, name: call.name.clone(), ok: !result.is_error });
            let mut text = String::new();
            if result.is_error {
                text.push_str("Error: ");
            }
            for block in &result.content {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    if !text.is_empty() && !text.ends_with(' ') {
                        text.push('\n');
                    }
                    text.push_str(t);
                }
            }
            if text.is_empty() {
                text = "(the tool returned no text)".into();
            }
            // The result is fed back clipped: a huge document dump would crowd out the answer.
            let clipped = clip_text(&text, limits.max_tool_result_bytes);
            messages.push(json!({"role": "tool", "tool_call_id": call.id, "content": clipped}));
        }
    }
}

/// Clip `s` to at most `max` bytes on a character boundary, saying it was clipped.
fn clip_text(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max.saturating_sub(40).min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… (truncated at {max} bytes)", &s[..end])
}
