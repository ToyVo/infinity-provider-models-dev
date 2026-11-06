//! `OpenAI` Chat Completions backend with SSE streaming.
//!
//! Covers every `OpenAI`-compatible endpoint: `OpenAI` itself plus `DeepSeek`,
//! `Groq`, `Cerebras`, `xAI`, `Mistral`, `Gemini` (`OpenAI`-compat path), `Together`,
//! `Fireworks`, `OpenRouter`, `NVIDIA NIM`, `Moonshot`/`Kimi`, `ZAI`, `MiniMax`, `Qwen`
//! (`DashScope` compat), `HuggingFace` router, `Vercel` AI Gateway, `OpenCode` Zen,
//! `Azure OpenAI`, `Ollama` / `LM Studio` / `vLLM`, and any custom
//! `INFINITY_OPENAI_BASE_URL` proxy.

use async_stream::stream;
use futures_util::StreamExt;
use infinity_provider_protocol::{
    CompletionError, CompletionRequest, ErrorClass, FinalResponse, ModelStream, StreamChunk,
    ToolCall, ToolCallDeltaContent, ToolDefinition, Usage,
};
use reqwest::Client;
use serde_json::{Value, json};

use crate::convert::{
    classify_http_status, error_message_from_body, to_openai_messages, to_openai_tools,
};

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// Upper bound for one model-list discovery probe.
const DISCOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug)]
struct AccumulatedToolCall {
    id: String,
    name: String,
    args: String,
    name_sent: bool,
}

/// Stream one completion from an `OpenAI`-compatible endpoint.
///
/// # Errors
///
/// Returns [`CompletionError::provider`] when the endpoint rejects the
/// request (with the provider's retry classification) and
/// [`CompletionError::ResponseError`] when a live stream breaks mid-flight.
/// Individual stream items may also yield errors.
pub async fn invoke(
    client: &Client,
    base_url: &str,
    api_key: &str,
    extra_headers: &[(String, String)],
    upstream_model: &str,
    default_max_tokens: Option<u64>,
    request: CompletionRequest,
) -> Result<ModelStream, CompletionError> {
    let body = request_body(upstream_model, default_max_tokens, &request);

    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let mut req = client
        .post(&url)
        .timeout(REQUEST_TIMEOUT)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream");
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    for (k, v) in extra_headers {
        req = req.header(k.as_str(), v.as_str());
    }
    let resp = req.json(&body).send().await.map_err(transport_error)?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        let class = classify_http_status(status, &text);
        return Err(CompletionError::provider(
            class,
            format!("{base_url} -> {status}: {}", error_message_from_body(&text)),
        ));
    }

    let mut byte_stream = resp.bytes_stream();
    let s = stream! {
        let mut buf = String::new();
        let mut tools: Vec<AccumulatedToolCall> = Vec::new();
        let mut usage: Option<Usage> = None;

        while let Some(chunk) = byte_stream.next().await {
            let bytes = match chunk {
                Ok(b) => b,
                Err(e) => {
                    yield Err(CompletionError::ResponseError(format!("stream read failed: {e}")));
                    break;
                }
            };
            buf.push_str(&String::from_utf8_lossy(&bytes));
            // Process complete `data:` frames; keep the tail.
            while let Some(end) = buf.find("\n\n").or_else(|| buf.find("\r\n\r\n")) {
                let frame_end = end.saturating_add(2);
                let frame: String = buf.drain(..frame_end).collect();
                buf = buf.trim_start_matches(['\n', '\r']).to_owned();
                for line in frame.lines() {
                    let line = line.trim();
                    let Some(data) = line.strip_prefix("data:") else { continue };
                    let data = data.trim();
                    if data.is_empty() || data == "[DONE]" {
                        continue;
                    }
                    let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
                    if let Some(u) = parse_usage(&v) {
                        usage = Some(u);
                    }
                    for out in parse_choices(&v, &mut tools) {
                        yield out;
                    }
                }
            }
        }
        for tc in std::mem::take(&mut tools) {
            yield Ok(finish_tool_call(tc));
        }
        yield Ok(StreamChunk::Final(FinalResponse { usage }));
    };
    Ok(Box::pin(s))
}

fn request_body(
    upstream_model: &str,
    default_max_tokens: Option<u64>,
    request: &CompletionRequest,
) -> Value {
    let (messages, system) =
        to_openai_messages(request.preamble.as_deref(), &request.chat_history, true);
    let mut map = serde_json::Map::new();
    map.insert("model".to_owned(), json!(upstream_model));
    map.insert(
        "messages".to_owned(),
        Value::Array(with_system(messages, system)),
    );
    map.insert("stream".to_owned(), Value::Bool(true));
    map.insert("stream_options".to_owned(), json!({"include_usage": true}));
    if let Some(max) = request.max_tokens.or(default_max_tokens) {
        map.insert("max_tokens".to_owned(), json!(max));
    }
    if !request.tools.is_empty() {
        map.insert(
            "tools".to_owned(),
            Value::Array(to_openai_tools(&request.tools)),
        );
    }
    let mut body = Value::Object(map);
    if let Some(extra) = request.additional_params.as_ref() {
        merge_value(&mut body, extra);
    }
    body
}

