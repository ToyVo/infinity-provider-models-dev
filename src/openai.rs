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
    CompletionError, CompletionRequest, ErrorClass, FinalResponse, ModelStream, Reasoning,
    ReasoningContent, StreamChunk, ToolCall, ToolCallDeltaContent, ToolDefinition, Usage,
};
use reqwest::Client;
use serde_json::{Value, json};

use crate::convert::{
    classify_http_status, error_message_from_body, take_frames, to_openai_messages, to_openai_tools,
};

/// How long to wait for the endpoint to accept a request (connect, send,
/// response headers). This deliberately covers only initiation: once a
/// response stream is live it must never be cut off by a total deadline
/// (long generations stream for many minutes).
const REQUEST_INITIATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// How long a live stream may go without yielding a chunk before it is
/// treated as stalled. Chunks normally arrive every few seconds; thinking
/// gaps on buffered backends can stretch much longer.
const STREAM_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
/// Upper bound for one model-list discovery probe.
const DISCOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug)]
struct AccumulatedToolCall {
    id: String,
    name: String,
    args: String,
    name_sent: bool,
}

/// How to reach and authenticate one `OpenAI`-compatible backend.
pub struct Backend<'a> {
    /// Shared HTTP client.
    pub client: &'a Client,
    /// Base URL (`.../v1`), without the trailing path.
    pub base_url: &'a str,
    /// API key; empty for trust-local endpoints.
    pub api_key: &'a str,
    /// Extra headers (e.g. `OpenRouter` attribution).
    pub extra_headers: &'a [(String, String)],
    /// Whether the model accepts image input.
    pub supports_images: bool,
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
    backend: &Backend<'_>,
    upstream_model: &str,
    default_max_tokens: Option<u64>,
    request: CompletionRequest,
) -> Result<ModelStream, CompletionError> {
    let body = request_body(
        upstream_model,
        default_max_tokens,
        &request,
        backend.supports_images,
    );

    let url = format!(
        "{}/chat/completions",
        backend.base_url.trim_end_matches('/')
    );
    let mut req = backend
        .client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .header("User-Agent", crate::USER_AGENT);
    if !backend.api_key.is_empty() {
        req = req.bearer_auth(backend.api_key);
    }
    for (k, v) in backend.extra_headers {
        req = req.header(k.as_str(), v.as_str());
    }
    // Bound only initiation (connect, send, response headers). The body
    // stream below has no total deadline; stalled streams trip the idle
    // watchdog instead.
    let resp = tokio::time::timeout(REQUEST_INITIATION_TIMEOUT, req.json(&body).send())
        .await
        .map_err(|_| {
            CompletionError::provider(
                ErrorClass::Transient,
                format!("{}: request initiation timed out", backend.base_url),
            )
        })?
        .map_err(transport_error)?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        let class = classify_http_status(status, &text);
        return Err(CompletionError::provider(
            class,
            format!(
                "{} -> {status}: {}",
                backend.base_url,
                error_message_from_body(&text)
            ),
        ));
    }

    Ok(adapt_stream(resp.bytes_stream(), STREAM_IDLE_TIMEOUT))
}

