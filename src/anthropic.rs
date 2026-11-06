//! Anthropic Messages backend with SSE streaming.
//!
//! Used for `ANTHROPIC_API_KEY` (and compatible env aliases). The rest of
//! the presets go through the OpenAI-compatible backend in `openai.rs`.

use async_stream::stream;
use futures_util::StreamExt;
use infinity_provider_protocol::{
    AssistantContent, CompletionError, CompletionRequest, ErrorClass, FinalResponse, Message,
    ModelStream, Reasoning, StreamChunk, ToolCall, ToolCallDeltaContent, Usage, UserContent,
};
use reqwest::Client;
use serde_json::{Value, json};

use crate::convert::{classify_http_status, error_message_from_body};
use crate::oauth::{self, OAuthSessionHandle};

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// How to reach and authenticate one `Anthropic` backend.
pub struct Backend<'a> {
    /// Shared HTTP client.
    pub client: &'a Client,
    /// `https://api.anthropic.com` (or override).
    pub base_url: &'a str,
    /// API key; ignored when `oauth` is present.
    pub api_key: &'a str,
    /// Extra headers (e.g. `OpenRouter` attribution).
    pub extra_headers: &'a [(String, String)],
    /// Subscription session; `None` for API-key auth.
    pub oauth: Option<&'a OAuthSessionHandle>,
}

/// Stream one completion from the `Anthropic` Messages endpoint.
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
    let body = request_body(upstream_model, default_max_tokens, &request);

    // Subscription sessions mint a bearer token (refreshing when needed);
    // plain API keys pass through untouched.
    let oauth_token: Option<String> = match backend.oauth {
        Some(handle) => Some(ensure_bearer(backend.client, handle, false).await?),
        None => None,
    };
    let mut resp = post_messages(
        backend.client,
        backend.base_url,
        backend.api_key,
        oauth_token.as_deref(),
        backend.extra_headers,
        &body,
    )
    .await?;
    // A `401` means the bearer died between the freshness check and the
    // request: refresh once and retry before surfacing the failure.
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        if let Some(handle) = backend.oauth {
            let token = ensure_bearer(backend.client, handle, true).await?;
            resp = post_messages(
                backend.client,
                backend.base_url,
                backend.api_key,
                Some(&token),
                backend.extra_headers,
                &body,
            )
            .await?;
        }
    }

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

    let mut byte_stream = resp.bytes_stream();

    let s = stream! {
        let mut buf = String::new();
        let mut state = StreamState::new();

        while let Some(chunk) = byte_stream.next().await {
            let bytes = match chunk {
                Ok(b) => b,
                Err(e) => {
                    yield Err(CompletionError::ResponseError(format!("stream read failed: {e}")));
                    break;
                }
            };
            buf.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(end) = buf.find("\n\n") {
                let frame_end = end.saturating_add(2);
                let frame: String = buf.drain(..frame_end).collect();
                buf = buf.trim_start_matches(['\n', '\r']).to_owned();
                let Some((event, data)) = split_event(&frame) else {
                    continue;
                };
                if data.is_empty() || event == "ping" {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(&data) else {
                    continue;
                };
                for item in state.handle_event(&event, &v) {
                    yield item;
                }
            }
        }
        yield Ok(StreamChunk::Final(FinalResponse {
            usage: Some(state.usage),
        }));
    };
    Ok(Box::pin(s))
}

/// Split one SSE frame into its `event:` name and `data:` payload.
fn split_event(frame: &str) -> Option<(String, String)> {
    let mut event = String::new();
    let mut data = String::new();
    for line in frame.lines() {
        let line = line.trim();
        if let Some(e) = line.strip_prefix("event:") {
            e.trim().clone_into(&mut event);
        } else if let Some(d) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(d.trim());
        }
    }
    if event.is_empty() && data.is_empty() {
        return None;
    }
    Some((event, data))
}