fn with_system(mut messages: Vec<Value>, system: Option<String>) -> Vec<Value> {
    if let Some(s) = system {
        messages.insert(0, json!({"role": "system", "content": s}));
    }
    messages
}

fn merge_value(dst: &mut Value, src: &Value) {
    match (dst, src) {
        (Value::Object(d), Value::Object(s)) => {
            for (k, v) in s {
                merge_value(d.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
        (d, s) => *d = s.clone(),
    }
}

fn parse_usage(v: &Value) -> Option<Usage> {
    let u = v.get("usage")?;
    Some(Usage {
        input_tokens: u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
        output_tokens: u
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        total_tokens: u.get("total_tokens").and_then(Value::as_u64).unwrap_or(0),
        cached_input_tokens: u
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

/// Grow `tools` to cover `index` and return the slot for it.
fn slot_for(
    tools: &mut Vec<AccumulatedToolCall>,
    index: usize,
) -> Option<&mut AccumulatedToolCall> {
    while tools.len() <= index {
        tools.push(AccumulatedToolCall {
            id: String::new(),
            name: String::new(),
            args: String::new(),
            name_sent: false,
        });
    }
    tools.get_mut(index)
}

fn parse_choices(
    v: &Value,
    tools: &mut Vec<AccumulatedToolCall>,
) -> Vec<Result<StreamChunk, CompletionError>> {
    let choices = v
        .get("choices")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for choice in &choices {
        out.extend(parse_choice(choice, tools));
    }
    out
}

fn parse_choice(
    choice: &Value,
    tools: &mut Vec<AccumulatedToolCall>,
) -> Vec<Result<StreamChunk, CompletionError>> {
    if let Some(err) = choice.get("error") {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("stream error");
        let code = err.get("code").and_then(Value::as_str).unwrap_or("");
        return vec![Err(CompletionError::provider(
            classify_http_status(400, &format!("{code} {msg}")),
            msg.to_owned(),
        ))];
    }
    let mut out = Vec::new();
    let delta = choice.pointer("/delta").unwrap_or(&Value::Null).clone();
    if let Some(text) = delta.get("content").and_then(Value::as_str) {
        if !text.is_empty() {
            out.push(Ok(StreamChunk::Text(text.to_owned())));
        }
    }
    if let Some(reasoning) = delta.get("reasoning_content").and_then(Value::as_str) {
        if !reasoning.is_empty() {
            out.push(Ok(StreamChunk::ReasoningDelta {
                id: None,
                text: reasoning.to_owned(),
            }));
        }
    }
    if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
        accumulate_delta_tool_calls(calls, tools, &mut out);
    }
    // Some servers send the full tool call only in the final message.
    if let Some(msg) = choice.get("message").and_then(Value::as_object) {
        apply_full_message(msg, tools, &mut out);
    }
    out
}

fn accumulate_delta_tool_calls(
    calls: &[Value],
    tools: &mut Vec<AccumulatedToolCall>,
    out: &mut Vec<Result<StreamChunk, CompletionError>>,
) {
    for call in calls {
        let raw_index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
        let Ok(index) = usize::try_from(raw_index) else {
            continue;
        };
        let Some(slot) = slot_for(tools, index) else {
            continue;
        };
        if let Some(id) = call.get("id").and_then(Value::as_str) {
            if !id.is_empty() {
                id.clone_into(&mut slot.id);
            }
        }
        let func = call.get("function").unwrap_or(&Value::Null);
        if let Some(name) = func.get("name").and_then(Value::as_str) {
            if !name.is_empty() {
                name.clone_into(&mut slot.name);
            }
        }
        if let Some(frag) = func.get("arguments").and_then(Value::as_str) {
            slot.args.push_str(frag);
            let id = if slot.id.is_empty() {
                format!("tool-{index}")
            } else {
                slot.id.clone()
            };
            if !slot.name_sent && !slot.name.is_empty() {
                slot.name_sent = true;
                out.push(Ok(StreamChunk::ToolCallDelta {
                    id: id.clone(),
                    content: ToolCallDeltaContent::Name(slot.name.clone()),
                }));
            }
            if !frag.is_empty() {
                out.push(Ok(StreamChunk::ToolCallDelta {
                    id,
                    content: ToolCallDeltaContent::Delta(frag.to_owned()),
                }));
            }
        }
        if slot.id.is_empty() {
            slot.id = format!("tool-{index}");
        }
    }
}

fn apply_full_message(
    msg: &serde_json::Map<String, Value>,
    tools: &mut Vec<AccumulatedToolCall>,
    out: &mut Vec<Result<StreamChunk, CompletionError>>,
) {
    let empty = Vec::new();
    let calls = msg
        .get("tool_calls")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    for (index, call) in calls.iter().enumerate() {
        let id = call
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let func = call.get("function").unwrap_or(&Value::Null);
        let name = func
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let args = func
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if name.is_empty() {
            continue;
        }
        let Some(slot) = slot_for(tools, index) else {
            continue;
        };
        slot.id = if id.is_empty() {
            format!("tool-{index}")
        } else {
            id
        };
        slot.name = name;
        slot.args = args;
        slot.name_sent = true;
    }
    if let Some(text) = msg.get("content").and_then(Value::as_str) {
        if !text.is_empty() {
            out.push(Ok(StreamChunk::Text(text.to_owned())));
        }
    }
}

fn finish_tool_call(tc: AccumulatedToolCall) -> StreamChunk {
    if tc.name.is_empty() && tc.args.is_empty() {
        return StreamChunk::Text(String::new());
    }
    let args: Value = if tc.args.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&tc.args).unwrap_or_else(|_| json!({"_raw": tc.args}))
    };
    StreamChunk::ToolCall(ToolCall::new(tc.id, tc.name, args))
}

fn transport_error(e: reqwest::Error) -> CompletionError {
    if e.is_timeout() || e.is_connect() || e.is_body() || e.is_decode() {
        CompletionError::provider(ErrorClass::Transient, format!("transport error: {e}"))
    } else if e.is_request() {
        CompletionError::RequestError(Box::new(e))
    } else {
        CompletionError::provider(ErrorClass::Transient, format!("transport error: {e}"))
    }
}

/// Extract model ids from an OpenAI-style `GET /models` body.
///
/// Accepts `{data: [{id, ...}]}` and bare `[{id, ...}]` shapes.
#[must_use]
pub fn discovered_ids(body: &Value) -> Vec<String> {
    let items = body
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| body.as_array());
    let Some(items) = items else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .map(str::to_owned)
        .filter(|id| !id.is_empty())
        .collect()
}

/// Probe an OpenAI-compatible `GET {base}/models` endpoint.
///
/// Returns the advertised ids, or an empty list when the endpoint is down,
/// refuses, or answers in an unknown shape. Never fails: discovery is a
/// best-effort enrichment of the curated catalog.
#[must_use]
pub async fn discover_models(client: &Client, base_url: &str, api_key: &str) -> Vec<String> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut req = client
        .get(&url)
        .timeout(DISCOVERY_TIMEOUT)
        .header("Accept", "application/json");
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    let resp = match req.send().await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::debug!("discovery: {url} unreachable: {e}");
            return Vec::new();
        }
    };
    if !resp.status().is_success() {
        tracing::debug!("discovery: {url} -> {}", resp.status());
        return Vec::new();
    }
    match resp.json::<Value>().await {
        Ok(body) => discovered_ids(&body),
        Err(e) => {
            tracing::debug!("discovery: {url} returned invalid JSON: {e}");
            Vec::new()
        }
    }
}

