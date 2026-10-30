//! A streamed Chat Completions answer as Messages API events.
//!
//! Text streams as it comes. Tool calls are held until the answer ends and
//! then sent whole, each its own block: providers split them differently
//! (the name late, several interleaved, all in one chunk), and Claude Code
//! runs a tool only once its call is complete anyway.

use serde_json::{Value, json};

use crate::Api;
use crate::errors::ApiError;
use crate::request::Names;
use crate::response::{
    arguments, content_text, message_id, responses_finish, stop_reason, tool_id, usage,
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

/// The answer so far, and the events it has made.
#[derive(Debug)]
pub(crate) struct Translator {
    api: Api,
    id: String,
    model: String,
    names: Names,
    estimate: u64,
    /// The open text block's index.
    text: Option<usize>,
    next: usize,
    calls: Vec<Call>,
    finish: Option<String>,
    usage: Option<Value>,
    output_chars: usize,
    done: bool,
}

impl Translator {
    pub(crate) fn new(api: Api, model: &str, names: Names, estimate: u64) -> Self {
        Self {
            api,
            id: message_id(None),
            model: model.to_owned(),
            names,
            estimate,
            text: None,
            next: 0,
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
        let Some(choice) = chunk.pointer("/choices/0") else {
            return Ok(out);
        };
        let delta = choice.get("delta").or_else(|| choice.get("message"));
        if let Some(delta) = delta {
            let text = content_text(delta.get("content"));
            if !text.is_empty() {
                out.extend(self.text_delta(&text));
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

    /// One Responses API event: text deltas stream, function calls gather
    /// by their output index, and `response.completed` (or `.incomplete`)
    /// ends the answer with its counts.
    fn responses_event(&mut self, ev: &Value) -> Result<Vec<Event>, ApiError> {
        let field = |k: &str| ev.get(k).and_then(Value::as_str);
        let index = ev.get("output_index").and_then(Value::as_u64);
        match field("type").unwrap_or("") {
            "response.output_text.delta" | "response.refusal.delta" => {
                let delta = field("delta").unwrap_or("");
                if !delta.is_empty() {
                    return Ok(self.text_delta(delta));
                }
            }
            kind @ ("response.output_item.added" | "response.output_item.done") => {
                let item = ev.get("item").unwrap_or(&Value::Null);
                let item_field = |k: &str| item.get(k).and_then(Value::as_str);
                if item_field("type") == Some("function_call") {
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
        Ok(Vec::new())
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

    fn text_delta(&mut self, text: &str) -> Vec<Event> {
        let mut out = Vec::new();
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

    /// The end: the open text block closes, the tool calls go out, then the
    /// stop reason and the provider's counts.
    pub(crate) fn finish(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        if let Some(i) = self.text.take() {
            out.push(stop(i));
        }
        let calls: Vec<Call> = std::mem::take(&mut self.calls)
            .into_iter()
            .filter(|c| !c.name.is_empty())
            .collect();
        let called = !calls.is_empty();
        for c in calls {
            let i = self.next;
            self.next += 1;
            out.push(Event(
                "content_block_start",
                json!({ "type": "content_block_start", "index": i, "content_block": {
                    "type": "tool_use",
                    "id": tool_id(c.id.as_deref()),
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
                r#"{"choices":[{"delta":{"tool_calls":[{"id":"a","function":{"name":"Bash","arguments":{"command":"ls"}}}]}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"id":"b","function":{"name":"Read","arguments":"{}"}}]},"finish_reason":"stop"}]}"#,
            ],
        );
        let events = t.finish();
        let starts: Vec<&Value> = events
            .iter()
            .filter(|e| e.0 == "content_block_start")
            .map(|e| &e.1["content_block"]["id"])
            .collect();
        assert_eq!(starts, [&json!("a"), &json!("b")]);
        assert_eq!(events[1].1["delta"]["partial_json"], r#"{"command":"ls"}"#);
        // `stop` after tool calls is still a tool use.
        assert_eq!(events[6].1["delta"]["stop_reason"], "tool_use");
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