/// Adapt a raw SSE byte stream into protocol chunks.
///
/// `idle_timeout` bounds silence between chunks (not the whole stream:
/// long generations legitimately run for minutes). Transport failures,
/// stalls, and terminal error frames close the stream *without* a `Final`
/// so a dead stream never reads as a successful completion.
fn adapt_stream<S, B, E>(mut byte_stream: S, idle_timeout: std::time::Duration) -> ModelStream
where
    S: futures_util::Stream<Item = Result<B, E>> + Unpin + Send + 'static,
    B: AsRef<[u8]> + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    Box::pin(stream! {
        let mut buf = String::new();
        let mut pending: Vec<u8> = Vec::new();
        let mut tools: Vec<AccumulatedToolCall> = Vec::new();
        let mut usage: Option<Usage> = None;
        let mut reasoning = String::new();
        // Once the stream dies, nothing more can arrive: close without
        // emitting tool calls, reasoning, or a Final that would read as
        // success.
        let mut failed = false;

        'chunks: loop {
            let chunk = tokio::time::timeout(idle_timeout, byte_stream.next()).await;
            let chunk = match chunk {
                Err(_) => {
                    yield Err(CompletionError::ResponseError(
                        "stream stalled: no chunks received".to_owned(),
                    ));
                    failed = true;
                    break;
                }
                Ok(None) => break,
                Ok(Some(chunk)) => chunk,
            };
            let bytes = match chunk {
                Ok(b) => b,
                Err(e) => {
                    yield Err(CompletionError::ResponseError(format!("stream read failed: {e}")));
                    failed = true;
                    break;
                }
            };
            crate::convert::push_bytes(&mut buf, &mut pending, bytes.as_ref());
            // Process complete `data:` frames; take_frames keeps the tail.
            for frame in take_frames(&mut buf) {
                for line in frame.lines() {
                    let line = line.trim();
                    let Some(data) = line.strip_prefix("data:") else { continue };
                    let data = data.trim();
                    if data.is_empty() || data == "[DONE]" {
                        continue;
                    }
                    let Ok(v) = serde_json::from_str::<Value>(data) else {
                        tracing::debug!(
                            "openai: skipping malformed frame: {}",
                            data.chars().take(200).collect::<String>()
                        );
                        continue;
                    };
                    // Some gateways answer 200 and then report the failure
                    // as a top-level error object mid-stream. Error frames
                    // are terminal: stop reading after reporting one.
                    if let Some(msg) = stream_error_message(&v) {
                        yield Err(CompletionError::provider(
                            classify_http_status(400, &msg),
                            msg,
                        ));
                        failed = true;
                        break 'chunks;
                    }
                    if let Some(u) = parse_usage(&v) {
                        usage = Some(u);
                    }
                    for out in parse_choices(&v, &mut tools, &mut reasoning) {
                        yield out;
                    }
                }
            }
        }
        if !failed {
            for tc in std::mem::take(&mut tools) {
                if let Some(call) = finish_tool_call(tc) {
                    yield Ok(call);
                }
            }
            // The complete reasoning block is what gets recorded in
            // history; deltas above were the UI-facing stream.
            if !reasoning.is_empty() {
                yield Ok(StreamChunk::Reasoning(Reasoning {
                    id: None,
                    content: vec![ReasoningContent::Text {
                        text: std::mem::take(&mut reasoning),
                        signature: None,
                    }],
                }));
            }
            yield Ok(StreamChunk::Final(FinalResponse { usage }));
        }
    })
}

/// A top-level `{"error": ...}` object in a streamed frame.
fn stream_error_message(v: &Value) -> Option<String> {
    match v.get("error") {
        Some(Value::String(message)) if !message.is_empty() => Some(message.clone()),
        Some(Value::Object(_)) => v
            .pointer("/error/message")
            .and_then(Value::as_str)
            .filter(|message| !message.is_empty())
            .map(str::to_owned),
        _ => None,
    }
}

fn request_body(
    upstream_model: &str,
    default_max_tokens: Option<u64>,
    request: &CompletionRequest,
    supports_images: bool,
) -> Value {
    let (messages, system) = to_openai_messages(
        request.preamble.as_deref(),
        &request.chat_history,
        supports_images,
    );
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
    reasoning: &mut String,
) -> Vec<Result<StreamChunk, CompletionError>> {
    let choices = v
        .get("choices")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for choice in &choices {
        out.extend(parse_choice(choice, tools, reasoning));
    }
    out
}

