//! A streamed Chat Completions or Responses API answer as Messages API
//! events.
//!
//! Text streams as it comes, and so does the model's reasoning, as a
//! thinking block that closes with its signature once the model moves on
//! ([`crate::reasoning`]). Tool calls are held until the answer ends and
//! then sent whole, each its own block: providers split them differently
//! (the name late, several interleaved, all in one chunk), and Claude Code
//! runs a tool only once its call is complete anyway.

use std::collections::HashSet;

use serde_json::{Value, json};

use crate::Api;
use crate::errors::ApiError;
use crate::reasoning::Kept;
use crate::request::Names;
use crate::response::{
    FILTERED, Piece, ThinkTags, arguments, chat_reasoning, content_text, message_id,
    reasoning_item, responses_finish, shows_thinking, stop_reason, tool_id, usage,
};

/// One server-sent event for Claude Code.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Event(pub &'static str, pub Value);

impl Event {
    pub(crate) fn sse(&self) -> String {
        format!("event: {}\ndata: {}\n\n", self.0, self.1)
    }

    pub(crate) fn ping() -> Self {
        Self("ping", json!({ "type": "ping" }))
    }

    pub(crate) fn error(e: &ApiError) -> Self {
        Self("error", e.body())
    }
}

/// The provider's event stream, one `data:` line at a time. Servers that
/// are loose about blank lines between events still read right.
#[derive(Debug, Default)]
pub(crate) struct SseLines {
    buf: Vec<u8>,
}

impl SseLines {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let Some(last) = self.buf.iter().rposition(|&b| b == b'\n') else {
            return Vec::new();
        };
        let complete: Vec<u8> = self.buf.drain(..=last).collect();
        String::from_utf8_lossy(&complete)
            .lines()
            .filter_map(data)
            .collect()
    }

    /// What is left once the stream ends, without its last newline.
    pub(crate) fn finish(&mut self) -> Option<String> {
        let rest = std::mem::take(&mut self.buf);
        data(&String::from_utf8_lossy(&rest))
    }
}

fn data(line: &str) -> Option<String> {
    let d = line.trim_end_matches('\r').strip_prefix("data:")?;
    let d = d.strip_prefix(' ').unwrap_or(d);
    (!d.trim().is_empty()).then(|| d.to_owned())
}

#[derive(Debug, Default)]
struct Call {
    index: u64,
    id: Option<String>,
    name: String,
    args: String,
}

/// What an open thinking block's signature will say.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pending {
    /// A Responses API reasoning item: its signature comes when it's done.
    Item,
    Content,
    Shown,
}

/// The answer so far, and the events it has made.
#[derive(Debug)]
pub(crate) struct Translator {
    api: Api,
    id: String,
    model: String,
    names: Names,
    estimate: u64,
    /// Reasoning goes to Claude Code as thinking ([`shows_thinking`]).
    thinking_shown: bool,
    /// The open text block's index.
    text: Option<usize>,
    /// The open thinking block's index, and what it will be signed as.
    thinking: Option<(usize, Pending)>,
    next: usize,
    /// The answer's own text has begun: whitespace before it is dropped.
    spoke: bool,
    tags: ThinkTags,
    /// Responses API output indices whose text has come.
    texts: HashSet<u64>,
    /// The summary part a reasoning item's text is on.
    summary_part: Option<(u64, u64)>,
    calls: Vec<Call>,
    finish: Option<String>,
    usage: Option<Value>,
    output_chars: usize,
    done: bool,
}

impl Translator {
    /// Answers labeled `model`: what Claude Code asked for.
    pub(crate) fn new(api: Api, model: &str, names: Names, estimate: u64) -> Self {
        Self {
            api,
            id: message_id(None),
            model: model.to_owned(),
            names,
            estimate,
            thinking_shown: shows_thinking(model),
            text: None,
            thinking: None,
            next: 0,
            spoke: false,
            tags: ThinkTags::default(),
            texts: HashSet::new(),
            summary_part: None,
            calls: Vec::new(),
            finish: None,
            usage: None,
            output_chars: 0,
            done: false,
        }
    }

