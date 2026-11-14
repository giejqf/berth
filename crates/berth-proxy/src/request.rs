//! A Messages API request as the provider's API takes it: Chat Completions
//! or the Responses API ([`Api`]).
//!
//! What the provider can't take is left out: Claude's thinking (its
//! signatures are Anthropic's), server tools (web search runs on Anthropic's
//! side), cache markers. The model's own reasoning, kept as thinking Berth
//! signed ([`crate::reasoning`]), goes back to it within a tool loop. A
//! tool's result becomes a tool message (a `function_call_output` item);
//! images it returned follow in a user message, since tool results carry
//! text only. A system message mid-conversation (Claude Code's reminders)
//! joins the user turn it follows, as Claude Code's own reminders do.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use crate::Api;
use crate::reasoning::Kept;
use crate::shape::Shape;

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

fn fnv64(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// A tool call's id as OpenAI takes one: at most 40 of `[A-Za-z0-9_-]`
/// (Chat Completions refuses longer). Another server's longer id (vLLM's
/// `chatcmpl-tool-…`) is hashed to one, the same each time, so a call and
/// its result still meet.
fn wire_id(id: &str) -> String {
    let fits = !id.is_empty()
        && id.len() <= 40
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if fits {
        id.to_owned()
    } else {
        format!("call_{:016x}{:08x}", fnv64(id), fnv(id))
    }
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
    /// Claude Code asks the model to think: the slot's effort applies.
    pub thinking: bool,
}

/// How a request goes to this model: what it reads, and what of its
/// reasoning goes back to it.
#[derive(Debug, Clone, Copy)]
struct Reads {
    api: Api,
    images: bool,
    reasoning_back: bool,
}

pub(crate) fn translate(req: &Value, model: &str, api: Api, shape: &Shape) -> Outgoing {
    let reads = Reads {
        api,
        images: !shape.no_images,
        reasoning_back: !shape.no_reasoning_back,
    };
    let mut names = Names::default();
    let mut items = Vec::new();
    let (tools, cut) = tools(req, &mut names, api, shape.description_max);
    let system = match (system_text(req.get("system")), cut) {
        (s, None) => s,
        (None, Some(cut)) => Some(cut),
        (Some(s), Some(cut)) => Some(format!("{s}\n\n{cut}")),
    };
    if api == Api::ChatCompletions
        && let Some(system) = &system
    {
        items.push(json!({ "role": "system", "content": system }));
    }
    let messages: &[Value] = req
        .get("messages")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice);
    // The tool loop under way: what follows the last prompt (a user turn
    // that isn't tool results). Reasoning goes back to the model only here.
    let prompt_at = messages
        .iter()
        .rposition(|m| {
            m.get("role").and_then(Value::as_str) == Some("user")
                && !blocks(m.get("content").unwrap_or(&Value::Null))
                    .iter()
                    .any(|b| matches!(b, Block::Other(v) if kind(v) == "tool_result"))
        })
        .unwrap_or(0);
    for (i, m) in messages.iter().enumerate() {
        let content = m.get("content").unwrap_or(&Value::Null);
        match m.get("role").and_then(Value::as_str) {
            Some("assistant") => {
                assistant(content, &mut items, &mut names, reads, i > prompt_at);
            }
            Some("system") => reminder(content, &mut items, api),
            _ => user(content, &mut items, reads),
        }
    }

    let mut body = Map::new();
    body.insert("model".into(), model.into());
    match api {
        Api::ChatCompletions => {
            body.insert("messages".into(), items.into());
        }
        Api::Responses => {
            if let Some(system) = system {
                body.insert("instructions".into(), system.into());
            }
            body.insert("input".into(), items.into());
            // Nothing kept on the provider's side: Claude Code sends the
            // whole conversation each time.
            body.insert("store".into(), false.into());
        }
    }
    if !tools.is_empty() {
        body.insert("tools".into(), tools.into());
        if let Some(choice) = req.get("tool_choice") {
            if let Some(c) = tool_choice(choice, &mut names, api) {
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
    // The Responses API has no stop sequences.
    if api == Api::ChatCompletions
        && let Some(stops) = req.get("stop_sequences").and_then(Value::as_array)
        && !stops.is_empty()
    {
        body.insert("stop".into(), stops.iter().take(4).cloned().collect());
    }
    let stream = req.get("stream") == Some(&Value::Bool(true));
    if stream {
        body.insert("stream".into(), true.into());
        if api == Api::ChatCompletions {
            body.insert("stream_options".into(), json!({ "include_usage": true }));
        }
    }
    let thinking = req
        .get("thinking")
        .and_then(|t| t.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|t| t != "disabled")
        || req.pointer("/output_config/effort").is_some();
    Outgoing {
        body,
        names,
        stream,
        max_tokens: req.get("max_tokens").and_then(Value::as_u64),
        estimate: estimate_request(req),
        thinking,
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

/// What a user turn says: text, and images by URL (`data:` for inline ones).
enum Part {
    Text(String),
    Image(String),
}

impl Part {
    fn wire(&self, api: Api) -> Value {
        match (self, api) {
            (Self::Text(t), Api::ChatCompletions) => json!({ "type": "text", "text": t }),
            (Self::Text(t), Api::Responses) => json!({ "type": "input_text", "text": t }),
            (Self::Image(u), Api::ChatCompletions) => {
                json!({ "type": "image_url", "image_url": { "url": u } })
            }
            (Self::Image(u), Api::Responses) => {
                json!({ "type": "input_image", "image_url": u, "detail": "auto" })
            }
        }
    }
}

/// What a model without vision reads where an image was.
const NO_IMAGE: &str = "[An image was here; this model reads no images.]";

impl Part {
    /// An image as this model reads it: itself, or a note.
    fn image(url: String, reads: Reads) -> Self {
        if reads.images {
            Self::Image(url)
        } else {
            Self::Text(NO_IMAGE.to_owned())
        }
    }
}

fn image_url(b: &Value) -> Option<String> {
    let source = b.get("source")?;
    Some(match source.get("type").and_then(Value::as_str) {
        Some("base64") => format!(
            "data:{};base64,{}",
            source.get("media_type")?.as_str()?,
            source.get("data")?.as_str()?
        ),
        Some("url") => source.get("url")?.as_str()?.to_owned(),
        _ => return None,
    })
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

/// A user turn: each tool result as a tool message (`function_call_output`
/// in the Responses API), then a user message with the images tools
/// returned and the user's own parts.
fn user(content: &Value, out: &mut Vec<Value>, reads: Reads) {
    let api = reads.api;
    let mut returned = Vec::new();
    let mut parts = Vec::new();
    for b in blocks(content) {
        match b {
            Block::Text(t) => {
                if !t.is_empty() {
                    parts.push(Part::Text(t.to_owned()));
                }
            }
            Block::Other(v) => match kind(v) {
                "image" => parts.extend(image_url(v).map(|u| Part::image(u, reads))),
                "document" => parts.push(Part::Text(document_text(v))),
                "tool_result" => {
                    let id = wire_id(v.get("tool_use_id").and_then(Value::as_str).unwrap_or(""));
                    let (text, images) = tool_result(v);
                    out.push(match api {
                        Api::ChatCompletions => {
                            json!({ "role": "tool", "tool_call_id": id, "content": text })
                        }
                        Api::Responses => {
                            json!({ "type": "function_call_output", "call_id": id, "output": text })
                        }
                    });
                    if !images.is_empty() {
                        returned.push(Part::Text(format!("The images tool call {id} returned:")));
                        returned.extend(images.into_iter().map(|u| Part::image(u, reads)));
                    }
                }
                _ => {}
            },
        }
    }
    returned.extend(parts);
    if returned.is_empty() {
        return;
    }
    let content: Value = if returned.iter().all(|p| matches!(p, Part::Text(_))) {
        returned
            .iter()
            .filter_map(|p| match p {
                Part::Text(t) => Some(t.as_str()),
                Part::Image(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n")
            .into()
    } else {
        returned.iter().map(|p| p.wire(api)).collect()
    };
    out.push(json!({ "role": "user", "content": content }));
}

/// A tool result's text, and the images it returned (their URLs).
fn tool_result(b: &Value) -> (String, Vec<String>) {
    let mut text = Vec::new();
    let mut images = Vec::new();
    for part in blocks(b.get("content").unwrap_or(&Value::Null)) {
        match part {
            Block::Text(t) => text.push(t.to_owned()),
            Block::Other(v) => match kind(v) {
                "image" => images.extend(image_url(v)),
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

/// A system message in the conversation (Claude Code's reminders: the
/// model, the tokens left): a reminder in the user turn it follows, or a
/// user turn of its own. Not every server takes a system message after the
/// first, and the model reads it there as it reads Claude Code's others.
fn reminder(content: &Value, out: &mut Vec<Value>, api: Api) {
    let text: Vec<&str> = blocks(content)
        .into_iter()
        .filter_map(|b| match b {
            Block::Text(t) if !t.trim().is_empty() => Some(t),
            _ => None,
        })
        .collect();
    if text.is_empty() {
        return;
    }
    let note = format!(
        "<system-reminder>\n{}\n</system-reminder>",
        text.join("\n\n")
    );
    let last_user = out.last_mut().filter(|m| {
        m.get("role").and_then(Value::as_str) == Some("user") && m.get("type").is_none()
    });
    match last_user.and_then(|m| m.get_mut("content")) {
        Some(Value::String(s)) => {
            s.push_str("\n\n");
            s.push_str(&note);
        }
        Some(Value::Array(parts)) => parts.push(Part::Text(note).wire(api)),
        _ => out.push(json!({ "role": "user", "content": note })),
    }
}

/// An assistant turn: its text and its tool calls. One message with
/// `tool_calls` in Chat Completions; a message, then a `function_call` item
/// for each call, in the Responses API. In the tool loop under way
/// (`in_loop`), the model's reasoning goes first: its reasoning items, or
/// its `reasoning_content`.
fn assistant(
    content: &Value,
    out: &mut Vec<Value>,
    names: &mut Names,
    reads: Reads,
    in_loop: bool,
) {
    let api = reads.api;
    let mut text = String::new();
    let mut calls = Vec::new();
    let mut items = Vec::new();
    let mut reasoning = String::new();
    for b in blocks(content) {
        match b {
            Block::Text(t) => text.push_str(t),
            Block::Other(v) if kind(v) == "tool_use" => {
                let name = names.wire(v.get("name").and_then(Value::as_str).unwrap_or(""));
                let id = wire_id(v.get("id").and_then(Value::as_str).unwrap_or(""));
                let args = v
                    .get("input")
                    .cloned()
                    .unwrap_or_else(|| json!({}))
                    .to_string();
                calls.push(match api {
                    Api::ChatCompletions => json!({
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": args },
                    }),
                    Api::Responses => json!({
                        "type": "function_call",
                        "call_id": id,
                        "name": name,
                        "arguments": args,
                    }),
                });
            }
            Block::Other(v) if kind(v) == "thinking" && in_loop && reads.reasoning_back => {
                let signature = v.get("signature").and_then(Value::as_str).unwrap_or("");
                let thought = v.get("thinking").and_then(Value::as_str).unwrap_or("");
                match (Kept::read(signature), api) {
                    (Some(Kept::Item { id, encrypted }), Api::Responses) => {
                        let summary: Vec<Value> = (!thought.is_empty()
                            && thought != crate::reasoning::UNSUMMARIZED)
                            .then(|| json!({ "type": "summary_text", "text": thought }))
                            .into_iter()
                            .collect();
                        items.push(json!({
                            "type": "reasoning",
                            "id": id,
                            "summary": summary,
                            "encrypted_content": encrypted,
                        }));
                    }
                    (Some(Kept::Content), Api::ChatCompletions) => reasoning.push_str(thought),
                    _ => {}
                }
            }
            // Claude's thinking and server tools: the provider can't take them.
            Block::Other(_) => {}
        }
    }
    if text.trim().is_empty() && calls.is_empty() {
        return;
    }
    // Reasoning goes back only before a call it led to: a reasoning item
    // must be followed by what it produced.
    let reasoning_back = !calls.is_empty();
    match api {
        Api::ChatCompletions => {
            let mut m = Map::new();
            m.insert("role".into(), "assistant".into());
            m.insert(
                "content".into(),
                if text.trim().is_empty() {
                    Value::Null
                } else {
                    text.into()
                },
            );
            if !calls.is_empty() {
                m.insert("tool_calls".into(), calls.into());
            }
            if reasoning_back && !reasoning.is_empty() {
                m.insert("reasoning_content".into(), reasoning.into());
            }
            out.push(Value::Object(m));
        }
        Api::Responses => {
            if reasoning_back {
                out.extend(items);
            }
            if !text.trim().is_empty() {
                out.push(json!({ "role": "assistant", "content": text }));
            }
            out.extend(calls);
        }
    }
}

/// Claude Code's tools as functions. Server tools (web search, code
/// execution) run on Anthropic's side and have no schema here: left out.
/// The Responses API's functions are flat, and strict unless told not to
/// be (Claude Code's schemas aren't written for strict mode). A model that
/// takes descriptions only up to `max` gets them cut there, and the whole of
/// each one cut in a note for the system prompt.
fn tools(
    req: &Value,
    names: &mut Names,
    api: Api,
    max: Option<usize>,
) -> (Vec<Value>, Option<String>) {
    let mut cut = Vec::new();
    let tools = req
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let mut schema = t.get("input_schema")?.clone();
            let original = t.get("name")?.as_str()?;
            let name = names.wire(original);
            if let Some(o) = schema.as_object_mut() {
                o.remove("$schema");
            }
            let full = t.get("description").and_then(Value::as_str).unwrap_or("");
            let description = match max {
                Some(max) if full.chars().count() > max => {
                    cut.push(format!("## {name}\n\n{full}"));
                    const MORE: &str = "… (The whole description is in the system prompt.)";
                    let head: String = full
                        .chars()
                        .take(max.saturating_sub(MORE.chars().count()))
                        .collect();
                    format!("{head}{MORE}")
                }
                _ => full.to_owned(),
            };
            Some(match api {
                Api::ChatCompletions => json!({
                    "type": "function",
                    "function": { "name": name, "description": description, "parameters": schema },
                }),
                Api::Responses => json!({
                    "type": "function",
                    "name": name,
                    "description": description,
                    "parameters": schema,
                    "strict": false,
                }),
            })
        })
        .collect();
    let note = (!cut.is_empty()).then(|| {
        format!(
            "# Tool descriptions\n\nThese tools' descriptions were too long for this API, so they are here in full.\n\n{}",
            cut.join("\n\n")
        )
    });
    (tools, note)
}

fn tool_choice(choice: &Value, names: &mut Names, api: Api) -> Option<Value> {
    Some(match choice.get("type").and_then(Value::as_str)? {
        "auto" => "auto".into(),
        "any" => "required".into(),
        "none" => "none".into(),
        "tool" => {
            let name = names.wire(choice.get("name")?.as_str()?);
            match api {
                Api::ChatCompletions => json!({ "type": "function", "function": { "name": name } }),
                Api::Responses => json!({ "type": "function", "name": name }),
            }
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
        let out = translate(
            &conversation(),
            "deepseek-chat",
            Api::ChatCompletions,
            &Shape::default(),
        );
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
    fn a_conversation_becomes_responses_api_input() {
        let out = translate(
            &conversation(),
            "gpt-5-codex",
            Api::Responses,
            &Shape::default(),
        );
        let body = Value::Object(out.body);
        assert_eq!(body["instructions"], "You are Claude Code.\n\nBe brief.");
        assert_eq!(body["store"], false);
        assert!(body.get("messages").is_none() && body.get("stream_options").is_none());
        let input = body["input"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            input[0],
            json!({ "role": "user", "content": "List the files" })
        );
        assert_eq!(
            input[1],
            json!({ "role": "assistant", "content": "Listing them." })
        );
        assert_eq!(
            input[2],
            json!({ "type": "function_call", "call_id": "toolu_1", "name": "Bash", "arguments": r#"{"command":"ls"}"# })
        );
        assert_eq!(
            input[3],
            json!({ "type": "function_call_output", "call_id": "toolu_1", "output": "a.txt" })
        );
        let parts = input[4]["content"].as_array().cloned().unwrap_or_default();
        assert_eq!(parts[0]["type"], "input_text");
        assert_eq!(parts[1]["type"], "input_image");
        assert_eq!(parts[1]["image_url"], "data:image/png;base64,iVBOR");
        let tool = &body["tools"][0];
        assert_eq!(tool["name"], "Bash");
        assert_eq!(tool["strict"], false);
        assert!(tool["parameters"].get("$schema").is_none());
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(
            tool_choice(
                &json!({ "type": "tool", "name": "Bash" }),
                &mut Names::default(),
                Api::Responses
            ),
            Some(json!({ "type": "function", "name": "Bash" }))
        );
    }

    #[test]
    fn tools_lose_server_tools_and_long_names_fit() {
        let out = translate(
            &conversation(),
            "m",
            Api::ChatCompletions,
            &Shape::default(),
        );
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
            tool_choice(
                &json!({ "type": "tool", "name": "Bash" }),
                &mut names,
                Api::ChatCompletions
            ),
            Some(json!({ "type": "function", "function": { "name": "Bash" } }))
        );
        assert_eq!(
            tool_choice(&json!({ "type": "any" }), &mut names, Api::ChatCompletions),
            Some("required".into())
        );
    }

    #[test]
    fn system_messages_join_the_user_turn_as_reminders() {
        let req = json!({ "messages": [
            { "role": "user", "content": "Fix it" },
            { "role": "system", "content": "You are powered by the model gpt-5." },
            { "role": "assistant", "content": [{ "type": "text", "text": "Done." }] },
            { "role": "system", "content": [{ "type": "text", "text": "<total_tokens>9 left</total_tokens>" }] },
        ]});
        let out = translate(&req, "m", Api::ChatCompletions, &Shape::default());
        let m = out.body["messages"].as_array().cloned().unwrap_or_default();
        assert_eq!(m.len(), 3);
        assert_eq!(
            m[0]["content"],
            "Fix it\n\n<system-reminder>\nYou are powered by the model gpt-5.\n</system-reminder>"
        );
        assert_eq!(m[2]["role"], "user");
        assert!(
            m[2]["content"]
                .as_str()
                .is_some_and(|c| c.contains("total_tokens"))
        );
    }

    #[test]
    fn reasoning_goes_back_within_the_tool_loop() {
        let item = Kept::Item {
            id: "rs_9".into(),
            encrypted: "gAAAA".into(),
        };
        let req = json!({ "messages": [
            { "role": "user", "content": "Earlier" },
            { "role": "assistant", "content": [
                { "type": "thinking", "thinking": "old", "signature": item.signature() },
                { "type": "tool_use", "id": "call_1", "name": "Bash", "input": {} }
            ]},
            { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "call_1", "content": "ok" }] },
            { "role": "assistant", "content": [{ "type": "text", "text": "Done." }] },
            { "role": "user", "content": "Now" },
            { "role": "assistant", "content": [
                { "type": "thinking", "thinking": "Plan", "signature": item.signature() },
                { "type": "thinking", "thinking": "Claude's", "signature": "EqQB" },
                { "type": "tool_use", "id": "call_2", "name": "Bash", "input": {} }
            ]},
            { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "call_2", "content": "ok" }] },
        ]});
        let out = translate(&req, "gpt-5", Api::Responses, &Shape::default());
        let input = out.body["input"].as_array().cloned().unwrap_or_default();
        let reasoning: Vec<&Value> = input.iter().filter(|i| i["type"] == "reasoning").collect();
        // Only the loop under way's, before its call.
        assert_eq!(reasoning.len(), 1);
        assert_eq!(
            *reasoning[0],
            json!({ "type": "reasoning", "id": "rs_9", "summary": [{ "type": "summary_text", "text": "Plan" }], "encrypted_content": "gAAAA" })
        );
        let at = input.iter().position(|i| i["type"] == "reasoning");
        assert_eq!(at.map(|a| &input[a + 1]["call_id"]), Some(&json!("call_2")));
        // Once refused, none goes back.
        let shape = Shape {
            no_reasoning_back: true,
            ..Shape::default()
        };
        let out = translate(&req, "gpt-5", Api::Responses, &shape);
        assert!(!out.body["input"].to_string().contains("reasoning"));
        // Chat Completions: the reasoning text, as `reasoning_content`.
        let chat = json!({ "messages": [
            { "role": "user", "content": "Now" },
            { "role": "assistant", "content": [
                { "type": "thinking", "thinking": "Let me run it", "signature": Kept::Content.signature() },
                { "type": "tool_use", "id": "call_3", "name": "Bash", "input": {} }
            ]},
            { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "call_3", "content": "ok" }] },
        ]});
        let out = translate(
            &chat,
            "deepseek-reasoner",
            Api::ChatCompletions,
            &Shape::default(),
        );
        assert_eq!(
            out.body["messages"][1]["reasoning_content"],
            "Let me run it"
        );
    }

    #[test]
    fn long_call_ids_are_shortened_alike_on_both_sides() {
        let long = "chatcmpl-tool-0123456789abcdef0123456789abcdef";
        let req = json!({ "messages": [
            { "role": "user", "content": "Go" },
            { "role": "assistant", "content": [{ "type": "tool_use", "id": long, "name": "Bash", "input": {} }] },
            { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": long, "content": "ok" }] },
        ]});
        let out = translate(&req, "m", Api::ChatCompletions, &Shape::default());
        let m = out.body["messages"].as_array().cloned().unwrap_or_default();
        let id = m[1]["tool_calls"][0]["id"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        assert!(id.len() <= 40 && id.starts_with("call_"));
        assert_eq!(m[2]["tool_call_id"], json!(id));
        assert_eq!(wire_id("toolu_01ABC"), "toolu_01ABC");
    }

    #[test]
    fn descriptions_too_long_for_the_model_go_whole_in_the_system_prompt() {
        let long = "Run a command. ".repeat(200);
        let req = json!({
            "system": "You are Claude Code.",
            "tools": [{ "name": "Bash", "description": long, "input_schema": { "type": "object" } }],
            "messages": [{ "role": "user", "content": "hi" }],
        });
        let shape = Shape {
            description_max: Some(1024),
            ..Shape::default()
        };
        let out = translate(&req, "m", Api::ChatCompletions, &shape);
        let d = out.body["tools"][0]["function"]["description"]
            .as_str()
            .unwrap_or("");
        assert!(d.chars().count() <= 1024, "{}", d.len());
        let system = out.body["messages"][0]["content"].as_str().unwrap_or("");
        assert!(system.starts_with("You are Claude Code.") && system.contains(&long));
    }

    #[test]
    fn a_model_without_vision_reads_a_note_for_each_image() {
        let shape = Shape {
            no_images: true,
            ..Shape::default()
        };
        let out = translate(&conversation(), "m", Api::ChatCompletions, &shape);
        let text = out.body["messages"].to_string();
        assert!(!text.contains("image_url") && text.contains("reads no images"));
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
