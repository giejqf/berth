//! A Chat Completions answer as a Messages API message.

use serde_json::{Value, json};

use crate::request::Names;

/// The message's id: the provider's, kept recognizable.
pub(crate) fn message_id(upstream: Option<&str>) -> String {
    match upstream.filter(|id| !id.is_empty()) {
        Some(id) => format!("msg_{}", clean_id(id)),
        None => format!("msg_{}", uuid::Uuid::new_v4().simple()),
    }
}

/// A tool call's id as Claude Code takes one (`[A-Za-z0-9_-]`); a new one
/// when the provider sent none.
pub(crate) fn tool_id(upstream: Option<&str>) -> String {
    match upstream.map(clean_id).filter(|id| !id.is_empty()) {
        Some(id) => id,
        None => format!("toolu_{}", uuid::Uuid::new_v4().simple()),
    }
}

fn clean_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Why the model stopped, in Messages API terms. A provider that ends with
/// `stop` after calling tools still called them.
pub(crate) fn stop_reason(finish: Option<&str>, called_tools: bool) -> &'static str {
    match finish {
        Some("length") => "max_tokens",
        Some("content_filter") => "refusal",
        Some("tool_calls" | "function_call") => "tool_use",
        _ if called_tools => "tool_use",
        _ => "end_turn",
    }
}

/// Token counts as Claude Code reads them. Without the provider's, the
/// estimates stand in.
pub(crate) fn usage(u: Option<&Value>, input_estimate: u64, output_estimate: u64) -> Value {
    let num = |v: Option<&Value>| v.and_then(Value::as_u64);
    let Some(u) = u.filter(|u| u.is_object()) else {
        return json!({
            "input_tokens": input_estimate,
            "output_tokens": output_estimate,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 0,
        });
    };
    let prompt = num(u.get("prompt_tokens")).unwrap_or(input_estimate);
    let cached = num(u.pointer("/prompt_tokens_details/cached_tokens"))
        .or_else(|| num(u.get("prompt_cache_hit_tokens")))
        .unwrap_or(0)
        .min(prompt);
    json!({
        "input_tokens": prompt - cached,
        "output_tokens": num(u.get("completion_tokens")).unwrap_or(output_estimate),
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": cached,
    })
}

/// A tool call's arguments as the JSON object Claude Code parses; `{}` for
/// none, or for arguments that aren't JSON (the tool then says what it is
/// missing, and the model tries again).
pub(crate) fn arguments(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return "{}".to_owned();
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(v) if v.is_object() => raw.to_owned(),
        _ => "{}".to_owned(),
    }
}

/// Text in a message's `content`: a string, or parts with text.
pub(crate) fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect(),
        _ => String::new(),
    }
}

/// The provider's whole answer as one Messages API message.
pub(crate) fn message(resp: &Value, model: &str, names: &Names, estimate: u64) -> Value {
    let choice = resp.pointer("/choices/0");
    let msg = choice.and_then(|c| c.get("message"));
    let mut content = Vec::new();
    let mut text = content_text(msg.and_then(|m| m.get("content")));
    if let Some(refusal) = msg
        .and_then(|m| m.get("refusal"))
        .and_then(Value::as_str)
        .filter(|r| !r.is_empty())
    {
        text.push_str(refusal);
    }
    let output_estimate = (text.len() as u64).div_ceil(4);
    if !text.is_empty() {
        content.push(json!({ "type": "text", "text": text }));
    }
    for call in msg
        .and_then(|m| m.get("tool_calls"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let f = call.get("function");
        let name = f
            .and_then(|f| f.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let args = arguments(
            f.and_then(|f| f.get("arguments"))
                .and_then(Value::as_str)
                .unwrap_or(""),
        );
        content.push(json!({
            "type": "tool_use",
            "id": tool_id(call.get("id").and_then(Value::as_str)),
            "name": names.original(name),
            "input": serde_json::from_str::<Value>(&args).unwrap_or_else(|_| json!({})),
        }));
    }
    let called = content.iter().any(|b| b["type"] == "tool_use");
    let finish = choice
        .and_then(|c| c.get("finish_reason"))
        .and_then(Value::as_str);
    json!({
        "id": message_id(resp.get("id").and_then(Value::as_str)),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason(finish, called),
        "stop_sequence": null,
        "usage": usage(resp.get("usage"), estimate, output_estimate),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_with_tool_calls_becomes_blocks() {
        let resp = json!({
            "id": "chatcmpl-9x.1",
            "choices": [{ "index": 0, "finish_reason": "tool_calls", "message": {
                "role": "assistant",
                "content": "Let me look.",
                "reasoning_content": "thinking out loud",
                "tool_calls": [
                    { "id": "call_a", "type": "function", "function": { "name": "Bash", "arguments": "{\"command\":\"ls\"}" } },
                    { "type": "function", "function": { "name": "Read", "arguments": "not json" } }
                ]
            }}],
            "usage": { "prompt_tokens": 1200, "completion_tokens": 40, "prompt_tokens_details": { "cached_tokens": 1000 } }
        });
        let m = message(&resp, "claude-opus-5-5", &Names::default(), 0);
        assert_eq!(m["id"], "msg_chatcmpl-9x_1");
        assert_eq!(m["model"], "claude-opus-5-5");
        assert_eq!(m["stop_reason"], "tool_use");
        let c = m["content"].as_array().cloned().unwrap_or_default();
        assert_eq!(c.len(), 3);
        assert_eq!(c[0], json!({ "type": "text", "text": "Let me look." }));
        assert_eq!(c[1]["input"], json!({ "command": "ls" }));
        assert_eq!(c[1]["id"], "call_a");
        assert_eq!(c[2]["input"], json!({}));
        assert!(
            c[2]["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("toolu_"))
        );
        assert_eq!(m["usage"]["input_tokens"], 200);
        assert_eq!(m["usage"]["cache_read_input_tokens"], 1000);
        assert_eq!(m["usage"]["output_tokens"], 40);
    }

    #[test]
    fn stop_reasons_map() {
        assert_eq!(stop_reason(Some("stop"), false), "end_turn");
        assert_eq!(stop_reason(Some("stop"), true), "tool_use");
        assert_eq!(stop_reason(Some("length"), true), "max_tokens");
        assert_eq!(stop_reason(None, false), "end_turn");
        assert_eq!(stop_reason(Some("content_filter"), false), "refusal");
    }

    #[test]
    fn without_usage_the_estimates_stand_in() {
        let u = usage(None, 900, 12);
        assert_eq!(
            (u["input_tokens"].as_u64(), u["output_tokens"].as_u64()),
            (Some(900), Some(12))
        );
    }
}
