//! The model transport: reply parsing, request shaping, and the opt-in live acceptance test.

use serde_json::json;

use crate::AgentErrorKind;
use crate::config::AgentConfig;
use crate::transport::{Transport, parse_message, request_body, wire_tools};

fn config(base_url: &str, model: &str) -> AgentConfig {
    let prefs = figcraft_engine::prefs::AgentPrefs {
        enabled: true,
        base_url: base_url.into(),
        model: model.into(),
        ..figcraft_engine::prefs::AgentPrefs::default()
    };
    AgentConfig::from_prefs(&prefs).expect("valid config")
}

/// A reply as an OpenAI-compatible server writes it.
fn reply(content: &str, model: &str, tool_calls: Option<serde_json::Value>) -> String {
    let mut message = json!({"role": "assistant", "content": content});
    if let Some(calls) = tool_calls {
        message["tool_calls"] = calls;
    }
    json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "model": model,
        "choices": [{"index": 0, "message": message, "finish_reason": "stop"}]
    })
    .to_string()
}

#[test]
fn a_text_reply_is_read() {
    let m = parse_message(&reply("Here it is.", "qwen", None)).expect("parsed");
    assert_eq!(m.text, "Here it is.");
    assert!(m.tool_calls.is_empty());
    assert!(!m.wants_tools());
    assert_eq!(m.model.as_deref(), Some("qwen"), "the returned model name is kept");
}

#[test]
fn the_returned_model_alias_is_accepted_not_validated() {
    // The provider may answer under a different model than asked for (an alias, a quantised
    // variant). The plan requires accepting it: nothing here compares it to the configured name.
    let m = parse_message(&reply("ok", "Qwen3-235B-A22B-Instruct-2507", None)).expect("parsed");
    assert_eq!(m.model.as_deref(), Some("Qwen3-235B-A22B-Instruct-2507"));
    assert_eq!(m.text, "ok");
}

#[test]
fn a_tool_call_is_parsed_with_its_parsed_arguments() {
    let calls = json!([{
        "id": "call_1",
        "type": "function",
        "function": {"name": "create_node", "arguments": "{\"type\": \"rectangle\", \"width\": 100}"}
    }]);
    let m = parse_message(&reply("I'll create that.", "m", Some(calls))).expect("parsed");
    assert!(m.wants_tools());
    assert_eq!(m.tool_calls.len(), 1);
    let call = &m.tool_calls[0];
    assert_eq!(call.id, "call_1");
    assert_eq!(call.name, "create_node");
    assert_eq!(call.arguments["type"], "rectangle");
    assert_eq!(call.arguments["width"], 100);
    assert_eq!(m.text, "I'll create that.", "text alongside a tool call is kept");
}

#[test]
fn a_malformed_argument_string_keeps_the_call_and_reports_it_unusable() {
    // The turn must not die because a model wrote bad JSON: the tool call is reported with null
    // arguments, which the tool then refuses, which the model can act on.
    let calls = json!([{"id": "c", "type": "function", "function": {"name": "x", "arguments": "{not json"}}]);
    let m = parse_message(&reply("", "m", Some(calls))).expect("parsed");
    assert_eq!(m.tool_calls.len(), 1);
    assert_eq!(m.tool_calls[0].arguments, serde_json::Value::Null);
    assert_eq!(m.tool_calls[0].raw_arguments, "{not json");
}

#[test]
fn several_tool_calls_keep_their_order() {
    let calls = json!([
        {"id": "a", "type": "function", "function": {"name": "first", "arguments": "{}"}},
        {"id": "b", "type": "function", "function": {"name": "second", "arguments": "{}"}},
    ]);
    let m = parse_message(&reply("", "m", Some(calls))).expect("parsed");
    let names: Vec<&str> = m.tool_calls.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["first", "second"]);
}

#[test]
fn a_provider_error_object_is_a_readable_failure() {
    let body = json!({"error": {"message": "the model is not loaded", "type": "invalid_request_error"}}).to_string();
    let e = parse_message(&body).expect_err("an error object is a failure");
    assert_eq!(e.kind, AgentErrorKind::Protocol);
    assert!(e.message.contains("not loaded"), "got {}", e.message);
}

#[test]
fn a_reply_without_choices_or_message_is_a_protocol_failure() {
    let e = parse_message("{}").expect_err("no choices");
    assert_eq!(e.kind, AgentErrorKind::Protocol);
    let e = parse_message(&json!({"choices": []}).to_string()).expect_err("no choice");
    assert_eq!(e.kind, AgentErrorKind::Protocol);
    let e = parse_message(&json!({"choices": [{"index": 0}]}).to_string()).expect_err("no message");
    assert_eq!(e.kind, AgentErrorKind::Protocol);
    let e = parse_message("not json").expect_err("not json");
    assert_eq!(e.kind, AgentErrorKind::Protocol);
}

#[test]
fn a_provider_error_body_is_summarized_not_echoed() {
    let long = "x".repeat(5000);
    let body = json!({"error": {"message": long}}).to_string();
    let e = parse_message(&body).expect_err("error");
    assert!(e.message.chars().count() < 400, "the body is summarized: {} chars", e.message.chars().count());
}

