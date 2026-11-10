//! The live acceptance test against a reachable OpenAI-compatible endpoint.
//!
//! Opt-in, so the deterministic suite never touches the network:
//!
//! ```text
//! FIGCRAFT_AI_BASE_URL=http://127.0.0.1:8111/v1 \
//! FIGCRAFT_AI_MODEL=DeepSeek-V4.1-Flash-1017 \
//! cargo test -p figcraft-agent --test live_vllm -- --ignored --nocapture
//! ```
//!
//! A credential, when the endpoint needs one, is named by `FIGCRAFT_AI_API_KEY_NAME` and read from
//! that variable: no key is committed, and none is printed. A local vLLM usually needs none.
//!
//! Without the variables the test says it skipped and returns, so a stray `--ignored` is harmless.

use figcraft_agent::AgentConfig;
use figcraft_agent::transport::{Transport, request_body, wire_tools};
use serde_json::json;

/// The endpoint and model from the environment, or `None` when not configured.
fn live_config() -> Option<(AgentConfig, Option<String>)> {
    let base = std::env::var("FIGCRAFT_AI_BASE_URL").ok().filter(|v| !v.trim().is_empty())?;
    let model = std::env::var("FIGCRAFT_AI_MODEL").ok().filter(|v| !v.trim().is_empty())?;
    let key_name = std::env::var("FIGCRAFT_AI_API_KEY_NAME").ok().filter(|v| !v.trim().is_empty());
    let credential = key_name.as_deref().and_then(|n| std::env::var(n).ok()).filter(|v| !v.is_empty());
    let prefs = figcraft_engine::prefs::AgentPrefs {
        enabled: true,
        base_url: base,
        model,
        credential: key_name,
        ..figcraft_engine::prefs::AgentPrefs::default()
    };
    let config = AgentConfig::from_prefs(&prefs).ok()?;
    Some((config, credential))
}

#[test]
#[ignore = "needs FIGCRAFT_AI_BASE_URL and FIGCRAFT_AI_MODEL; set them to run against a real endpoint"]
fn a_text_completion_answers_and_the_model_alias_is_accepted() {
    let Some((config, credential)) = live_config() else {
        eprintln!("skipped: set FIGCRAFT_AI_BASE_URL and FIGCRAFT_AI_MODEL to run this");
        return;
    };
    let transport = Transport::new(120);
    let messages = vec![json!({"role": "user", "content": "Reply with exactly: OK"})];
    let body = request_body(&config, &messages, &[], false);
    let m = transport.complete(&config, credential.as_deref(), &body).expect("the endpoint answers");
    assert!(!m.text.is_empty(), "an answer came back");
    println!("live: {:?} (configured `{}`, answered as {:?})", m.text.chars().take(80).collect::<String>(), config.model(), m.model);
    // Whatever alias the server returned, nothing rejected it: the model name is opaque.
    if let Some(returned) = &m.model {
        assert!(!returned.is_empty(), "the returned model name is a name");
    }
}

#[test]
#[ignore = "needs FIGCRAFT_AI_BASE_URL and FIGCRAFT_AI_MODEL; set them to run against a real endpoint"]
fn a_model_calls_the_canvas_tool_it_is_given() {
    let Some((config, credential)) = live_config() else {
        eprintln!("skipped: set FIGCRAFT_AI_BASE_URL and FIGCRAFT_AI_MODEL to run this");
        return;
    };
    let transport = Transport::new(120);
    let definitions = vec![json!({
        "name": "create_node",
        "description": "Create a design layer on the canvas",
        "inputSchema": {
            "type": "object",
            "properties": {
                "type": {"type": "string", "description": "rectangle, ellipse, frame or text"},
                "x": {"type": "number", "description": "left"},
                "y": {"type": "number", "description": "top"},
                "width": {"type": "number"},
                "height": {"type": "number"}
            },
            "required": ["type"]
        }
    })];
    let tools = wire_tools(&definitions);
    let messages = vec![json!({
        "role": "user",
        "content": "Create a rectangle 100 wide and 60 tall at 10,10 in the design. Use the create_node tool."
    })];
    let body = request_body(&config, &messages, &tools, false);
    let m = transport.complete(&config, credential.as_deref(), &body).expect("the endpoint answers");
    assert!(m.wants_tools(), "the model asked for a tool (text was {:?})", m.text);
    let call = m.tool_calls.iter().find(|c| c.name == "create_node").expect("it used the tool it was given");
    println!("live: `create_node` with {:?}", call.arguments);
    assert_eq!(call.arguments["type"], "rectangle", "with usable arguments");
    assert_eq!(call.arguments["width"], 100);
    assert_eq!(call.arguments["height"], 60);
}

#[test]
#[ignore = "needs FIGCRAFT_AI_BASE_URL and FIGCRAFT_AI_MODEL; set them to run against a real endpoint"]
fn a_model_writes_a_structured_requirements_document() {
    let Some((config, credential)) = live_config() else {
        eprintln!("skipped: set FIGCRAFT_AI_BASE_URL and FIGCRAFT_AI_MODEL to run this");
        return;
    };
    let transport = Transport::new(180);
    let definitions = vec![json!({
        "name": "upsert_requirements",
        "description": "Write a structured requirements document into the design file",
        "inputSchema": {
            "type": "object",
            "properties": {
                "title": {"type": "string"},
                "goals": {"type": "array", "items": {"type": "string"}},
                "requirements": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {"title": {"type": "string"}, "priority": {"type": "string"}},
                        "required": ["title"]
                    }
                },
                "acceptanceCriteria": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["title"]
        }
    })];
    let tools = wire_tools(&definitions);
    let messages = vec![json!({
        "role": "user",
        "content": "Write a requirements document titled `Checkout` for a mobile checkout redesign, with at least 4 goals, 12 requirements and 9 acceptance criteria. Use the upsert_requirements tool."
    })];
    let body = request_body(&config, &messages, &tools, false);
    let m = transport.complete(&config, credential.as_deref(), &body).expect("the endpoint answers");
    let call = m.tool_calls.iter().find(|c| c.name == "upsert_requirements").expect("it used the requirements tool");
    let count = |key: &str| call.arguments.get(key).and_then(|v| v.as_array()).map(Vec::len).unwrap_or(0);
    let (goals, reqs, criteria) = (count("goals"), count("requirements"), count("acceptanceCriteria"));
    println!("live: PRD `{}` with {goals} goals, {reqs} requirements, {criteria} criteria", call.arguments["title"]);
    assert_eq!(call.arguments["title"], "Checkout");
    assert!(goals >= 4, "at least four goals, got {goals}");
    assert!(reqs >= 12, "at least twelve requirements, got {reqs}");
    assert!(criteria >= 9, "at least nine acceptance criteria, got {criteria}");
}