    /// `message_start`, before anything the provider sends. Its input count
    /// is the estimate; the provider's comes with `message_delta`.
    pub(crate) fn start(&self) -> Event {
        Event(
            "message_start",
            json!({ "type": "message_start", "message": {
                "id": self.id,
                "type": "message",
                "role": "assistant",
                "model": self.model,
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {
                    "input_tokens": self.estimate,
                    "output_tokens": 0,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0,
                },
            }}),
        )
    }

    /// Whether the provider said it was done (`[DONE]`, `response.completed`).
    pub(crate) fn done(&self) -> bool {
        self.done
    }

    /// Whether the stream ended without an answer's end: the connection
    /// dropped. Claude Code tries again on the error this becomes.
    pub(crate) fn ended_early(&self) -> bool {
        !self.done && self.finish.is_none()
    }

    /// One `data:` payload from the provider. An error it reports mid-way
    /// ends the answer.
    pub(crate) fn data(&mut self, data: &str) -> Result<Vec<Event>, ApiError> {
        if data.trim() == "[DONE]" {
            self.done = true;
            return Ok(Vec::new());
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            return Ok(Vec::new());
        };
        if self.api == Api::Responses {
            return self.responses_event(&chunk);
        }
        if let Some(err) = chunk.get("error") {
            return Err(ApiError::in_stream(err));
        }
        if let Some(u) = chunk.get("usage").filter(|u| u.is_object()) {
            self.usage = Some(u.clone());
        }
        let mut out = Vec::new();
        // Azure's first chunk has no choices: its prompt's filter results.
        let Some(choice) = chunk.pointer("/choices/0") else {
            return Ok(out);
        };
        let delta = choice.get("delta").or_else(|| choice.get("message"));
        if let Some(delta) = delta {
            let reasoning = chat_reasoning(delta);
            if !reasoning.is_empty() {
                out.extend(self.thinking_delta(&reasoning, Pending::Content));
            }
            let text = content_text(delta.get("content"));
            if !text.is_empty() {
                out.extend(self.answer_text(&text));
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for c in calls {
                    self.call_delta(c);
                }
            }
        }
        if let Some(f) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish = Some(f.to_owned());
        }
        Ok(out)
    }

    /// One Responses API event: text and reasoning summaries stream,
    /// function calls gather by their output index, and `response.completed`
    /// (or `.incomplete`) ends the answer with its counts. Text a server
    /// sends only whole (in `.done` events, or the completed response) still
    /// comes through, once.
    fn responses_event(&mut self, ev: &Value) -> Result<Vec<Event>, ApiError> {
        let field = |k: &str| ev.get(k).and_then(Value::as_str);
        let index = ev.get("output_index").and_then(Value::as_u64);
        let mut out = Vec::new();
        match field("type").unwrap_or("") {
            "response.output_text.delta" | "response.refusal.delta" => {
                let delta = field("delta").unwrap_or("");
                if !delta.is_empty() {
                    self.texts.extend(index);
                    out.extend(self.answer_text(delta));
                }
            }
            "response.output_text.done" => {
                if let Some(text) = field("text")
                    && index.is_none_or(|i| !self.texts.contains(&i))
                {
                    self.texts.extend(index);
                    out.extend(self.answer_text(text));
                }
            }
            "response.reasoning_summary_text.delta" => {
                let part = ev.get("summary_index").and_then(Value::as_u64).unwrap_or(0);
                let at = (index.unwrap_or(0), part);
                let mut delta = field("delta").unwrap_or("").to_owned();
                if self.summary_part.is_some_and(|p| p != at) {
                    delta.insert_str(0, "\n\n");
                }
                self.summary_part = Some(at);
                out.extend(self.thinking_delta(&delta, Pending::Item));
            }
            kind @ ("response.output_item.added" | "response.output_item.done") => {
                let item = ev.get("item").unwrap_or(&Value::Null);
                let item_field = |k: &str| item.get(k).and_then(Value::as_str);
                match item_field("type") {
                    Some("function_call") => {
                        let next = self.calls.len() as u64;
                        let call = self.call_at(index.unwrap_or(next));
                        if call.id.is_none() {
                            call.id = item_field("call_id").map(str::to_owned);
                        }
                        if call.name.is_empty() {
                            call.name = item_field("name").unwrap_or("").to_owned();
                        }
                        // The finished item holds the whole arguments.
                        if let Some(args) = item_field("arguments").filter(|a| !a.is_empty())
                            && kind == "response.output_item.done"
                        {
                            call.args = args.to_owned();
                        }
                    }
                    Some("reasoning") if kind == "response.output_item.done" => {
                        out.extend(self.reasoning_done(item));
                    }
                    Some("message")
                        if kind == "response.output_item.done"
                            && index.is_none_or(|i| !self.texts.contains(&i)) =>
                    {
                        self.texts.extend(index);
                        let text = content_text(item.get("content"));
                        if !text.is_empty() {
                            out.extend(self.answer_text(&text));
                        }
                    }
                    _ => {}
                }
            }
            "response.function_call_arguments.delta" => {
                let next = self.calls.len().saturating_sub(1) as u64;
                let delta = field("delta").unwrap_or("");
                self.output_chars += delta.len();
                self.call_at(index.unwrap_or(next)).args.push_str(delta);
            }
            "response.function_call_arguments.done" => {
                if let Some(args) = field("arguments") {
                    let next = self.calls.len().saturating_sub(1) as u64;
                    self.call_at(index.unwrap_or(next)).args = args.to_owned();
                }
            }
            "response.completed" | "response.incomplete" => {
                let resp = ev.get("response").unwrap_or(&Value::Null);
                if let Some(u) = resp.get("usage").filter(|u| u.is_object()) {
                    self.usage = Some(u.clone());
                }
                // A server that sent nothing on the way: the whole output.
                if self.next == 0 && self.calls.is_empty() && self.thinking.is_none() {
                    out.extend(self.whole_output(resp));
                }
                self.finish = Some(responses_finish(resp).unwrap_or("stop").to_owned());
                self.done = true;
            }
            "response.failed" => {
                let err = ev.pointer("/response/error").unwrap_or(ev);
                return Err(ApiError::in_stream(err));
            }
            "error" => return Err(ApiError::in_stream(ev)),
            _ => {}
        }
        Ok(out)
    }

    /// A completed response's output, for a server that streamed none of it.
    fn whole_output(&mut self, resp: &Value) -> Vec<Event> {
        let mut out = Vec::new();
        for (i, item) in resp
            .get("output")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            match item.get("type").and_then(Value::as_str) {
                Some("reasoning") => out.extend(self.reasoning_done(item)),
                Some("message") => {
                    let text = content_text(item.get("content"));
                    if !text.is_empty() {
                        out.extend(self.answer_text(&text));
                    }
                }
                Some("function_call") => {
                    let field = |k: &str| item.get(k).and_then(Value::as_str);
                    let call = self.call_at(i as u64);
                    call.id = field("call_id").or_else(|| field("id")).map(str::to_owned);
                    call.name = field("name").unwrap_or("").to_owned();
                    call.args = field("arguments").unwrap_or("").to_owned();
                }
                _ => {}
            }
        }
        out
    }

    /// A Responses API reasoning item is done: the thinking block its
    /// summary streamed into closes with its signature, or, when no summary
    /// came, one opens and closes for it.
    fn reasoning_done(&mut self, item: &Value) -> Vec<Event> {
        self.summary_part = None;
        let Some((summary, kept)) = reasoning_item(item) else {
            return self.close_thinking(None);
        };
        let mut out = Vec::new();
        if !matches!(self.thinking, Some((_, Pending::Item))) {
            out.extend(self.thinking_delta(&summary, Pending::Item));
        }
        out.extend(self.close_thinking(Some(kept)));
        out
    }

    /// The call at output `index`, new if it isn't there yet.
    fn call_at(&mut self, index: u64) -> &mut Call {
        let at = match self.calls.iter().position(|k| k.index == index) {
            Some(at) => at,
            None => {
                self.calls.push(Call {
                    index,
                    ..Call::default()
                });
                self.calls.len() - 1
            }
        };
        &mut self.calls[at]
    }

    /// The answer's text as it comes: `<think>` reasoning split off, the
    /// whitespace before the first words dropped.
    fn answer_text(&mut self, text: &str) -> Vec<Event> {
        let pieces = self.tags.push(text);
        let mut out = Vec::new();
        for p in pieces {
            match p {
                Piece::Thinking(t) => out.extend(self.thinking_delta(&t, Pending::Shown)),
                Piece::Text(t) => out.extend(self.text_delta(&t)),
            }
        }
        out
    }

    fn text_delta(&mut self, text: &str) -> Vec<Event> {
        let text = if self.spoke { text } else { text.trim_start() };
        if text.is_empty() {
            return Vec::new();
        }
        self.spoke = true;
        let mut out = self.close_thinking(None);
        let index = match self.text {
            Some(i) => i,
            None => {
                let i = self.next;
                self.next += 1;
                self.text = Some(i);
                out.push(Event(
                    "content_block_start",
                    json!({ "type": "content_block_start", "index": i,
                            "content_block": { "type": "text", "text": "" } }),
                ));
                i
            }
        };
        self.output_chars += text.len();
        out.push(Event(
            "content_block_delta",
            json!({ "type": "content_block_delta", "index": index,
                    "delta": { "type": "text_delta", "text": text } }),
        ));
        out
    }

    /// Reasoning as it comes, into the open thinking block (one opens after
    /// whatever was open).
    fn thinking_delta(&mut self, text: &str, pending: Pending) -> Vec<Event> {
        let opens = self.thinking.as_ref().is_none_or(|(_, p)| *p != pending);
        let text = if opens { text.trim_start() } else { text };
        if !self.thinking_shown || text.is_empty() {
            return Vec::new();
        }
        self.output_chars += text.len();
        let mut out = Vec::new();
        if self.thinking.as_ref().is_some_and(|(_, p)| *p != pending) {
            out.extend(self.close_thinking(None));
        }
        if let Some(i) = self.text.take() {
            out.push(stop(i));
        }
        let index = match &self.thinking {
            Some((i, _)) => *i,
            None => {
                let i = self.next;
                self.next += 1;
                self.thinking = Some((i, pending));
                out.push(Event(
                    "content_block_start",
                    json!({ "type": "content_block_start", "index": i,
                            "content_block": { "type": "thinking", "thinking": "", "signature": "" } }),
                ));
                i
            }
        };
        out.push(Event(
            "content_block_delta",
            json!({ "type": "content_block_delta", "index": index,
                    "delta": { "type": "thinking_delta", "thinking": text } }),
        ));
        out
    }

    /// Close the open thinking block, signed as `kept` says (a reasoning
    /// item's), else as what it holds.
    fn close_thinking(&mut self, kept: Option<Kept>) -> Vec<Event> {
        let Some((i, pending)) = self.thinking.take() else {
            return Vec::new();
        };
        let kept = kept.unwrap_or(match pending {
            Pending::Content => Kept::Content,
            Pending::Item | Pending::Shown => Kept::Shown,
        });
        vec![
            Event(
                "content_block_delta",
                json!({ "type": "content_block_delta", "index": i,
                        "delta": { "type": "signature_delta", "signature": kept.signature() } }),
            ),
            stop(i),
        ]
    }

    fn call_delta(&mut self, c: &Value) {
        let id = c
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let index = match c.get("index").and_then(Value::as_u64) {
            Some(i) => i,
            // No index: a new call when it brings a new id, else the last one.
            None => match (id, self.calls.last()) {
                (Some(id), Some(last)) if last.id.as_deref() != Some(id) => last.index + 1,
                (_, Some(last)) => last.index,
                (_, None) => 0,
            },
        };
        let call = self.call_at(index);
        if call.id.is_none() {
            call.id = id.map(str::to_owned);
        }
        let f = c.get("function");
        if let Some(name) = f.and_then(|f| f.get("name")).and_then(Value::as_str)
            && call.name.is_empty()
        {
            call.name = name.to_owned();
        }
        if let Some(args) = f.and_then(|f| f.get("arguments")) {
            match args {
                Value::String(s) => call.args.push_str(s),
                // Some servers send the arguments as an object.
                other if other.is_object() => call.args = other.to_string(),
                _ => {}
            }
        }
        self.output_chars += c.to_string().len() / 2;
    }

    /// The end: what `<think>` still held, the open blocks close, the tool
    /// calls go out, then the stop reason and the provider's counts.
    pub(crate) fn finish(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        match self.tags.finish() {
            Some(Piece::Text(t)) => out.extend(self.text_delta(&t)),
            Some(Piece::Thinking(t)) => out.extend(self.thinking_delta(&t, Pending::Shown)),
            None => {}
        }
        if self.finish.as_deref() == Some("content_filter") {
            let note = if self.spoke {
                format!("\n\n{FILTERED}")
            } else {
                FILTERED.to_owned()
            };
            out.extend(self.text_delta(&note));
        }
        out.extend(self.close_thinking(None));
        if let Some(i) = self.text.take() {
            out.push(stop(i));
        }
        let calls: Vec<Call> = std::mem::take(&mut self.calls)
            .into_iter()
            .filter(|c| !c.name.is_empty())
            .collect();
        let called = !calls.is_empty();
        let mut used = HashSet::new();
        for c in calls {
            let i = self.next;
            self.next += 1;
            out.push(Event(
                "content_block_start",
                json!({ "type": "content_block_start", "index": i, "content_block": {
                    "type": "tool_use",
                    "id": tool_id(c.id.as_deref(), &mut used),
                    "name": self.names.original(&c.name),
                    "input": {},
                }}),
            ));
            out.push(Event(
                "content_block_delta",
                json!({ "type": "content_block_delta", "index": i,
                        "delta": { "type": "input_json_delta", "partial_json": arguments(&c.args) } }),
            ));
            out.push(stop(i));
        }
        let output_estimate = (self.output_chars as u64).div_ceil(4);
        out.push(Event(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": stop_reason(self.finish.as_deref(), called), "stop_sequence": null },
                "usage": usage(self.usage.as_ref(), self.estimate, output_estimate),
            }),
        ));
        out.push(Event("message_stop", json!({ "type": "message_stop" })));
        out
    }
}