#[test]
fn the_request_carries_the_model_the_messages_and_the_tools() {
    let c = config("http://127.0.0.1:8000", "DeepSeek-V4.1-Flash-1017");
    let messages = vec![json!({"role": "user", "content": "draw a card"})];
    let definitions = vec![json!({
        "name": "create_node",
        "description": "Create a layer",
        "inputSchema": {"type": "object", "properties": {"type": {"type": "string"}}, "required": ["type"]}
    })];
    let tools = wire_tools(&definitions);
    let body = request_body(&c, &messages, &tools, false);

    assert_eq!(body["model"], "DeepSeek-V4.1-Flash-1017");
    assert_eq!(body["messages"], json!(messages));
    assert_eq!(body["stream"], false);
    assert_eq!(body["tool_choice"], "auto");
    let t = &body["tools"][0];
    assert_eq!(t["type"], "function");
    assert_eq!(t["function"]["name"], "create_node");
    assert_eq!(t["function"]["parameters"]["required"], json!(["type"]), "the catalog's schema is passed through");
}

#[test]
fn a_request_with_no_tools_omits_the_tool_fields() {
    let c = config("http://127.0.0.1:8000", "m");
    let body = request_body(&c, &[json!({"role": "user", "content": "hi"})], &[], false);
    assert!(body.get("tools").is_none(), "no tools, no field: {body}");
    assert!(body.get("tool_choice").is_none());
}

#[test]
fn the_url_is_the_normalized_base_plus_the_route() {
    // The base URL normalization is what makes every shape of endpoint work; this checks the route
    // path is what a chat-completions server expects.
    let c = config("http://127.0.0.1:8000/v1/", "m");
    assert_eq!(c.base_url(), "http://127.0.0.1:8000/v1");
    assert_eq!(format!("{}/{}", c.base_url(), c.route().path()), "http://127.0.0.1:8000/v1/chat/completions");
}

#[test]
fn an_unreachable_provider_is_a_readable_failure() {
    // A port nothing listens on: the transport must classify it, not panic or hang for a minute.
    let c = config("http://127.0.0.1:1", "m");
    let transport = Transport::new(2);
    let body = request_body(&c, &[json!({"role": "user", "content": "hi"})], &[], false);
    let e = transport.complete(&c, None, &body).expect_err("nothing listens on port 1");
    assert!(matches!(e.kind, AgentErrorKind::Unreachable | AgentErrorKind::Timeout), "got {:?}", e.kind);
    // And it says what happened, in a line a person can read.
    assert!(!e.message.is_empty());
}

#[test]
fn no_credential_means_no_authorization_header() {
    // A local vLLM often runs without one; the request must not invent an empty bearer token. This is
    // stated as the contract the transport holds; the header is only added when a credential exists.
    let c = config("http://127.0.0.1:8000", "m");
    assert_eq!(c.credential(), None, "the config carries no credential");
}

// ---------- reasoning stays out, and a stopped turn keeps its work ----------

#[test]
fn a_providers_reasoning_field_never_becomes_the_answer() {
    // DeepSeek-shaped servers return the model's scratchpad in `reasoning`. It is not an answer: the
    // plan says hidden reasoning is not exposed, so `parse_message` reads `content` only.
    let body = json!({
        "model": "m",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "I built the sidebar.",
                "reasoning": "We need a sidebar. First a frame, then rows. Let me think about widths...",
                "reasoning_content": "another scratchpad shape",
                "thinking": "yet another"
            },
            "finish_reason": "stop"
        }]
    })
    .to_string();
    let m = parse_message(&body).expect("parsed");
    assert_eq!(m.text, "I built the sidebar.", "only the answer is read");
    for leaked in ["sidebar. First a frame", "another scratchpad", "yet another", "We need"] {
        assert!(!m.text.contains(leaked), "reasoning must not reach the answer: {:?}", m.text);
    }
}

#[test]
fn a_turn_that_answers_in_reasoning_only_has_no_text() {
    // A model that thinks but says nothing produces an empty answer, not its scratchpad.
    let body = json!({"choices": [{"index": 0, "message": {"role": "assistant", "content": null, "reasoning": "thinking only"}}]}).to_string();
    let m = parse_message(&body).expect("parsed");
    assert_eq!(m.text, "", "no content means no answer");
    assert!(!m.text.contains("thinking only"));
}

#[test]
fn the_system_prompt_forbids_narrating_reasoning_and_asking() {
    // The prompt is part of the behaviour: it keeps the answer in an "I built X" register and stops a
    // model from ending a one-shot turn with a question.
    use crate::transport::SYSTEM_PROMPT;
    let lower = SYSTEM_PROMPT.to_lowercase();
    assert!(lower.contains("do not describe your reasoning"), "it forbids narrating reasoning");
    assert!(lower.contains("do not ask for confirmation"), "it forbids asking first");
    assert!(lower.contains("one or two short sentences"), "it bounds the answer");
    // It must not hard-code tool names, which would drift from the catalog.
    for tool in ["create_node", "set_properties", "inspect_document", "upsert_requirements"] {
        assert!(!SYSTEM_PROMPT.contains(tool), "the prompt names no tool: {tool}");
    }
}

#[test]
fn the_turn_limits_are_generous_enough_for_a_real_screen() {
    // A dozen rounds cut a prototype off halfway: a model builds a layout a couple of tools per round.
    // These are the numbers that made a complex request fail with 14 tool calls done and nothing left.
    let l = crate::run::Limits::default();
    assert!(l.max_rounds >= 40, "rounds: {}", l.max_rounds);
    assert!(l.max_tool_calls >= 100, "tool calls: {}", l.max_tool_calls);
    // Still finite, so a model looping on one tool cannot run forever.
    assert!(l.max_rounds < 1000);
    assert!(l.max_tool_calls < 1000);
    assert!(l.max_seconds > 0, "a wall clock still bounds a turn");
}
