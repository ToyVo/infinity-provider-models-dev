//! Anthropic Messages backend with SSE streaming.
//!
//! Used for `ANTHROPIC_API_KEY` (and compatible env aliases). The rest of
//! the presets go through the OpenAI-compatible backend in `openai.rs`.

use std::future::Future;

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

/// How long to wait for the endpoint to accept a request (connect, send,
/// response headers). This deliberately covers only initiation: once a
/// stream is live it must never be cut off by a total deadline.
const REQUEST_INITIATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// How long a live stream may go without yielding a chunk before it is
/// treated as stalled.
const STREAM_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
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
    supports_images: bool,
) -> Result<ModelStream, CompletionError> {
    let body = request_body(
        upstream_model,
        default_max_tokens,
        &request,
        supports_images,
    );

    // Subscription sessions mint a bearer token (refreshing when needed);
    // plain API keys pass through untouched.
    let oauth_token: Option<String> = match backend.oauth {
        Some(handle) => Some(ensure_bearer(backend.client, handle, false).await?),
        None => None,
    };
    // Bound only initiation (connect, send, response headers). The body
    // stream below has no total deadline; stalled streams trip the idle
    // watchdog instead.
    // A `401` means the bearer died between the freshness check and the
    // request: refresh once and retry before surfacing the failure.
    let mut resp = with_initiation_timeout(
        post_messages(
            backend.client,
            backend.base_url,
            backend.api_key,
            oauth_token.as_deref(),
            backend.extra_headers,
            &body,
        ),
        backend.base_url,
    )
    .await?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        if let Some(handle) = backend.oauth {
            let token = ensure_bearer(backend.client, handle, true).await?;
            resp = with_initiation_timeout(
                post_messages(
                    backend.client,
                    backend.base_url,
                    backend.api_key,
                    Some(token.as_str()),
                    backend.extra_headers,
                    &body,
                ),
                backend.base_url,
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

    Ok(adapt_stream(resp.bytes_stream(), STREAM_IDLE_TIMEOUT))
}

/// Adapt a raw SSE byte stream into protocol chunks.
///
/// `idle_timeout` bounds silence between chunks, not the whole stream.
/// Transport failures, stalls, and terminal error frames close the stream
/// without emitting tool calls, reasoning, or a `Final` that would read
/// as a successful completion.
fn adapt_stream<S, B, E>(mut byte_stream: S, idle_timeout: std::time::Duration) -> ModelStream
where
    S: futures_util::Stream<Item = Result<B, E>> + Unpin + Send + 'static,
    B: AsRef<[u8]> + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    Box::pin(stream! {
        let mut buf = String::new();
        let mut pending: Vec<u8> = Vec::new();
        let mut state = StreamState::new();
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
            for frame in crate::convert::take_frames(&mut buf) {
                let Some((event, data)) = split_event(&frame) else {
                    continue;
                };
                if data.is_empty() || event == "ping" {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(&data) else {
                    tracing::debug!(
                        "anthropic: skipping malformed frame: {}",
                        data.chars().take(200).collect::<String>()
                    );
                    continue;
                };
                let is_terminal_error = event == "error";
                for item in state.handle_event(&event, &v) {
                    yield item;
                }
                if is_terminal_error {
                    failed = true;
                    break 'chunks;
                }
            }
        }
        if !failed {
            for item in state.finish() {
                yield item;
            }
            let usage = (state.usage != Usage::default()).then_some(state.usage);
            yield Ok(StreamChunk::Final(FinalResponse { usage }));
        }
    })
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

/// Which kind of content block is currently open, if tracked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Tool,
    Thinking,
    Other,
}

/// Mutable state for one `Anthropic` streaming response.
struct StreamState {
    usage: Usage,
    tool_index: usize,
    tool_id: String,
    tool_name: String,
    tool_args: String,
    tool_name_sent: bool,
    open_block: Option<BlockKind>,
    thinking: String,
    thinking_signature: Option<String>,
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
            open_block: None,
            thinking: String::new(),
            thinking_signature: None,
        }
    }

    /// Flush anything still open at a clean end of stream (mirrors the
    /// per-block flushes below for streams that end without them).
    fn finish(&mut self) -> Vec<Result<StreamChunk, CompletionError>> {
        let mut out = self.flush_tool();
        out.extend(self.flush_reasoning());
        out
    }

    /// Complete the accumulated thinking block, if it has text.
    ///
    /// Mirrors the reference Bedrock provider: deltas stream the text live
    /// and the complete block (text plus provider signature, when the
    /// backend supplied one) is what gets recorded in history. Without the
    /// signature, the next turn cannot resume extended thinking.
    fn flush_reasoning(&mut self) -> Vec<Result<StreamChunk, CompletionError>> {
        if self.thinking.is_empty() {
            self.thinking_signature = None;
            return Vec::new();
        }
        let reasoning = Reasoning::new_with_signature(
            std::mem::take(&mut self.thinking),
            self.thinking_signature.take(),
        );
        vec![Ok(StreamChunk::Reasoning(reasoning))]
    }

    /// Complete the accumulated tool call, if it has a name or arguments.
    fn flush_tool(&mut self) -> Vec<Result<StreamChunk, CompletionError>> {
        if self.tool_name.is_empty() && self.tool_args.is_empty() {
            self.tool_id.clear();
            self.tool_name_sent = false;
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
        match block.get("type").and_then(Value::as_str) {
            Some("tool_use") => {
                self.open_block = Some(BlockKind::Tool);
            }
            // Thinking blocks (plain or redacted) accumulate server-side;
            // only blocks with streamed text produce a Reasoning chunk.
            Some("thinking" | "redacted_thinking") => {
                self.open_block = Some(BlockKind::Thinking);
                return Vec::new();
            }
            _ => {
                self.open_block = Some(BlockKind::Other);
                return Vec::new();
            }
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
            Some("thinking_delta") => {
                if let Some(t) = delta.get("thinking").and_then(Value::as_str) {
                    if !t.is_empty() {
                        self.thinking.push_str(t);
                        return vec![Ok(StreamChunk::ReasoningDelta {
                            id: None,
                            text: t.to_owned(),
                        })];
                    }
                }
                Vec::new()
            }
            Some("signature_delta") => {
                if let Some(signature) = delta.get("signature").and_then(Value::as_str) {
                    if !signature.is_empty() {
                        self.thinking_signature = Some(signature.to_owned());
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
        let kind = self.open_block.take();
        match kind {
            Some(BlockKind::Thinking) => self.flush_reasoning(),
            // Tool blocks flush here; text blocks never accumulate tool
            // state, and an unknown block kind still flushes a tool call in
            // case its start event was missed. `flush_tool` is a no-op
            // without a name or arguments.
            _ => self.flush_tool(),
        }
    }
}

/// Bound a send to initiation (connect, send, response headers).
///
/// A timeout here means the endpoint never started responding; the same
/// request may succeed on retry.
async fn with_initiation_timeout(
    send: impl Future<Output = Result<reqwest::Response, CompletionError>> + Send,
    base_url: &str,
) -> Result<reqwest::Response, CompletionError> {
    // `post_messages` already maps transport failures; this only adds the
    // initiation deadline.
    tokio::time::timeout(REQUEST_INITIATION_TIMEOUT, send)
        .await
        .map_err(|_| {
            CompletionError::provider(
                ErrorClass::Transient,
                format!("{base_url}: request initiation timed out"),
            )
        })?
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

/// Subscription auth headers: `Bearer` (no `x-api-key`) plus the `OAuth`
/// beta and `Claude Code` identity headers, mirroring the community
/// bridges. Shared by live sessions and pasted static bearer tokens.
fn with_oauth_headers(req: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    req.bearer_auth(token)
        .header("anthropic-beta", oauth::OAUTH_BETA)
        .header("anthropic-dangerous-direct-browser-access", "true")
        .header("User-Agent", oauth::CLAUDE_CODE_USER_AGENT)
        .header("x-app", "cli")
}

/// Send one `Messages` request with API-key or subscription auth.
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
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .header("anthropic-version", ANTHROPIC_VERSION);
    if let Some(token) = oauth_token {
        req = with_oauth_headers(req, token);
    } else if is_oauth_token(api_key) {
        // Static subscription token via the `anthropic` preset.
        req = with_oauth_headers(req, api_key);
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
    supports_images: bool,
) -> Value {
    let (system, messages) = to_anthropic_messages(
        request.preamble.as_deref(),
        &request.chat_history,
        supports_images,
    );
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
    supports_images: bool,
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
                            if supports_images {
                                blocks.push(image_block(img));
                            } else {
                                blocks.push(json!({"type": "text", "text": "[image omitted]"}));
                            }
                        }
                        UserContent::ToolResult(r) => {
                            blocks.push(tool_result_block(r, supports_images));
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

/// A tool result as an Anthropic `tool_result` block. Images become real
/// `image` blocks only when the model accepts them; otherwise a text
/// placeholder keeps the result well-formed without silent data loss.
fn tool_result_block(
    result: &infinity_provider_protocol::ToolResult,
    supports_images: bool,
) -> Value {
    let mut text = String::new();
    let mut images = Vec::new();
    for block in &result.content {
        match block {
            infinity_provider_protocol::ToolResultContent::Text(t) => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&t.text);
            }
            infinity_provider_protocol::ToolResultContent::Image(img) => {
                if supports_images {
                    images.push(image_block(img));
                } else {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str("[image omitted]");
                }
            }
        }
    }
    if images.is_empty() {
        return json!({
            "type": "tool_result",
            "tool_use_id": result.id,
            "content": text,
        });
    }
    let mut blocks = Vec::new();
    if !text.is_empty() {
        blocks.push(json!({"type": "text", "text": text}));
    }
    blocks.extend(images);
    json!({
        "type": "tool_result",
        "tool_use_id": result.id,
        "content": blocks,
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;

    fn frame(event: &str, data: &str) -> Vec<u8> {
        format!("event: {event}\ndata: {data}\n\n").into_bytes()
    }

    async fn collect<S>(byte_stream: S) -> Vec<Result<StreamChunk, CompletionError>>
    where
        S: futures_util::Stream<Item = Result<Vec<u8>, std::io::Error>> + Unpin + Send + 'static,
    {
        adapt_stream(byte_stream, std::time::Duration::from_secs(60))
            .collect()
            .await
    }

    fn text_frame(text: &str) -> Vec<u8> {
        frame(
            "content_block_delta",
            &format!("{{\"delta\":{{\"type\":\"text_delta\",\"text\":{text:?}}}}}"),
        )
    }

    #[tokio::test]
    async fn thinking_block_yields_reasoning_with_signature() {
        let start: Vec<u8> = b"event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n".to_vec();
        let thinking_start: Vec<u8> =
            b"event: content_block_start\ndata: {\"content_block\":{\"type\":\"thinking\"}}\n\n"
                .to_vec();
        let thinking: Vec<u8> = b"event: content_block_delta\ndata: {\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"let me think\"}}\n\n".to_vec();
        let signature: Vec<u8> = b"event: content_block_delta\ndata: {\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig123\"}}\n\n".to_vec();
        let stop: Vec<u8> = b"event: content_block_stop\ndata: {}\n\n".to_vec();
        let delta: Vec<u8> =
            b"event: message_delta\ndata: {\"usage\":{\"output_tokens\":5}}\n\n".to_vec();
        let end: Vec<u8> = b"event: message_stop\ndata: {}\n\n".to_vec();
        let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
            vec![start, thinking_start, thinking, signature, stop, delta, end]
                .into_iter()
                .map(Ok)
                .collect();
        let out = collect(stream::iter(chunks)).await;
        let reasoning = out.iter().find_map(|item| match item {
            Ok(StreamChunk::Reasoning(r)) => Some(r),
            _ => None,
        });
        let reasoning = reasoning.expect("expected a Reasoning chunk");
        assert_eq!(reasoning.first_text(), Some("let me think"));
        assert_eq!(reasoning.first_signature(), Some("sig123"));
        assert!(
            out.iter()
                .any(|item| matches!(item, Ok(StreamChunk::ReasoningDelta { .. }))),
            "expected streamed ReasoningDelta, got: {out:?}"
        );
        assert!(
            out.iter().any(|item| matches!(
                item,
                Ok(StreamChunk::Final(FinalResponse { usage: Some(_) }))
            )),
            "expected Final with usage, got: {out:?}"
        );
    }

    #[tokio::test]
    async fn stream_error_terminates_without_final() {
        let start: Vec<u8> =
            b"event: content_block_start\ndata: {\"content_block\":{\"type\":\"text\"}}\n\n"
                .to_vec();
        let text: Vec<u8> = b"event: content_block_delta\ndata: {\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n".to_vec();
        let error: Vec<u8> =
            b"event: error\ndata: {\"message\":\"overloaded\",\"type\":\"overloaded_error\"}\n\n"
                .to_vec();
        let unreached: Vec<u8> = b"event: content_block_delta\ndata: {\"delta\":{\"type\":\"text_delta\",\"text\":\"unreached\"}}\n\n".to_vec();
        let chunks: Vec<Result<Vec<u8>, std::io::Error>> = vec![start, text, error, unreached]
            .into_iter()
            .map(Ok)
            .collect();
        let out = collect(stream::iter(chunks)).await;
        assert_eq!(out.len(), 2, "error must close the stream: {out:?}");
        assert!(matches!(&out[0], Ok(StreamChunk::Text(t)) if t == "hi"));
        match &out[1] {
            Err(CompletionError::ProviderError { class, .. }) => {
                assert_eq!(*class, ErrorClass::Transient);
            }
            other => panic!("expected transient provider error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn no_usage_means_no_usage_in_final() {
        let chunks: Vec<Result<Vec<u8>, std::io::Error>> = vec![Ok(text_frame("hi"))];
        let out = collect(stream::iter(chunks)).await;
        assert!(
            out.iter()
                .any(|item| matches!(item, Ok(StreamChunk::Final(FinalResponse { usage: None })))),
            "unreported usage must stay None, got: {out:?}"
        );
    }
}
