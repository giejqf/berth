//! A Messages API request as a Chat Completions request.
//!
//! What the provider can't take is left out: thinking (its signatures are
//! Anthropic's), server tools (web search runs on Anthropic's side), cache
//! markers. A tool's result becomes a `tool` message; images it returned
//! follow in a user message, since tool messages carry text only.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

/// Tool names as they go upstream, and back. OpenAI's are at most 64 of
/// `[A-Za-z0-9_-]`; Claude Code's MCP tools can be longer.
#[derive(Debug, Default, Clone)]
pub(crate) struct Names {
    back: HashMap<String, String>,
}

impl Names {
    pub(crate) fn wire(&mut self, name: &str) -> String {
        let wire = wire_name(name);
        if wire != name {
            self.back.insert(wire.clone(), name.to_owned());
        }
        wire
    }

    pub(crate) fn original(&self, wire: &str) -> String {
        self.back
            .get(wire)
            .cloned()
            .unwrap_or_else(|| wire.to_owned())
    }
}

/// `name` as OpenAI takes it: as it is when it fits, else cut and with a
/// hash of the whole, so two names never meet.
fn wire_name(name: &str) -> String {
    let fits = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if fits {
        return name.to_owned();
    }
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(55)
        .collect();
    format!("{clean}_{:08x}", fnv(name))
}

