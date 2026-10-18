//! "Show details": turn a node's raw SDK events into readable transcript entries.

use serde_json::Value;

use crate::model::RawEventRow;

/// Longest tool result body shown before truncation.
const MAX_BODY: usize = 4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    AssistantText,
    Thinking,
    ToolUse,
    ToolResult,
    User,
    System,
    Result,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEntry {
    pub kind: EntryKind,
    pub title: String,
    pub body: String,
    pub is_error: bool,
}

/// Render raw events in order. Unknown shapes are skipped, never fatal.
#[must_use]
pub fn render(events: &[RawEventRow]) -> Vec<TranscriptEntry> {
    let mut out = Vec::new();
    for e in events {
        let Ok(json) = serde_json::from_str::<Value>(&e.json) else {
            continue;
        };
        match e.kind.as_str() {
            "assistant" => assistant(&json, &mut out),
            "user" => user(&json, &mut out),
            "system" => {
                // Only system events a reader cares about; hooks, token counters
                // and status pings are noise.
                let subtype = json
                    .get("subtype")
                    .and_then(Value::as_str)
                    .unwrap_or("system");
                let (title, body) = match subtype {
                    "init" => (
                        "Session started",
                        format!(
                            "model {} · cwd {}",
                            str_at(&json, "/model"),
                            str_at(&json, "/cwd")
                        ),
                    ),
                    "task_notification" => (
                        "Background agent finished",
                        str_at(&json, "/summary").to_owned(),
                    ),
                    "compact_boundary" => ("Context compacted", String::new()),
                    _ => continue,
                };
                out.push(entry(EntryKind::System, title, body, false));
            }
            "result" => {
                let subtype = json
                    .get("subtype")
                    .and_then(Value::as_str)
                    .unwrap_or("result");
                let cost = json
                    .get("total_cost_usd")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                let ms = json.get("duration_ms").and_then(Value::as_u64).unwrap_or(0);
                let is_error = json
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                out.push(entry(
                    EntryKind::Result,
                    &format!("Turn ended ({subtype})"),
                    format!("{:.1}s · ${cost:.4} total", ms as f64 / 1000.0),
                    is_error,
                ));
            }
            _ => {}
        }
    }
    out
}

fn assistant(json: &Value, out: &mut Vec<TranscriptEntry>) {
    let Some(blocks) = json.pointer("/message/content").and_then(Value::as_array) else {
        return;
    };
    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = b.get("text").and_then(Value::as_str).unwrap_or_default();
                if !text.trim().is_empty() {
                    out.push(entry(
                        EntryKind::AssistantText,
                        "Assistant",
                        text.trim().to_owned(),
                        false,
                    ));
                }
            }
            Some("thinking") => {
                let text = b
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !text.trim().is_empty() {
                    out.push(entry(
                        EntryKind::Thinking,
                        "Thinking",
                        text.trim().to_owned(),
                        false,
                    ));
                }
            }
            Some("tool_use") => {
                let name = b.get("name").and_then(Value::as_str).unwrap_or("tool");
                let name = name.strip_prefix("mcp__berth__").unwrap_or(name);
                let input = b.get("input").map(pretty_input).unwrap_or_default();
                out.push(entry(EntryKind::ToolUse, name, input, false));
            }
            _ => {}
        }
    }
}

fn user(json: &Value, out: &mut Vec<TranscriptEntry>) {
    match json.pointer("/message/content") {
        Some(Value::String(s)) => out.push(entry(EntryKind::User, "User", s.clone(), false)),
        Some(Value::Array(blocks)) => {
            for b in blocks {
                match b.get("type").and_then(Value::as_str) {
                    Some("tool_result") => {
                        let is_error = b.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                        let body = content_text(b.get("content"));
                        out.push(entry(
                            EntryKind::ToolResult,
                            "Result",
                            truncate(&body),
                            is_error,
                        ));
                    }
                    Some("text") => {
                        let text = b.get("text").and_then(Value::as_str).unwrap_or_default();
                        out.push(entry(EntryKind::User, "User", text.to_owned(), false));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// Inputs read best as `key: value` lines; long string values keep their newlines.
fn pretty_input(input: &Value) -> String {
    match input {
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| match v {
                Value::String(s) => format!("{k}: {}", truncate(s)),
                other => format!("{k}: {other}"),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn truncate(s: &str) -> String {
    if s.len() <= MAX_BODY {
        return s.to_owned();
    }
    let mut end = MAX_BODY;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… ({} more bytes)", &s[..end], s.len() - end)
}

fn str_at<'a>(json: &'a Value, pointer: &str) -> &'a str {
    json.pointer(pointer).and_then(Value::as_str).unwrap_or("")
}

fn entry(kind: EntryKind, title: &str, body: String, is_error: bool) -> TranscriptEntry {
    TranscriptEntry {
        kind,
        title: title.to_owned(),
        body,
        is_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: &str, json: &str) -> RawEventRow {
        RawEventRow {
            id: 1,
            workflow_id: "w".into(),
            round_id: None,
            branch_id: None,
            seq: 1,
            kind: kind.into(),
            json: json.into(),
            created_at: 0,
        }
    }

    #[test]
    fn renders_text_tools_and_results() {
        let events = [
            row(
                "assistant",
                r#"{"message":{"content":[{"type":"text","text":"Looking."},{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
            ),
            row(
                "user",
                r#"{"message":{"content":[{"type":"tool_result","content":[{"type":"text","text":"a\nb"}],"is_error":true}]}}"#,
            ),
            row(
                "result",
                r#"{"subtype":"success","total_cost_usd":0.5,"duration_ms":1500}"#,
            ),
            row("assistant", "not json"),
        ];
        let t = render(&events);
        assert_eq!(t.len(), 4);
        assert_eq!(t[0].body, "Looking.");
        assert_eq!(
            (t[1].title.as_str(), t[1].body.as_str()),
            ("Bash", "command: ls")
        );
        assert!(t[2].is_error);
        assert_eq!(t[2].body, "a\nb");
        assert_eq!(t[3].body, "1.5s · $0.5000 total");
    }
}
