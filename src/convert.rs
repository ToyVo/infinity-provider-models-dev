//! Shared conversion helpers between the infinity protocol `Message` types and
//! the `OpenAI` / `Anthropic` wire formats.

use infinity_provider_protocol::{
    AssistantContent, Image, ImageMediaType, ImageSource, Message, ToolDefinition,
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
                            let text = r
                                .content
                                .iter()
                                .map(|b| match b {
                                    ToolResultContent::Text(t) => t.text.clone(),
                                    ToolResultContent::Image(img) => image_to_data_url(img),
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            tool_msgs.push(json!({
                                "role": "tool",
                                "tool_call_id": r.id,
                                "content": text,
                            }));
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
                        AssistantContent::Reasoning(_) => {}
                    }
                }
                if tool_calls.is_empty() {
                    messages.push(json!({"role": "assistant", "content": text}));
                } else {
                    messages.push(json!({
                        "role": "assistant",
                        "content": if text.is_empty() { Value::Null } else { Value::String(text) },
                        "tool_calls": tool_calls,
                    }));
                }
            }
        }
    }

    (messages, system)
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
        return C::Fatal;
    }
    C::Fatal
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