fn fnv(s: &str) -> u32 {
    s.bytes().fold(0x811c_9dc5_u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

/// The request as it goes upstream, before the model's own shape applies
/// ([`crate::shape::Shape`]).
#[derive(Debug, Clone)]
pub(crate) struct Outgoing {
    pub body: Map<String, Value>,
    pub names: Names,
    pub stream: bool,
    /// `max_tokens` as Claude Code asked.
    pub max_tokens: Option<u64>,
    /// Input tokens, estimated, until the provider counts them.
    pub estimate: u64,
}

pub(crate) fn translate(req: &Value, model: &str) -> Outgoing {
    let mut names = Names::default();
    let mut messages = Vec::new();
    if let Some(system) = system_text(req.get("system")) {
        messages.push(json!({ "role": "system", "content": system }));
    }
    for m in req
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let content = m.get("content").unwrap_or(&Value::Null);
        match m.get("role").and_then(Value::as_str) {
            Some("assistant") => assistant(content, &mut messages, &mut names),
            _ => user(content, &mut messages),
        }
    }

    let mut body = Map::new();
    body.insert("model".into(), model.into());
    body.insert("messages".into(), messages.into());
    let tools = tools(req, &mut names);
    if !tools.is_empty() {
        body.insert("tools".into(), tools.into());
        if let Some(choice) = req.get("tool_choice") {
            if let Some(c) = tool_choice(choice, &mut names) {
                body.insert("tool_choice".into(), c);
            }
            if choice.get("disable_parallel_tool_use") == Some(&Value::Bool(true)) {
                body.insert("parallel_tool_calls".into(), false.into());
            }
        }
    }
    for key in ["temperature", "top_p"] {
        if let Some(v) = req.get(key).filter(|v| v.is_number()) {
            body.insert(key.into(), v.clone());
        }
    }
    if let Some(stops) = req.get("stop_sequences").and_then(Value::as_array)
        && !stops.is_empty()
    {
        body.insert("stop".into(), stops.iter().take(4).cloned().collect());
    }
    let stream = req.get("stream") == Some(&Value::Bool(true));
    if stream {
        body.insert("stream".into(), true.into());
        body.insert("stream_options".into(), json!({ "include_usage": true }));
    }
    Outgoing {
        body,
        names,
        stream,
        max_tokens: req.get("max_tokens").and_then(Value::as_u64),
        estimate: estimate_request(req),
    }
}

fn system_text(system: Option<&Value>) -> Option<String> {
    let text = match system? {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => return None,
    };
    (!text.trim().is_empty()).then_some(text)
}

/// A message's content blocks; a string is one text block.
enum Block<'a> {
    Text(&'a str),
    Other(&'a Value),
}

fn blocks(content: &Value) -> Vec<Block<'_>> {
    match content {
        Value::String(s) => vec![Block::Text(s)],
        Value::Array(a) => a
            .iter()
            .map(|b| match kind(b) {
                "text" => Block::Text(b.get("text").and_then(Value::as_str).unwrap_or("")),
                _ => Block::Other(b),
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn kind(b: &Value) -> &str {
    b.get("type").and_then(Value::as_str).unwrap_or("")
}

fn text_part(text: &str) -> Value {
    json!({ "type": "text", "text": text })
}

fn image_part(b: &Value) -> Option<Value> {
    let source = b.get("source")?;
    let url = match source.get("type").and_then(Value::as_str) {
        Some("base64") => format!(
            "data:{};base64,{}",
            source.get("media_type")?.as_str()?,
            source.get("data")?.as_str()?
        ),
        Some("url") => source.get("url")?.as_str()?.to_owned(),
        _ => return None,
    };
    Some(json!({ "type": "image_url", "image_url": { "url": url } }))
}

/// A document block as text: its own text when it has one, else a note
/// that it was left out (a PDF).
fn document_text(b: &Value) -> String {
    let source = b.get("source");
    let text = source
        .filter(|s| s.get("type").and_then(Value::as_str) == Some("text"))
        .and_then(|s| s.get("data"))
        .and_then(Value::as_str);
    let title = b
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("A document");
    match text {
        Some(t) => format!("{title}:\n{t}"),
        None => format!("[{title} was attached, but this model reads only text and images.]"),
    }
}

fn user(content: &Value, out: &mut Vec<Value>) {
    let mut tool_messages = Vec::new();
    // Images tools returned, then the user's own parts.
    let mut returned = Vec::new();
    let mut parts = Vec::new();
    for b in blocks(content) {
        match b {
            Block::Text(t) => {
                if !t.is_empty() {
                    parts.push(text_part(t));
                }
            }
            Block::Other(v) => match kind(v) {
                "image" => parts.extend(image_part(v)),
                "document" => parts.push(text_part(&document_text(v))),
                "tool_result" => {
                    let id = v.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                    let (text, images) = tool_result(v);
                    tool_messages
                        .push(json!({ "role": "tool", "tool_call_id": id, "content": text }));
                    if !images.is_empty() {
                        returned.push(text_part(&format!("The images tool call {id} returned:")));
                        returned.extend(images);
                    }
                }
                _ => {}
            },
        }
    }
    out.extend(tool_messages);
    returned.extend(parts);
    if returned.is_empty() {
        return;
    }
    let all_text = returned.iter().all(|p| kind(p) == "text");
    let content = if all_text {
        returned
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n")
            .into()
    } else {
        Value::Array(returned)
    };
    out.push(json!({ "role": "user", "content": content }));
}

/// A tool result's text, and the images it returned.
fn tool_result(b: &Value) -> (String, Vec<Value>) {
    let mut text = Vec::new();
    let mut images = Vec::new();
    for part in blocks(b.get("content").unwrap_or(&Value::Null)) {
        match part {
            Block::Text(t) => text.push(t.to_owned()),
            Block::Other(v) => match kind(v) {
                "image" => images.extend(image_part(v)),
                "document" => text.push(document_text(v)),
                _ => {}
            },
        }
    }
    let mut text = text.join("\n");
    if text.trim().is_empty() {
        text = if images.is_empty() {
            "(no output)".to_owned()
        } else {
            "(images follow)".to_owned()
        };
    }
    if b.get("is_error") == Some(&Value::Bool(true)) && !text.starts_with("Error") {
        text = format!("Error: {text}");
    }
    (text, images)
}

fn assistant(content: &Value, out: &mut Vec<Value>, names: &mut Names) {
    let mut text = String::new();
    let mut calls = Vec::new();
    for b in blocks(content) {
        match b {
            Block::Text(t) => text.push_str(t),
            Block::Other(v) if kind(v) == "tool_use" => {
                let name = v.get("name").and_then(Value::as_str).unwrap_or("");
                let input = v.get("input").cloned().unwrap_or_else(|| json!({}));
                calls.push(json!({
                    "id": v.get("id").and_then(Value::as_str).unwrap_or(""),
                    "type": "function",
                    "function": { "name": names.wire(name), "arguments": input.to_string() },
                }));
            }
            // Thinking and server tools: the provider can't take them back.
            Block::Other(_) => {}
        }
    }
    if text.is_empty() && calls.is_empty() {
        return;
    }
    let mut m = Map::new();
    m.insert("role".into(), "assistant".into());
    m.insert(
        "content".into(),
        if text.is_empty() {
            Value::Null
        } else {
            text.into()
        },
    );
    if !calls.is_empty() {
        m.insert("tool_calls".into(), calls.into());
    }
    out.push(Value::Object(m));
}

/// Claude Code's tools as functions. Server tools (web search, code
/// execution) run on Anthropic's side and have no schema here: left out.
fn tools(req: &Value, names: &mut Names) -> Vec<Value> {
    req.get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let mut schema = t.get("input_schema")?.clone();
            let name = t.get("name")?.as_str()?;
            if let Some(o) = schema.as_object_mut() {
                o.remove("$schema");
            }
            Some(json!({
                "type": "function",
                "function": {
                    "name": names.wire(name),
                    "description": t.get("description").and_then(Value::as_str).unwrap_or(""),
                    "parameters": schema,
                },
            }))
        })
        .collect()
}

fn tool_choice(choice: &Value, names: &mut Names) -> Option<Value> {
    Some(match choice.get("type").and_then(Value::as_str)? {
        "auto" => "auto".into(),
        "any" => "required".into(),
        "none" => "none".into(),
        "tool" => {
            let name = choice.get("name")?.as_str()?;
            json!({ "type": "function", "function": { "name": names.wire(name) } })
        }
        _ => return None,
    })
}

/// About how many tokens the request's input is: four characters a token,
/// an image at what Claude counts for a typical one (base64 would be
/// counted as text a thousand times over).
pub(crate) fn estimate_request(req: &Value) -> u64 {
    ["system", "messages", "tools"]
        .iter()
        .filter_map(|k| req.get(*k))
        .map(estimate)
        .sum()
}

fn estimate(v: &Value) -> u64 {
    match v {
        Value::String(s) => (s.len() as u64).div_ceil(4),
        Value::Array(a) => a.iter().map(estimate).sum(),
        Value::Object(o) => match o.get("type").and_then(Value::as_str) {
            Some("image") => 1600,
            Some("document") if o.get("source").is_some_and(|s| kind(s) == "base64") => 3000,
            _ => o
                .iter()
                .filter(|(k, _)| k.as_str() != "signature")
                .map(|(_, v)| estimate(v))
                .sum(),
        },
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation() -> Value {
        json!({
            "model": "claude-opus-5-5",
            "max_tokens": 32000,
            "stream": true,
            "system": [
                { "type": "text", "text": "You are Claude Code.", "cache_control": { "type": "ephemeral" } },
                { "type": "text", "text": "Be brief." }
            ],
            "tools": [
                { "name": "Bash", "description": "Run a command", "input_schema": {
                    "$schema": "http://json-schema.org/draft-07/schema#",
                    "type": "object", "properties": { "command": { "type": "string" } } } },
                { "type": "web_search_20250305", "name": "web_search", "max_uses": 5 },
                { "name": "mcp__a_very_long_server_name_for_tools__and_an_even_longer_tool_name_x", "input_schema": { "type": "object" } }
            ],
            "tool_choice": { "type": "auto", "disable_parallel_tool_use": true },
            "messages": [
                { "role": "user", "content": "List the files" },
                { "role": "assistant", "content": [
                    { "type": "thinking", "thinking": "hmm", "signature": "sig" },
                    { "type": "text", "text": "Listing them." },
                    { "type": "tool_use", "id": "toolu_1", "name": "Bash", "input": { "command": "ls" } }
                ]},
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "toolu_1", "content": [
                        { "type": "text", "text": "a.txt" },
                        { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "iVBOR" } }
                    ]},
                    { "type": "text", "text": "<system-reminder>ok</system-reminder>" }
                ]}
            ]
        })
    }

    #[test]
    fn a_conversation_becomes_chat_messages() {
        let out = translate(&conversation(), "deepseek-chat");
        let body = Value::Object(out.body);
        assert_eq!(body["model"], "deepseek-chat");
        let m = body["messages"].as_array().cloned().unwrap_or_default();
        assert_eq!(m.len(), 5);
        assert_eq!(
            m[0],
            json!({ "role": "system", "content": "You are Claude Code.\n\nBe brief." })
        );
        assert_eq!(m[1], json!({ "role": "user", "content": "List the files" }));
        assert_eq!(m[2]["role"], "assistant");
        assert_eq!(m[2]["content"], "Listing them.");
        assert_eq!(m[2]["tool_calls"][0]["id"], "toolu_1");
        assert_eq!(m[2]["tool_calls"][0]["function"]["name"], "Bash");
        assert_eq!(
            m[2]["tool_calls"][0]["function"]["arguments"],
            r#"{"command":"ls"}"#
        );
        assert_eq!(
            m[3],
            json!({ "role": "tool", "tool_call_id": "toolu_1", "content": "a.txt" })
        );
        // The returned image, then the reminder, in a user message after the tool's.
        assert_eq!(m[4]["role"], "user");
        let parts = m[4]["content"].as_array().cloned().unwrap_or_default();
        assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,iVBOR");
        assert_eq!(parts[2]["text"], "<system-reminder>ok</system-reminder>");
        assert!(out.stream && out.max_tokens == Some(32000));
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["parallel_tool_calls"], false);
        assert_eq!(body["tool_choice"], "auto");
    }

    #[test]
    fn tools_lose_server_tools_and_long_names_fit() {
        let out = translate(&conversation(), "m");
        let tools = out.body["tools"].as_array().cloned().unwrap_or_default();
        assert_eq!(tools.len(), 2);
        assert!(tools[0]["function"]["parameters"].get("$schema").is_none());
        let long = tools[1]["function"]["name"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        assert!(long.len() <= 64);
        assert_eq!(
            out.names.original(&long),
            "mcp__a_very_long_server_name_for_tools__and_an_even_longer_tool_name_x"
        );
        assert_eq!(out.names.original("Bash"), "Bash");
        assert_ne!(wire_name("a.b"), wire_name("a_b"));
    }

    #[test]
    fn errors_and_empty_results_still_say_something() {
        let (text, _) =
            tool_result(&json!({ "tool_use_id": "t", "content": "boom", "is_error": true }));
        assert_eq!(text, "Error: boom");
        let (text, _) = tool_result(&json!({ "tool_use_id": "t", "content": [] }));
        assert_eq!(text, "(no output)");
    }

    #[test]
    fn a_forced_tool_maps_to_a_function() {
        let mut names = Names::default();
        assert_eq!(
            tool_choice(&json!({ "type": "tool", "name": "Bash" }), &mut names),
            Some(json!({ "type": "function", "function": { "name": "Bash" } }))
        );
        assert_eq!(
            tool_choice(&json!({ "type": "any" }), &mut names),
            Some("required".into())
        );
    }

    #[test]
    fn images_count_as_images_not_as_their_base64() {
        let big = "A".repeat(400_000);
        let req = json!({ "messages": [{ "role": "user", "content": [
            { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": big } },
            { "type": "text", "text": "abcdefgh" }
        ]}]});
        assert!(estimate_request(&req) < 1700);
    }
}