/// Mutable state for one `Anthropic` streaming response.
struct StreamState {
    usage: Usage,
    tool_index: usize,
    tool_id: String,
    tool_name: String,
    tool_args: String,
    tool_name_sent: bool,
}

impl StreamState {
    fn new() -> Self {
        Self {
            usage: Usage::default(),
            tool_index: 0,
            tool_id: String::new(),
            tool_name: String::new(),
            tool_args: String::new(),
            tool_name_sent: false,
        }
    }

    fn handle_event(
        &mut self,
        event: &str,
        v: &Value,
    ) -> Vec<Result<StreamChunk, CompletionError>> {
        match event {
            "message_start" => {
                if let Some(u) = v.pointer("/message/usage").map(parse_anthropic_usage) {
                    self.usage = u;
                }
                Vec::new()
            }
            "content_block_start" => self.on_block_start(v),
            "content_block_delta" => self.on_block_delta(v),
            "content_block_stop" => self.on_block_stop(),
            "message_delta" => {
                if let Some(u) = v.get("usage").map(parse_anthropic_usage) {
                    self.usage.output_tokens = self.usage.output_tokens.max(u.output_tokens);
                    self.usage.total_tokens = self
                        .usage
                        .input_tokens
                        .saturating_add(self.usage.output_tokens);
                    if u.cached_input_tokens > 0 {
                        self.usage.cached_input_tokens = u.cached_input_tokens;
                    }
                }
                Vec::new()
            }
            "message_stop" => {
                if let Some(u) = v.get("usage").map(parse_anthropic_usage) {
                    self.usage = u;
                }
                Vec::new()
            }
            "error" => {
                let msg = v
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .or_else(|| v.get("message").and_then(Value::as_str))
                    .unwrap_or("anthropic stream error");
                vec![Err(CompletionError::provider(
                    classify_http_status(400, msg),
                    msg.to_owned(),
                ))]
            }
            _ => Vec::new(),
        }
    }

    fn on_block_start(&mut self, v: &Value) -> Vec<Result<StreamChunk, CompletionError>> {
        let block = v.get("content_block").unwrap_or(&Value::Null);
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            return Vec::new();
        }
        block
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .clone_into(&mut self.tool_id);
        block
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .clone_into(&mut self.tool_name);
        self.tool_args.clear();
        self.tool_name_sent = false;
        if self.tool_id.is_empty() {
            self.tool_id = format!("tool-{}", self.tool_index);
        }
        if self.tool_name.is_empty() {
            return Vec::new();
        }
        self.tool_name_sent = true;
        vec![Ok(StreamChunk::ToolCallDelta {
            id: self.tool_id.clone(),
            content: ToolCallDeltaContent::Name(self.tool_name.clone()),
        })]
    }

    fn on_block_delta(&mut self, v: &Value) -> Vec<Result<StreamChunk, CompletionError>> {
        let delta = v.get("delta").unwrap_or(&Value::Null);
        match delta.get("type").and_then(Value::as_str) {
            Some("text_delta") => {
                if let Some(t) = delta.get("text").and_then(Value::as_str) {
                    if !t.is_empty() {
                        return vec![Ok(StreamChunk::Text(t.to_owned()))];
                    }
                }
                Vec::new()
            }
            Some("thinking_delta" | "signature_delta") => {
                if let Some(t) = delta.get("thinking").and_then(Value::as_str) {
                    if !t.is_empty() {
                        return vec![Ok(StreamChunk::ReasoningDelta {
                            id: None,
                            text: t.to_owned(),
                        })];
                    }
                }
                Vec::new()
            }
            Some("input_json_delta") => self.on_tool_fragment(delta),
            _ => Vec::new(),
        }
    }

    fn on_tool_fragment(&mut self, delta: &Value) -> Vec<Result<StreamChunk, CompletionError>> {
        let Some(frag) = delta.get("partial_json").and_then(Value::as_str) else {
            return Vec::new();
        };
        self.tool_args.push_str(frag);
        let mut out = Vec::new();
        if !self.tool_name_sent && !self.tool_name.is_empty() {
            self.tool_name_sent = true;
            out.push(Ok(StreamChunk::ToolCallDelta {
                id: self.tool_id.clone(),
                content: ToolCallDeltaContent::Name(self.tool_name.clone()),
            }));
        }
        if !frag.is_empty() {
            out.push(Ok(StreamChunk::ToolCallDelta {
                id: self.tool_id.clone(),
                content: ToolCallDeltaContent::Delta(frag.to_owned()),
            }));
        }
        out
    }

    fn on_block_stop(&mut self) -> Vec<Result<StreamChunk, CompletionError>> {
        if self.tool_name.is_empty() && self.tool_args.is_empty() {
            return Vec::new();
        }
        let args: Value = if self.tool_args.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&self.tool_args)
                .unwrap_or_else(|_| json!({"_raw": self.tool_args}))
        };
        let id = if self.tool_id.is_empty() {
            format!("tool-{}", self.tool_index)
        } else {
            self.tool_id.clone()
        };
        self.tool_index = self.tool_index.saturating_add(1);
        let call = StreamChunk::ToolCall(ToolCall::new(id, self.tool_name.clone(), args));
        self.tool_id.clear();
        self.tool_name.clear();
        self.tool_args.clear();
        self.tool_name_sent = false;
        vec![Ok(call)]
    }
}