/// Build a non-streaming request body (used by tests).
#[must_use]
pub fn build_body(
    upstream_model: &str,
    max_tokens: Option<u64>,
    tools: &[ToolDefinition],
    messages: Vec<Value>,
    system: Option<String>,
) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("model".to_owned(), json!(upstream_model));
    map.insert(
        "messages".to_owned(),
        Value::Array(with_system(messages, system)),
    );
    map.insert("stream".to_owned(), Value::Bool(true));
    if let Some(max) = max_tokens {
        map.insert("max_tokens".to_owned(), json!(max));
    }
    if !tools.is_empty() {
        map.insert("tools".to_owned(), Value::Array(to_openai_tools(tools)));
    }
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_openai_list_shape() {
        let body = json!({"data": [{"id": "a"}, {"id": "b"}]});
        assert_eq!(discovered_ids(&body), vec!["a".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn parses_bare_array_and_skips_bad_entries() {
        let body = json!([{"id": "a"}, {"other": 1}, {"id": ""}]);
        assert_eq!(discovered_ids(&body), vec!["a".to_owned()]);
    }

    #[test]
    fn rejects_unknown_shapes() {
        assert!(discovered_ids(&json!({"models": []})).is_empty());
        assert!(discovered_ids(&Value::Null).is_empty());
    }
}