fn parse_choice(
    choice: &Value,
    tools: &mut Vec<AccumulatedToolCall>,
    reasoning: &mut String,
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
    if let Some(fragment) = delta.get("reasoning_content").and_then(Value::as_str) {
        if !fragment.is_empty() {
            reasoning.push_str(fragment);
            out.push(Ok(StreamChunk::ReasoningDelta {
                id: None,
                text: fragment.to_owned(),
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

/// Complete an accumulated tool call, or `None` when the slot never
/// received a name or arguments (some servers emit empty placeholder
/// entries that would otherwise surface as noise).
fn finish_tool_call(tc: AccumulatedToolCall) -> Option<StreamChunk> {
    if tc.name.is_empty() && tc.args.is_empty() {
        return None;
    }
    let args: Value = if tc.args.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&tc.args).unwrap_or_else(|_| json!({"_raw": tc.args}))
    };
    Some(StreamChunk::ToolCall(ToolCall::new(tc.id, tc.name, args)))
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
        .header("Accept", "application/json")
        .header("User-Agent", crate::USER_AGENT);
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
        assert_eq!(discovered_ids(&json!({"models": []})), Vec::<String>::new());
        assert_eq!(discovered_ids(&Value::Null), Vec::<String>::new());
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    use futures_util::stream;

    fn frame(json: &str) -> Vec<u8> {
        format!("data: {json}\n\n").into_bytes()
    }

    async fn collect<S, B, E>(
        byte_stream: S,
        idle_ms: u64,
    ) -> Vec<Result<StreamChunk, CompletionError>>
    where
        S: futures_util::Stream<Item = Result<B, E>> + Unpin + Send + 'static,
        B: AsRef<[u8]> + Send + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        adapt_stream(byte_stream, std::time::Duration::from_millis(idle_ms))
            .collect()
            .await
    }

    #[tokio::test]
    async fn error_frame_terminates_without_final() {
        let chunks: Vec<Result<Vec<u8>, std::io::Error>> = vec![
            Ok(frame(r#"{"choices":[{"delta":{"content":"hi"}}]}"#)),
            Ok(frame(
                r#"{"error":{"message":"server overloaded","code":"overloaded"}}"#,
            )),
            Ok(frame(r#"{"choices":[{"delta":{"content":"unreached"}}]}"#)),
        ];
        let out = collect(stream::iter(chunks), 60_000).await;
        assert!(matches!(out.first(), Some(Ok(StreamChunk::Text(t))) if t == "hi"));
        assert_eq!(out.len(), 2, "error must close the stream: {out:?}");
        match &out[1] {
            Err(CompletionError::ProviderError { class, .. }) => {
                assert_eq!(*class, ErrorClass::Transient);
            }
            other => panic!("expected classified provider error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stalled_stream_errors_without_final() {
        let pending = stream::pending::<Result<Vec<u8>, std::io::Error>>();
        let out = collect(pending, 50).await;
        assert_eq!(out.len(), 1, "stall must close the stream: {out:?}");
        assert!(
            matches!(&out[0], Err(CompletionError::ResponseError(_))),
            "unexpected: {out:?}"
        );
    }

    #[tokio::test]
    async fn slow_drip_stream_completes() {
        let chunks = stream::unfold(0_u32, |n| async move {
            if n >= 3 {
                return None;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let frame = frame(&format!(
                r#"{{"choices":[{{"delta":{{"content":"{n}"}}}}]}}"#
            ));
            Some((Ok::<Vec<u8>, std::io::Error>(frame), n + 1))
        });
        let out = collect(Box::pin(chunks), 5_000).await;
        assert!(
            matches!(
                &out[..],
                [
                    Ok(StreamChunk::Text(_)),
                    Ok(StreamChunk::Text(_)),
                    Ok(StreamChunk::Text(_)),
                    Ok(StreamChunk::Final(_))
                ]
            ),
            "unexpected: {out:?}"
        );
    }

    #[tokio::test]
    async fn tool_call_accumulates_name_and_args() {
        let chunks: Vec<Result<Vec<u8>, std::io::Error>> = vec![
            Ok(frame(
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_weather","arguments":""}}]}}]}"#,
            )),
            Ok(frame(
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":"}}]}}]}"#,
            )),
            Ok(frame(
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"sf\"}"}}]}}]}"#,
            )),
            Ok(frame(r#"{"choices":[{"finish_reason":"tool_calls"}]}"#)),
            Ok(b"data: [DONE]\n\n".to_vec()),
        ];
        let out = collect(stream::iter(chunks), 60_000).await;
        let names: Vec<_> = out
            .iter()
            .filter_map(|item| match item {
                Ok(StreamChunk::ToolCallDelta {
                    content: ToolCallDeltaContent::Name(n),
                    ..
                }) => Some(n.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["get_weather".to_owned()]);
        assert!(
            out.iter().any(|item| matches!(
                item,
                Ok(StreamChunk::ToolCall(call)) if call.function.name == "get_weather"
            )),
            "expected a complete tool call, got: {out:?}"
        );
        assert!(
            out.iter()
                .any(|item| matches!(item, Ok(StreamChunk::Final(_))))
        );
    }
}