/// Resolve the subscription bearer token, refreshing when stale (`force`
/// refreshes unconditionally after a `401`).
async fn ensure_bearer(
    client: &Client,
    handle: &OAuthSessionHandle,
    force: bool,
) -> Result<String, CompletionError> {
    let refreshed = if force {
        oauth::force_refresh(client, handle).await
    } else {
        oauth::ensure_fresh_token(client, handle).await
    };
    refreshed.map_err(|e| {
        let class = if e.retryable {
            ErrorClass::Transient
        } else {
            ErrorClass::Fatal
        };
        CompletionError::provider(class, format!("oauth: {}", e.message))
    })
}

/// Whether a key is a subscription bearer rather than an API key.
fn is_oauth_token(api_key: &str) -> bool {
    api_key.starts_with("sk-ant-oauth")
}

/// Send one `Messages` request with API-key or subscription auth.
///
/// Subscription (`Bearer`) requests omit `x-api-key` and carry the `OAuth`
/// beta plus `Claude Code` identity headers, mirroring the community bridges.
async fn post_messages(
    client: &Client,
    base_url: &str,
    api_key: &str,
    oauth_token: Option<&str>,
    extra_headers: &[(String, String)],
    body: &Value,
) -> Result<reqwest::Response, CompletionError> {
    let url = format!("{}/messages", base_url.trim_end_matches('/'));
    let mut req = client
        .post(&url)
        .timeout(REQUEST_TIMEOUT)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .header("anthropic-version", ANTHROPIC_VERSION);
    if let Some(token) = oauth_token {
        req = req
            .bearer_auth(token)
            .header("anthropic-beta", oauth::OAUTH_BETA)
            .header("anthropic-dangerous-direct-browser-access", "true")
            .header("User-Agent", oauth::CLAUDE_CODE_USER_AGENT)
            .header("x-app", "cli");
    } else if is_oauth_token(api_key) {
        // Static subscription token via the `anthropic` preset.
        req = req
            .bearer_auth(api_key)
            .header("anthropic-beta", oauth::OAUTH_BETA);
    } else {
        req = req
            .header("x-api-key", api_key)
            .header("User-Agent", crate::USER_AGENT);
    }
    for (k, v) in extra_headers {
        req = req.header(k.as_str(), v.as_str());
    }
    req.json(body).send().await.map_err(transport_error)
}