fn stop(index: usize) -> Event {
    Event(
        "content_block_stop",
        json!({ "type": "content_block_stop", "index": index }),
    )
}

/// The events for a whole message: for a provider that answered a
/// streamed request all at once.
pub(crate) fn events_for(message: &Value) -> Vec<Event> {
    let mut start = message.clone();
    if let Some(o) = start.as_object_mut() {
        o.insert("content".into(), json!([]));
        o.insert("stop_reason".into(), Value::Null);
    }
    let mut out = vec![Event(
        "message_start",
        json!({ "type": "message_start", "message": start }),
    )];
    for (i, block) in message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        if block["type"] == "tool_use" {
            let mut empty = block.clone();
            empty["input"] = json!({});
            out.push(Event(
                "content_block_start",
                json!({ "type": "content_block_start", "index": i, "content_block": empty }),
            ));
            out.push(Event(
                "content_block_delta",
                json!({ "type": "content_block_delta", "index": i,
                        "delta": { "type": "input_json_delta", "partial_json": block["input"].to_string() } }),
            ));
        } else if block["type"] == "thinking" {
            out.push(Event(
                "content_block_start",
                json!({ "type": "content_block_start", "index": i,
                        "content_block": { "type": "thinking", "thinking": "", "signature": "" } }),
            ));
            out.push(Event(
                "content_block_delta",
                json!({ "type": "content_block_delta", "index": i,
                        "delta": { "type": "thinking_delta", "thinking": block["thinking"] } }),
            ));
            out.push(Event(
                "content_block_delta",
                json!({ "type": "content_block_delta", "index": i,
                        "delta": { "type": "signature_delta", "signature": block["signature"] } }),
            ));
        } else {
            out.push(Event(
                "content_block_start",
                json!({ "type": "content_block_start", "index": i,
                        "content_block": { "type": "text", "text": "" } }),
            ));
            out.push(Event(
                "content_block_delta",
                json!({ "type": "content_block_delta", "index": i,
                        "delta": { "type": "text_delta", "text": block["text"] } }),
            ));
        }
        out.push(stop(i));
    }
    out.push(Event(
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": { "stop_reason": message["stop_reason"], "stop_sequence": null },
            "usage": message["usage"],
        }),
    ));
    out.push(Event("message_stop", json!({ "type": "message_stop" })));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reasoning::Kept;

    fn names(events: &[Event]) -> Vec<&'static str> {
        events.iter().map(|e| e.0).collect()
    }

    fn feed(t: &mut Translator, lines: &[&str]) -> Vec<Event> {
        let mut sse = SseLines::default();
        let mut out = Vec::new();
        let text: String = lines.iter().map(|l| format!("data: {l}\n\n")).collect();
        // Split mid-line, as the network does.
        let (a, b) = text.split_at(text.len() / 2);
        for part in [a, b] {
            for d in sse.push(part.as_bytes()) {
                out.extend(t.data(&d).unwrap_or_default());
            }
        }
        out
    }

    #[test]
    fn text_streams_and_tool_calls_come_whole_at_the_end() {
        let mut t = Translator::new(
            Api::ChatCompletions,
            "claude-opus-5-5",
            Names::default(),
            500,
        );
        assert_eq!(t.start().1["message"]["usage"]["input_tokens"], 500);
        let mut events = feed(
            &mut t,
            &[
                r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}"#,
                r#"{"choices":[{"index":0,"delta":{"reasoning_content":"hmm"}}]}"#,
                r#"{"choices":[{"index":0,"delta":{"content":"Let me "}}]}"#,
                r#"{"choices":[{"index":0,"delta":{"content":"look."}}]}"#,
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"Bash","arguments":""}}]}}]}"#,
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_2","function":{"name":"Read","arguments":"{\"file_path\":"}}]}}]}"#,
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"command\":\"ls\"}"}}]}}]}"#,
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"\"a.txt\"}"}}]}}]}"#,
                r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
                r#"{"choices":[],"usage":{"prompt_tokens":620,"completion_tokens":31}}"#,
                "[DONE]",
            ],
        );
        assert!(t.done() && !t.ended_early());
        events.extend(t.finish());
        assert_eq!(
            names(&events),
            [
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(events[2].1["delta"]["text"], "look.");
        assert_eq!(events[4].1["content_block"]["name"], "Bash");
        assert_eq!(events[4].1["index"], 1);
        assert_eq!(events[5].1["delta"]["partial_json"], r#"{"command":"ls"}"#);
        assert_eq!(
            events[8].1["delta"]["partial_json"],
            r#"{"file_path":"a.txt"}"#
        );
        assert_eq!(events[10].1["delta"]["stop_reason"], "tool_use");
        assert_eq!(events[10].1["usage"]["input_tokens"], 620);
        assert_eq!(events[10].1["usage"]["output_tokens"], 31);
    }

    #[test]
    fn a_responses_api_stream_comes_back_as_the_same_events() {
        let mut t = Translator::new(Api::Responses, "m", Names::default(), 0);
        let mut events = feed(
            &mut t,
            &[
                r#"{"type":"response.created","response":{"id":"resp_1","status":"in_progress","error":null}}"#,
                r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs_1"}}"#,
                r#"{"type":"response.output_item.added","output_index":1,"item":{"type":"message","id":"msg_1","content":[]}}"#,
                r#"{"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"Let me "}"#,
                r#"{"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"look."}"#,
                r#"{"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"Bash","arguments":""}}"#,
                r#"{"type":"response.function_call_arguments.delta","output_index":2,"delta":"{\"command\":"}"#,
                r#"{"type":"response.function_call_arguments.delta","output_index":2,"delta":"\"ls\"}"}"#,
                r#"{"type":"response.output_item.done","output_index":2,"item":{"type":"function_call","call_id":"call_1","name":"Bash","arguments":"{\"command\":\"ls\"}"}}"#,
                r#"{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":700,"output_tokens":20}}}"#,
            ],
        );
        assert!(t.done() && !t.ended_early());
        events.extend(t.finish());
        let kinds: Vec<&str> = events.iter().map(|e| e.0).collect();
        assert_eq!(
            kinds,
            [
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(events[4].1["content_block"]["id"], "call_1");
        assert_eq!(events[5].1["delta"]["partial_json"], r#"{"command":"ls"}"#);
        assert_eq!(events[7].1["delta"]["stop_reason"], "tool_use");
        assert_eq!(events[7].1["usage"]["input_tokens"], 700);

        // Out of output budget; and a failure mid-way.
        let mut t = Translator::new(Api::Responses, "m", Names::default(), 0);
        feed(
            &mut t,
            &[
                r#"{"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}}"#,
            ],
        );
        assert_eq!(t.finish()[0].1["delta"]["stop_reason"], "max_tokens");
        let mut t = Translator::new(Api::Responses, "m", Names::default(), 0);
        let failed = t.data(r#"{"type":"response.failed","response":{"error":{"code":"server_error","message":"boom"}}}"#);
        assert!(failed.is_err());
    }

    #[test]
    fn a_whole_call_in_one_chunk_without_an_index() {
        let mut t = Translator::new(Api::ChatCompletions, "m", Names::default(), 0);
        feed(
            &mut t,
            &[
                r#"{"choices":[{"delta":{"tool_calls":[{"id":"call_a1","function":{"name":"Bash","arguments":{"command":"ls"}}}]}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"id":"call_b2","function":{"name":"Read","arguments":"{}"}}]},"finish_reason":"stop"}]}"#,
            ],
        );
        let events = t.finish();
        let starts: Vec<&Value> = events
            .iter()
            .filter(|e| e.0 == "content_block_start")
            .map(|e| &e.1["content_block"]["id"])
            .collect();
        assert_eq!(starts, [&json!("call_a1"), &json!("call_b2")]);
        assert_eq!(events[1].1["delta"]["partial_json"], r#"{"command":"ls"}"#);
        // `stop` after tool calls is still a tool use.
        assert_eq!(events[6].1["delta"]["stop_reason"], "tool_use");
    }

    #[test]
    fn ids_a_server_repeats_or_numbers_are_made_unique() {
        let mut t = Translator::new(Api::ChatCompletions, "m", Names::default(), 0);
        feed(
            &mut t,
            &[
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"0","function":{"name":"Bash","arguments":"{}"}},{"index":1,"id":"call_same","function":{"name":"Read","arguments":"{}"}},{"index":2,"id":"call_same","function":{"name":"Grep","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
            ],
        );
        let ids: Vec<String> = t
            .finish()
            .iter()
            .filter(|e| e.0 == "content_block_start")
            .filter_map(|e| e.1["content_block"]["id"].as_str().map(str::to_owned))
            .collect();
        assert_eq!(ids.len(), 3);
        assert!(ids[0].starts_with("toolu_"));
        assert_eq!(ids[1], "call_same");
        assert!(ids[2].starts_with("toolu_"));
    }

    /// The events' blocks, as Claude Code assembles them: (type, text or
    /// thinking, signature).
    fn blocks(events: &[Event]) -> Vec<(String, String, String)> {
        let mut out: Vec<(String, String, String)> = Vec::new();
        for e in events {
            match e.0 {
                "content_block_start" => out.push((
                    e.1["content_block"]["type"]
                        .as_str()
                        .unwrap_or("")
                        .to_owned(),
                    String::new(),
                    String::new(),
                )),
                "content_block_delta" => {
                    let d = &e.1["delta"];
                    if let Some(last) = out.last_mut() {
                        for k in ["text", "thinking", "partial_json"] {
                            last.1.push_str(d[k].as_str().unwrap_or(""));
                        }
                        last.2.push_str(d["signature"].as_str().unwrap_or(""));
                    }
                }
                _ => {}
            }
        }
        out
    }

    #[test]
    fn reasoning_streams_as_thinking_signed_for_going_back() {
        let mut t = Translator::new(Api::Responses, "gpt-5", Names::default(), 0);
        let mut events = feed(
            &mut t,
            &[
                r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs_1","summary":[]}}"#,
                r#"{"type":"response.reasoning_summary_text.delta","output_index":0,"summary_index":0,"delta":"**Plan**"}"#,
                r#"{"type":"response.reasoning_summary_text.delta","output_index":0,"summary_index":1,"delta":"Then run."}"#,
                r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"**Plan**"},{"type":"summary_text","text":"Then run."}],"encrypted_content":"gAAA"}}"#,
                r#"{"type":"response.output_item.added","output_index":1,"item":{"type":"message","content":[]}}"#,
                r#"{"type":"response.output_text.delta","output_index":1,"delta":"\n\nRunning it."}"#,
                r#"{"type":"response.output_text.done","output_index":1,"text":"\n\nRunning it."}"#,
                r#"{"type":"response.output_item.done","output_index":1,"item":{"type":"message","content":[{"type":"output_text","text":"\n\nRunning it."}]}}"#,
                r#"{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":5,"output_tokens":5}}}"#,
            ],
        );
        events.extend(t.finish());
        let b = blocks(&events);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].0, "thinking");
        assert_eq!(b[0].1, "**Plan**\n\nThen run.");
        assert_eq!(
            Kept::read(&b[0].2),
            Some(Kept::Item {
                id: "rs_1".into(),
                encrypted: "gAAA".into()
            })
        );
        // The text once, without the blank lines it began with.
        assert_eq!(b[1], ("text".into(), "Running it.".into(), String::new()));

        // Under one of Claude's names, no thinking at all.
        let mut t = Translator::new(Api::Responses, "claude-opus-5-5", Names::default(), 0);
        let events = feed(
            &mut t,
            &[r#"{"type":"response.reasoning_summary_text.delta","output_index":0,"delta":"x"}"#],
        );
        assert!(events.is_empty());
    }

    #[test]
    fn think_tags_and_reasoning_content_become_thinking() {
        let mut t = Translator::new(Api::ChatCompletions, "DeepSeek-R1", Names::default(), 0);
        let mut events = feed(
            &mut t,
            &[
                r#"{"choices":[],"prompt_filter_results":[{"prompt_index":0}]}"#,
                r#"{"choices":[{"delta":{"content":"<thi"}}]}"#,
                r#"{"choices":[{"delta":{"content":"nk>\nHmm, ls.</th"}}]}"#,
                r#"{"choices":[{"delta":{"content":"ink>\n\nOn it."}}]}"#,
                r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
                "[DONE]",
            ],
        );
        events.extend(t.finish());
        let b = blocks(&events);
        assert_eq!(b[0].0, "thinking");
        assert_eq!(b[0].1, "Hmm, ls.");
        assert_eq!(Kept::read(&b[0].2), Some(Kept::Shown));
        assert_eq!(b[1], ("text".into(), "On it.".into(), String::new()));

        let mut t = Translator::new(
            Api::ChatCompletions,
            "deepseek-reasoner",
            Names::default(),
            0,
        );
        let mut events = feed(
            &mut t,
            &[
                r#"{"choices":[{"delta":{"reasoning_content":"Look first."}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_x1","function":{"name":"Bash","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
                "[DONE]",
            ],
        );
        events.extend(t.finish());
        let b = blocks(&events);
        assert_eq!(b[0].1, "Look first.");
        assert_eq!(Kept::read(&b[0].2), Some(Kept::Content));
        assert_eq!(b[1].0, "tool_use");
    }

    #[test]
    fn a_server_that_streams_nothing_but_the_end_still_answers() {
        let mut t = Translator::new(Api::Responses, "m", Names::default(), 0);
        let mut events = feed(
            &mut t,
            &[
                r#"{"type":"response.completed","response":{"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"Hi."}]},{"type":"function_call","call_id":"call_z9","name":"Bash","arguments":"{}"}]}}"#,
            ],
        );
        events.extend(t.finish());
        let b = blocks(&events);
        assert_eq!(b[0], ("text".into(), "Hi.".into(), String::new()));
        assert_eq!(b[1].0, "tool_use");
    }

    #[test]
    fn errors_mid_stream_and_dropped_connections_are_told_apart() {
        let mut t = Translator::new(Api::ChatCompletions, "m", Names::default(), 0);
        let err = t.data(r#"{"error":{"message":"Provider returned error","code":502}}"#);
        assert!(err.is_err());
        let mut t = Translator::new(Api::ChatCompletions, "m", Names::default(), 0);
        feed(&mut t, &[r#"{"choices":[{"delta":{"content":"par"}}]}"#]);
        assert!(t.ended_early());
    }

    #[test]
    fn comments_and_carriage_returns_are_skipped() {
        let mut sse = SseLines::default();
        let got = sse.push(b": OPENROUTER PROCESSING\r\n\r\ndata: {\"a\":1}\r\n\r\ndata: [DONE]");
        assert_eq!(got, [r#"{"a":1}"#]);
        assert_eq!(sse.finish().as_deref(), Some("[DONE]"));
    }

    #[test]
    fn a_whole_message_streams_as_events() {
        let m = json!({ "id": "msg_1", "type": "message", "role": "assistant", "model": "m",
            "content": [{ "type": "text", "text": "hi" }, { "type": "tool_use", "id": "t", "name": "Bash", "input": { "command": "ls" } }],
            "stop_reason": "tool_use", "stop_sequence": null, "usage": { "input_tokens": 3, "output_tokens": 1 } });
        let events = events_for(&m);
        assert_eq!(events[0].1["message"]["content"], json!([]));
        assert_eq!(events[5].1["delta"]["partial_json"], r#"{"command":"ls"}"#);
        assert_eq!(events[7].1["delta"]["stop_reason"], "tool_use");
    }
}
