//! Shared conversion helpers between the infinity protocol `Message` types and
//! the `OpenAI` / `Anthropic` wire formats.

use infinity_provider_protocol::{
    AssistantContent, Image, ImageMediaType, ImageSource, Message, ToolDefinition, ToolResult,
    ToolResultContent, UserContent,
};
use serde_json::{Value, json};

/// Render user/assistant history as `OpenAI` `messages`, returning
/// `(messages, system_prompt)`.
#[must_use]
pub fn to_openai_messages(
    preamble: Option<&str>,
    history: &[Message],
    supports_images: bool,
) -> (Vec<Value>, Option<String>) {
    let mut messages = Vec::new();
    let system: Option<String> = preamble.map(str::to_owned);

    for msg in history {
        match msg {
            Message::User { content } => {
                let mut text_parts = Vec::new();
                let mut image_parts = Vec::new();
                let mut tool_msgs = Vec::new();
                for c in content {
                    match c {
                        UserContent::Text(t) => text_parts.push(t.text.clone()),
                        UserContent::Image(img) => image_parts.push(image_to_data_url(img)),
                        UserContent::ToolResult(r) => {
                            tool_msgs.push(tool_result_message(r, supports_images));
                        }
                    }
                }
                if tool_msgs.is_empty() {
                    messages.push(user_message(
                        &text_parts.join("\n"),
                        &image_parts,
                        supports_images,
                    ));
                } else {
                    // Tool results go as their own `tool` messages; any
                    // co-located text becomes a sibling user message.
                    if !(text_parts.is_empty() && image_parts.is_empty()) {
                        messages.push(user_message(
                            &text_parts.join("\n"),
                            &image_parts,
                            supports_images,
                        ));
                    }
                    messages.extend(tool_msgs);
                }
            }
            Message::Assistant { content } => {
                let mut text = String::new();
                let mut reasoning_text = String::new();
                let mut tool_calls = Vec::new();
                for c in content {
                    match c {
                        AssistantContent::Text(t) => {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(&t.text);
                        }
                        AssistantContent::ToolCall(tc) => {
                            tool_calls.push(json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.function.name,
                                    "arguments": arguments_to_string(&tc.function.arguments),
                                }
                            }));
                        }
                        // Reasoning blocks round-trip so reasoning-model
                        // endpoints (DeepSeek-R1 style) see prior thinking.
                        // Models that do not understand the field ignore it;
                        // it is only ever sent when this same backend
                        // produced it.
                        AssistantContent::Reasoning(r) => {
                            let thinking = r.display_text();
                            if !thinking.is_empty() {
                                if !reasoning_text.is_empty() {
                                    reasoning_text.push('\n');
                                }
                                reasoning_text.push_str(&thinking);
                            }
                        }
                    }
                }
                messages.push(assistant_message(text, reasoning_text, tool_calls));
            }
        }
    }

    (messages, system)
}

fn assistant_message(text: String, reasoning: String, tool_calls: Vec<Value>) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("role".to_owned(), json!("assistant"));
    // An empty-string body keeps the message well-formed on endpoints
    // that reject null content.
    map.insert("content".to_owned(), Value::String(text));
    if !reasoning.is_empty() {
        map.insert("reasoning_content".to_owned(), Value::String(reasoning));
    }
    if !tool_calls.is_empty() {
        map.insert("tool_calls".to_owned(), Value::Array(tool_calls));
    }
    Value::Object(map)
}

