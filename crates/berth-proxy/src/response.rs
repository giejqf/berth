//! A Chat Completions or Responses API answer as a Messages API message.

use std::collections::HashSet;

use serde_json::{Value, json};

use crate::Api;
use crate::reasoning::{Kept, UNSUMMARIZED};
use crate::request::Names;

/// The message's id: the provider's, kept recognizable.
pub(crate) fn message_id(upstream: Option<&str>) -> String {
    match upstream.filter(|id| !id.is_empty()) {
        Some(id) => format!("msg_{}", clean_id(id)),
        None => format!("msg_{}", uuid::Uuid::new_v4().simple()),
    }
}

/// A tool call's id as Claude Code takes one (`[A-Za-z0-9_-]`, unique in
/// its message); a new one when the provider sent none, or one it already
/// used (servers that number calls `0`, `1` in every answer).
pub(crate) fn tool_id(upstream: Option<&str>, used: &mut HashSet<String>) -> String {
    let id = upstream
        .map(clean_id)
        .filter(|id| id.len() >= 6 && !used.contains(id))
        .unwrap_or_else(|| format!("toolu_{}", uuid::Uuid::new_v4().simple()));
    used.insert(id.clone());
    id
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

/// What an answer the provider's content filter stopped ends with.
pub(crate) const FILTERED: &str = "(The provider's content filter stopped this answer here.)";

/// Why the model stopped, in Messages API terms. A provider that ends with
/// `stop` after calling tools still called them.
pub(crate) fn stop_reason(finish: Option<&str>, called_tools: bool) -> &'static str {
    match finish {
        Some("length") => "max_tokens",
        // Not a refusal: Claude Code would say Anthropic's usage policy
        // was broken. The answer ends with [`FILTERED`].
        Some("content_filter") => "end_turn",
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
    // Chat Completions' names, or the Responses API's.
    let prompt = num(u.get("prompt_tokens"))
        .or_else(|| num(u.get("input_tokens")))
        .unwrap_or(input_estimate);
    let cached = num(u.pointer("/prompt_tokens_details/cached_tokens"))
        .or_else(|| num(u.pointer("/input_tokens_details/cached_tokens")))
        .or_else(|| num(u.get("prompt_cache_hit_tokens")))
        .unwrap_or(0)
        .min(prompt);
    let output = num(u.get("completion_tokens")).or_else(|| num(u.get("output_tokens")));
    json!({
        "input_tokens": prompt - cached,
        "output_tokens": output.unwrap_or(output_estimate),
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

/// A piece of an answer's text: the answer, or reasoning the model wrote
/// into it between `<think>` tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Piece {
    Text(String),
    Thinking(String),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum TagState {
    #[default]
    Start,
    Inside,
    After,
}

const OPEN: &str = "<think>";
const CLOSE: &str = "</think>";

/// `<think>…</think>` at the start of an answer, as DeepSeek-R1 and QwQ
/// write it through servers that don't take it out: reasoning, not the
/// answer. Fed as it streams; a tag split across chunks still reads.
#[derive(Debug, Default)]
pub(crate) struct ThinkTags {
    state: TagState,
    held: String,
}

impl ThinkTags {
    pub(crate) fn push(&mut self, s: &str) -> Vec<Piece> {
        self.held.push_str(s);
        let mut out = Vec::new();
        loop {
            match self.state {
                TagState::Start => {
                    let t = self.held.trim_start();
                    if t.is_empty() || (OPEN.starts_with(t) && t.len() < OPEN.len()) {
                        return out;
                    }
                    if let Some(rest) = t.strip_prefix(OPEN) {
                        self.held = rest.to_owned();
                        self.state = TagState::Inside;
                    } else {
                        self.state = TagState::After;
                    }
                }
                TagState::Inside => {
                    if let Some(at) = self.held.find(CLOSE) {
                        let thought = self.held[..at].to_owned();
                        if !thought.trim().is_empty() {
                            out.push(Piece::Thinking(thought));
                        }
                        self.held = self.held[at + CLOSE.len()..].to_owned();
                        self.state = TagState::After;
                        continue;
                    }
                    // All but what could be the start of the closing tag.
                    let keep = (1..CLOSE.len())
                        .rev()
                        .find(|&n| self.held.ends_with(&CLOSE[..n]))
                        .unwrap_or(0);
                    let cut = self.held.len() - keep;
                    if cut > 0 {
                        out.push(Piece::Thinking(self.held[..cut].to_owned()));
                        self.held = self.held[cut..].to_owned();
                    }
                    return out;
                }
                TagState::After => {
                    if !self.held.is_empty() {
                        out.push(Piece::Text(std::mem::take(&mut self.held)));
                    }
                    return out;
                }
            }
        }
    }

    /// What is still held when the answer ends.
    pub(crate) fn finish(&mut self) -> Option<Piece> {
        let held = std::mem::take(&mut self.held);
        if held.is_empty() {
            return None;
        }
        Some(match self.state {
            TagState::Inside => Piece::Thinking(held),
            _ => Piece::Text(held),
        })
    }
}

/// Reasoning a Chat Completions message or delta carries alongside its
/// text: `reasoning_content` (DeepSeek, vLLM, Qwen), `reasoning`
/// (OpenRouter, Ollama).
pub(crate) fn chat_reasoning(m: &Value) -> String {
    ["reasoning_content", "reasoning"]
        .iter()
        .find_map(|k| m.get(*k).and_then(Value::as_str).filter(|r| !r.is_empty()))
        .unwrap_or("")
        .to_owned()
}

/// A Responses API reasoning item as the thinking block that keeps it: its
/// summary to read, and what goes back to the model (its encrypted
/// content). `None` for one with neither.
pub(crate) fn reasoning_item(item: &Value) -> Option<(String, Kept)> {
    let summary: Vec<&str> = item
        .get("summary")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .filter(|t| !t.is_empty())
        .collect();
    let summary = summary.join("\n\n");
    let id = item.get("id").and_then(Value::as_str).unwrap_or("");
    let encrypted = item
        .get("encrypted_content")
        .and_then(Value::as_str)
        .filter(|e| !e.is_empty() && !id.is_empty());
    match encrypted {
        Some(e) => Some((
            if summary.is_empty() {
                UNSUMMARIZED.to_owned()
            } else {
                summary
            },
            Kept::Item {
                id: id.to_owned(),
                encrypted: e.to_owned(),
            },
        )),
        None if !summary.is_empty() => Some((summary, Kept::Shown)),
        None => None,
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

/// What an answer said: its reasoning, its text, its tool calls (id, name,
/// arguments) and why it ended, in Chat Completions' words.
#[derive(Debug, Default)]
struct Answer {
    thinking: Vec<(String, Kept)>,
    text: String,
    calls: Vec<(Option<String>, String, String)>,
    finish: Option<String>,
}

fn chat_answer(resp: &Value) -> Answer {
    let choice = resp.pointer("/choices/0");
    let msg = choice.and_then(|c| c.get("message"));
    let reasoning = msg.map(chat_reasoning).unwrap_or_default();
    let mut thinking = Vec::new();
    if !reasoning.is_empty() {
        thinking.push((reasoning, Kept::Content));
    }
    let mut text = content_text(msg.and_then(|m| m.get("content")));
    if let Some(refusal) = msg
        .and_then(|m| m.get("refusal"))
        .and_then(Value::as_str)
        .filter(|r| !r.is_empty())
    {
        text.push_str(refusal);
    }
    let calls = msg
        .and_then(|m| m.get("tool_calls"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|call| {
            let f = call.get("function");
            let field = |k: &str| {
                f.and_then(|f| f.get(k))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned()
            };
            (
                call.get("id").and_then(Value::as_str).map(str::to_owned),
                field("name"),
                field("arguments"),
            )
        })
        .collect();
    Answer {
        thinking,
        text,
        calls,
        finish: choice
            .and_then(|c| c.get("finish_reason"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

/// A Responses API answer: its `output` items, reasoning, messages and
/// function calls.
fn responses_answer(resp: &Value) -> Answer {
    let mut answer = Answer {
        finish: responses_finish(resp).map(str::to_owned),
        ..Answer::default()
    };
    for item in resp
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let field = |k: &str| item.get(k).and_then(Value::as_str);
        match field("type") {
            Some("message") => {
                for part in item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let text = part
                        .get("text")
                        .or_else(|| part.get("refusal"))
                        .and_then(Value::as_str);
                    answer.text.push_str(text.unwrap_or(""));
                }
            }
            Some("function_call") => answer.calls.push((
                field("call_id").or_else(|| field("id")).map(str::to_owned),
                field("name").unwrap_or("").to_owned(),
                field("arguments").unwrap_or("").to_owned(),
            )),
            Some("reasoning") => answer.thinking.extend(reasoning_item(item)),
            _ => {}
        }
    }
    answer
}

/// Why a Responses API answer stopped short, in Chat Completions' words:
/// `length` for the output budget, `content_filter`; `None` when complete.
pub(crate) fn responses_finish(resp: &Value) -> Option<&'static str> {
    if resp.get("status").and_then(Value::as_str) != Some("incomplete") {
        return None;
    }
    match resp
        .pointer("/incomplete_details/reason")
        .and_then(Value::as_str)
    {
        Some("content_filter") => Some("content_filter"),
        _ => Some("length"),
    }
}

/// The provider's whole answer as one Messages API message, as `model`
/// (what Claude Code asked for). Its reasoning comes as thinking blocks
/// unless `model` is one of Claude's names ([`crate::reasoning`]).
pub(crate) fn message(resp: &Value, api: Api, model: &str, names: &Names, estimate: u64) -> Value {
    let mut answer = match api {
        Api::ChatCompletions => chat_answer(resp),
        Api::Responses => responses_answer(resp),
    };
    let mut tags = ThinkTags::default();
    let mut pieces = tags.push(&answer.text);
    pieces.extend(tags.finish());
    let mut text = String::new();
    for p in pieces {
        match p {
            Piece::Text(t) => text.push_str(&t),
            Piece::Thinking(t) => answer.thinking.push((t.trim().to_owned(), Kept::Shown)),
        }
    }
    let mut text = text.trim_start().to_owned();
    if answer.finish.as_deref() == Some("content_filter") {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(FILTERED);
    }
    let mut content = Vec::new();
    if shows_thinking(model) {
        for (thought, kept) in &answer.thinking {
            content.push(
                json!({ "type": "thinking", "thinking": thought, "signature": kept.signature() }),
            );
        }
    }
    let output_estimate = (answer.text.len() as u64).div_ceil(4);
    if !text.trim().is_empty() {
        content.push(json!({ "type": "text", "text": text }));
    }
    let mut used = HashSet::new();
    for (id, name, args) in &answer.calls {
        let args = arguments(args);
        content.push(json!({
            "type": "tool_use",
            "id": tool_id(id.as_deref(), &mut used),
            "name": names.original(name),
            "input": serde_json::from_str::<Value>(&args).unwrap_or_else(|_| json!({})),
        }));
    }
    let called = !answer.calls.is_empty();
    json!({
        "id": message_id(resp.get("id").and_then(Value::as_str)),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason(answer.finish.as_deref(), called),
        "stop_sequence": null,
        "usage": usage(resp.get("usage"), estimate, output_estimate),
    })
}

/// Whether reasoning goes to Claude Code as thinking for answers labeled
/// `model`. Claude Code drops a thinking block once the model it talks to
/// isn't the one that wrote it; under one of Claude's own names, Berth's
/// thinking could reach Anthropic, which would refuse its signature.
pub(crate) fn shows_thinking(model: &str) -> bool {
    !model.trim().to_ascii_lowercase().starts_with("claude")
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
        let m = message(
            &resp,
            Api::ChatCompletions,
            "claude-opus-5-5",
            &Names::default(),
            0,
        );
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
    fn a_responses_api_answer_becomes_blocks() {
        let resp = json!({
            "id": "resp_1", "status": "incomplete",
            "incomplete_details": { "reason": "max_output_tokens" },
            "output": [
                { "type": "reasoning", "id": "rs_1", "summary": [] },
                { "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "Checking." }] },
                { "type": "function_call", "id": "fc_1", "call_id": "call_7", "name": "Bash", "arguments": "{\"command\":\"ls\"}" }
            ],
            "usage": { "input_tokens": 900, "input_tokens_details": { "cached_tokens": 100 }, "output_tokens": 12 }
        });
        let m = message(&resp, Api::Responses, "m", &Names::default(), 0);
        let c = m["content"].as_array().cloned().unwrap_or_default();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0]["text"], "Checking.");
        assert_eq!(c[1]["id"], "call_7");
        assert_eq!(c[1]["input"], json!({ "command": "ls" }));
        assert_eq!(m["stop_reason"], "max_tokens");
        assert_eq!(m["usage"]["input_tokens"], 800);
        assert_eq!(m["usage"]["cache_read_input_tokens"], 100);
        assert_eq!(m["usage"]["output_tokens"], 12);
        let done = json!({ "status": "completed", "output": [] });
        assert_eq!(responses_finish(&done), None);
    }

    #[test]
    fn stop_reasons_map() {
        assert_eq!(stop_reason(Some("stop"), false), "end_turn");
        assert_eq!(stop_reason(Some("stop"), true), "tool_use");
        assert_eq!(stop_reason(Some("length"), true), "max_tokens");
        assert_eq!(stop_reason(None, false), "end_turn");
        assert_eq!(stop_reason(Some("content_filter"), false), "end_turn");
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