fn request_body(
    upstream_model: &str,
    default_max_tokens: Option<u64>,
    request: &CompletionRequest,
) -> Value {
    let (system, messages) =
        to_anthropic_messages(request.preamble.as_deref(), &request.chat_history);
    let mut map = serde_json::Map::new();
    map.insert("model".to_owned(), json!(upstream_model));
    map.insert("messages".to_owned(), Value::Array(messages));
    map.insert("stream".to_owned(), Value::Bool(true));
    if let Some(system) = system {
        map.insert("system".to_owned(), system);
    }
    let max = request.max_tokens.or(default_max_tokens).unwrap_or(16_384);
    map.insert("max_tokens".to_owned(), json!(max));
    if !request.tools.is_empty() {
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect();
        map.insert("tools".to_owned(), Value::Array(tools));
    }
    let mut body = Value::Object(map);
    if let Some(extra) = request.additional_params.as_ref() {
        merge_value(&mut body, extra);
    }
    body
}

fn parse_anthropic_usage(u: &Value) -> Usage {
    let input = u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
    let output = u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
    Usage {
        input_tokens: input,
        output_tokens: output,
        total_tokens: input.saturating_add(output),
        cached_input_tokens: u
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}

fn to_anthropic_messages(
    preamble: Option<&str>,
    history: &[Message],
) -> (Option<Value>, Vec<Value>) {
    let system = preamble.map(|s| json!([{"type": "text", "text": s}]));
    let mut messages = Vec::new();
    for msg in history {
        match msg {
            Message::User { content } => {
                let mut blocks = Vec::new();
                for c in content {
                    match c {
                        UserContent::Text(t) => {
                            blocks.push(json!({"type": "text", "text": t.text}));
                        }
                        UserContent::Image(img) => {
                            blocks.push(image_block(img));
                        }
                        UserContent::ToolResult(r) => {
                            let text = r
                                .content
                                .iter()
                                .map(|b| match b {
                                    infinity_provider_protocol::ToolResultContent::Text(t) => {
                                        t.text.clone()
                                    }
                                    infinity_provider_protocol::ToolResultContent::Image(_) => {
                                        "[image omitted]".to_owned()
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            blocks.push(json!({
                                "type": "tool_result",
                                "tool_use_id": r.id,
                                "content": text,
                            }));
                        }
                    }
                }
                if !blocks.is_empty() {
                    messages.push(json!({"role": "user", "content": blocks}));
                }
            }
            Message::Assistant { content } => {
                let mut blocks = Vec::new();
                for c in content {
                    match c {
                        AssistantContent::Text(t) => {
                            blocks.push(json!({"type": "text", "text": t.text}));
                        }
                        AssistantContent::ToolCall(tc) => blocks.push(json!({
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.function.name,
                            "input": tc.function.arguments,
                        })),
                        AssistantContent::Reasoning(r) => {
                            blocks.push(reasoning_block(r));
                        }
                    }
                }
                if !blocks.is_empty() {
                    messages.push(json!({"role": "assistant", "content": blocks}));
                }
            }
        }
    }
    (system, messages)
}

fn image_block(img: &infinity_provider_protocol::Image) -> Value {
    match &img.data {
        infinity_provider_protocol::ImageSource::Url(u) => json!({
            "type": "image",
            "source": {"type": "url", "url": u},
        }),
        infinity_provider_protocol::ImageSource::Base64(b) => {
            let mime = img.media_type.as_ref().map_or(
                "image/png",
                infinity_provider_protocol::ImageMediaType::to_mime_type,
            );
            json!({
                "type": "image",
                "source": {"type": "base64", "media_type": mime, "data": b},
            })
        }
    }
}

fn reasoning_block(r: &Reasoning) -> Value {
    let text = r.display_text();
    let signature = r.first_signature().unwrap_or("");
    json!({"type": "thinking", "thinking": text, "signature": signature})
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

fn transport_error(e: reqwest::Error) -> CompletionError {
    if e.is_timeout() || e.is_connect() || e.is_body() || e.is_decode() {
        CompletionError::provider(ErrorClass::Transient, format!("transport error: {e}"))
    } else if e.is_request() {
        CompletionError::RequestError(Box::new(e))
    } else {
        CompletionError::provider(ErrorClass::Transient, format!("transport error: {e}"))
    }
}