/// A tool result as an `OpenAI` `tool` message. Images become real
/// `image_url` parts only when the model accepts them; otherwise they are
/// replaced with a placeholder (a base64 data URL inlined as text would
/// burn context without being visible to the model).
fn tool_result_message(result: &ToolResult, supports_images: bool) -> Value {
    let mut text_parts = Vec::new();
    let mut image_parts = Vec::new();
    for block in &result.content {
        match block {
            ToolResultContent::Text(t) => text_parts.push(t.text.clone()),
            ToolResultContent::Image(img) => {
                if supports_images {
                    image_parts.push(image_to_data_url(img));
                } else {
                    text_parts.push("[image omitted: model does not accept images]".to_owned());
                }
            }
        }
    }
    let text = text_parts.join("\n");
    if image_parts.is_empty() {
        return json!({
            "role": "tool",
            "tool_call_id": result.id,
            "content": text,
        });
    }
    let mut parts = Vec::new();
    if !text.is_empty() {
        parts.push(json!({"type": "text", "text": text}));
    }
    for url in image_parts {
        parts.push(json!({"type": "image_url", "image_url": {"url": url}}));
    }
    json!({
        "role": "tool",
        "tool_call_id": result.id,
        "content": parts,
    })
}

fn user_message(text: &str, images: &[String], supports_images: bool) -> Value {
    if images.is_empty() || !supports_images {
        let mut t = text.to_owned();
        if !images.is_empty() {
            if !t.is_empty() {
                t.push('\n');
            }
            t.push_str("[image omitted: backend does not accept images]");
        }
        return json!({"role": "user", "content": t});
    }
    let mut parts = Vec::new();
    if !text.is_empty() {
        parts.push(json!({"type": "text", "text": text}));
    }
    for url in images {
        parts.push(json!({"type": "image_url", "image_url": {"url": url}}));
    }
    json!({"role": "user", "content": parts})
}

fn image_to_data_url(img: &Image) -> String {
    match &img.data {
        ImageSource::Url(u) => u.clone(),
        ImageSource::Base64(b) => {
            let mime = img
                .media_type
                .as_ref()
                .map_or("image/png", ImageMediaType::to_mime_type);
            format!("data:{mime};base64,{b}")
        }
    }
}

#[must_use]
pub fn arguments_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => serde_json::to_string(v).unwrap_or_else(|_| "{}".to_owned()),
    }
}

#[must_use]
pub fn to_openai_tools(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                }
            })
        })
        .collect()
}

/// Classify an HTTP failure into the protocol's retry classes.
#[must_use]
pub fn classify_http_status(status: u16, body: &str) -> infinity_provider_protocol::ErrorClass {
    use infinity_provider_protocol::ErrorClass as C;
    if status == 429 {
        return C::Throttled;
    }
    if status == 529 || (500..600).contains(&status) {
        return C::Transient;
    }
    if status == 408 || status == 504 {
        return C::Transient;
    }
    if status == 400 || status == 413 || status == 422 {
        let msg = body.to_ascii_lowercase();
        if msg.contains("context")
            || msg.contains("too long")
            || msg.contains("too large")
            || msg.contains("token limit")
            || msg.contains("max_tokens")
            || msg.contains("input length")
        {
            return C::ContextOverflow;
        }
        // Some gateways report throttling as 400 with a rate-limit message.
        if msg.contains("rate limit")
            || msg.contains("too many requests")
            || msg.contains("try again")
        {
            return C::Throttled;
        }
        // Overloaded / out-of-capacity failures are transient: the same
        // request may succeed on retry once load sheds.
        if msg.contains("overloaded") || msg.contains("capacity") {
            return C::Transient;
        }
        return C::Fatal;
    }
    C::Fatal
}

/// Split complete SSE frames off the front of `buf`, leaving any partial
/// tail in place for the next read.
///
/// Handles both LF (`\n\n`) and CRLF (`\r\n\r\n`) framing; the earliest
/// complete delimiter wins and is consumed in full. A blank line is a
/// dispatch point in either convention, so mixed framing stays aligned.
#[must_use]
pub fn take_frames(buf: &mut String) -> Vec<String> {
    let mut frames = Vec::new();
    loop {
        let lf = buf.find("\n\n");
        let crlf = buf.find("\r\n\r\n");
        let (end, len) = match (lf, crlf) {
            (Some(a), Some(b)) if b < a => (b, 4),
            (Some(a), _) => (a, 2),
            (None, Some(b)) => (b, 4),
            (None, None) => break,
        };
        let frame: String = buf.drain(..end.saturating_add(len)).collect();
        frames.push(frame);
    }
    frames
}

/// Buffer raw response bytes into `out`, holding back split UTF-8.
///
/// TCP segments can split multi-byte chars; decoding per chunk would corrupt
/// them into replacement characters, so a trailing partial sequence waits in
/// `pending` until its remaining bytes arrive.
pub fn push_bytes(out: &mut String, pending: &mut Vec<u8>, bytes: &[u8]) {
    pending.extend_from_slice(bytes);
    let valid_up_to = match std::str::from_utf8(pending) {
        Ok(_) => pending.len(),
        Err(error) => error.valid_up_to(),
    };
    if valid_up_to > 0 {
        // Valid UTF-8 by construction, so `from_utf8_lossy` is a no-op
        // that avoids fallible conversions here.
        let valid: Vec<u8> = pending.drain(..valid_up_to).collect();
        out.push_str(&String::from_utf8_lossy(&valid));
    }
}

/// Extract a human-readable message from a JSON error body.
#[must_use]
pub fn error_message_from_body(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        if let Some(m) = v.pointer("/error/message").and_then(Value::as_str) {
            return m.to_owned();
        }
        if let Some(m) = v.pointer("/error").and_then(Value::as_str) {
            return m.to_owned();
        }
        if let Some(m) = v.pointer("/message").and_then(Value::as_str) {
            return m.to_owned();
        }
    }
    body.trim().chars().take(500).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use infinity_provider_protocol::ErrorClass;

    #[test]
    fn overloaded_and_capacity_are_transient() {
        assert_eq!(
            classify_http_status(400, "the engine is overloaded"),
            ErrorClass::Transient
        );
        assert_eq!(
            classify_http_status(400, "no capacity available right now"),
            ErrorClass::Transient
        );
        // Explicit throttle language still wins with a longer backoff.
        assert_eq!(
            classify_http_status(400, "overloaded, please try again later"),
            ErrorClass::Throttled
        );
        // Context overflow still wins over generic capacity wording.
        assert_eq!(
            classify_http_status(400, "input length exceeds context capacity"),
            ErrorClass::ContextOverflow
        );
        assert_eq!(
            classify_http_status(400, "invalid request"),
            ErrorClass::Fatal
        );
    }

    #[test]
    fn splits_lf_frames_and_keeps_tail() {
        let mut buf = "data: 1\n\ndata: 2\n\npartial".to_owned();
        assert_eq!(
            take_frames(&mut buf),
            vec!["data: 1\n\n".to_owned(), "data: 2\n\n".to_owned()]
        );
        assert_eq!(buf, "partial");
    }

    #[test]
    fn splits_crlf_frames() {
        let mut buf = "data: 1\r\n\r\ndata: 2\r\n\r\n".to_owned();
        assert_eq!(
            take_frames(&mut buf),
            vec!["data: 1\r\n\r\n".to_owned(), "data: 2\r\n\r\n".to_owned()]
        );
        assert_eq!(buf, String::new());
    }

    #[test]
    fn earliest_delimiter_wins_and_partial_tail_waits() {
        let mut buf = "data: 1\r\n\r\ndata:".to_owned();
        assert_eq!(take_frames(&mut buf), vec!["data: 1\r\n\r\n".to_owned()]);
        assert_eq!(buf, "data:");
        // Completing the tail later yields the next frame.
        buf.push_str(" 2\n\n");
        assert_eq!(take_frames(&mut buf), vec!["data: 2\n\n".to_owned()]);
        assert_eq!(buf, String::new());
    }

    #[test]
    fn no_delimiter_yields_nothing() {
        let mut buf = "data: incomplete".to_owned();
        assert_eq!(take_frames(&mut buf), Vec::<String>::new());
        assert_eq!(buf, "data: incomplete");
    }
}
