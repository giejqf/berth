//! "Show details": turn a node's raw SDK events into readable transcript entries.
//!
//! A tool call and its result are one entry. Each tool says in one line what
//! it did (the command, the file, the pattern); its arguments and output are
//! there when the entry is opened. Paths inside the checkout are written
//! relative to it, and Claude Code's `<system-reminder>` asides are left out.

use serde_json::{Map, Value};

use crate::model::RawEventRow;

/// Longest text body (a tool result, a long argument) shown before truncation.
const MAX_BODY: usize = 8000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A message to the agent: the user's prompt, or a subagent's task.
    User,
    AssistantText,
    Thinking,
    /// A tool call, with its result once it came.
    Tool,
    /// A result whose call is outside the span.
    ToolResult,
    System,
    /// The turn's end: duration and cost.
    Result,
}

/// How an entry's body reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyStyle {
    /// Markdown prose (messages, thinking).
    Prose,
    /// Verbatim mono (a command, a file's new content).
    Code,
    /// An edit: lines starting `- ` were removed, `+ ` added.
    Diff,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    pub text: String,
    pub is_error: bool,
    /// When it came (unix ms).
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEntry {
    pub kind: EntryKind,
    /// "You", "Claude", "Thinking", the tool's name, "Session started".
    pub title: String,
    /// One line after the title: a tool's command, file or pattern; the
    /// turn's duration and cost.
    pub summary: String,
    /// The summary and field values are code (paths, commands), not prose.
    pub code: bool,
    /// A tool's other arguments, in the agent's order (by name where
    /// serde_json doesn't keep it).
    pub fields: Vec<(String, String)>,
    pub body: String,
    pub style: BodyStyle,
    pub output: Option<ToolOutput>,
    pub is_error: bool,
    /// When it happened (unix ms).
    pub at: i64,
}

impl TranscriptEntry {
    fn new(kind: EntryKind, title: impl Into<String>, at: i64) -> Self {
        Self {
            kind,
            title: title.into(),
            summary: String::new(),
            code: false,
            fields: Vec::new(),
            body: String::new(),
            style: BodyStyle::Prose,
            output: None,
            is_error: false,
            at,
        }
    }

    fn summary(mut self, s: impl Into<String>) -> Self {
        self.summary = s.into();
        self
    }

    fn body(mut self, body: impl Into<String>, style: BodyStyle) -> Self {
        self.body = body.into();
        self.style = style;
        self
    }

    /// Whether the entry failed: a tool whose result is an error, or an
    /// errored turn.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.is_error || self.output.as_ref().is_some_and(|o| o.is_error)
    }
}

/// Where the events ran and whose they are.
#[derive(Debug, Clone, Copy, Default)]
pub struct Context<'a> {
    /// The checkout: paths under it are shown relative to it.
    pub cwd: Option<&'a str>,
    /// The events are a subagent's (its prompt came from the main agent).
    pub subagent: bool,
}

/// Render raw events in order. Unknown shapes are skipped, never fatal.
#[must_use]
pub fn render(events: &[RawEventRow], cx: Context<'_>) -> Vec<TranscriptEntry> {
    let mut r = Renderer {
        out: Vec::new(),
        cwd: cx.cwd.map(|c| c.trim_end_matches('/').to_owned()),
        subagent: cx.subagent,
        pending: Vec::new(),
    };
    for e in events {
        let Ok(json) = serde_json::from_str::<Value>(&e.json) else {
            continue;
        };
        match e.kind.as_str() {
            "assistant" => r.assistant(&json, e.created_at),
            "user" => r.user(&json, e.created_at),
            "system" => r.system(&json, e.created_at),
            "result" => r.result(&json, e.created_at),
            _ => {}
        }
    }
    r.out
}

struct Renderer {
    out: Vec<TranscriptEntry>,
    cwd: Option<String>,
    subagent: bool,
    /// Tool calls waiting for their result: (tool_use_id, index in `out`).
    pending: Vec<(String, usize)>,
}

impl Renderer {
    fn system(&mut self, json: &Value, at: i64) {
        // Only system events a reader cares about; hooks, token counters
        // and status pings are noise.
        let entry = match str_at(json, "/subtype") {
            "init" => {
                let cwd = str_at(json, "/cwd");
                if self.cwd.is_none() && !cwd.is_empty() {
                    self.cwd = Some(cwd.trim_end_matches('/').to_owned());
                }
                let mut s = str_at(json, "/model").to_owned();
                if !cwd.is_empty() {
                    s = if s.is_empty() {
                        cwd.to_owned()
                    } else {
                        format!("{s} · {cwd}")
                    };
                }
                TranscriptEntry::new(EntryKind::System, "Session started", at).summary(s)
            }
            "task_notification" => {
                TranscriptEntry::new(EntryKind::System, "Background agent finished", at)
                    .summary(one_line(str_at(json, "/summary")))
            }
            "compact_boundary" => TranscriptEntry::new(EntryKind::System, "Context compacted", at),
            _ => return,
        };
        self.out.push(entry);
    }

    fn result(&mut self, json: &Value, at: i64) {
        let subtype = json
            .get("subtype")
            .and_then(Value::as_str)
            .unwrap_or("success");
        let cost = json
            .get("total_cost_usd")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let ms = json.get("duration_ms").and_then(Value::as_u64).unwrap_or(0);
        let turns = json.get("num_turns").and_then(Value::as_u64);
        let is_error = json
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || subtype.starts_with("error");
        #[allow(clippy::cast_precision_loss)]
        let mut parts = vec![format!("{:.1}s", ms as f64 / 1000.0)];
        if cost > 0.0 {
            parts.push(format!("${cost:.4}"));
        }
        if let Some(n) = turns {
            parts.push(format!("{n} {}", if n == 1 { "turn" } else { "turns" }));
        }
        let title = if subtype == "success" {
            "Turn ended".to_owned()
        } else {
            format!("Turn ended: {}", subtype.replace('_', " "))
        };
        let mut e = TranscriptEntry::new(EntryKind::Result, title, at).summary(parts.join(" · "));
        e.is_error = is_error;
        self.out.push(e);
    }

    fn assistant(&mut self, json: &Value, at: i64) {
        let Some(blocks) = json.pointer("/message/content").and_then(Value::as_array) else {
            return;
        };
        for b in blocks {
            match b.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let text = b.get("text").and_then(Value::as_str).unwrap_or_default();
                    if !text.trim().is_empty() {
                        let who = if self.subagent { "Subagent" } else { "Claude" };
                        self.out.push(
                            TranscriptEntry::new(EntryKind::AssistantText, who, at)
                                .body(text.trim(), BodyStyle::Prose),
                        );
                    }
                }
                Some("thinking") => {
                    let text = b
                        .get("thinking")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !text.trim().is_empty() {
                        self.out.push(
                            TranscriptEntry::new(EntryKind::Thinking, "Thinking", at)
                                .body(text.trim(), BodyStyle::Prose),
                        );
                    }
                }
                Some("tool_use") => {
                    let name = b.get("name").and_then(Value::as_str).unwrap_or("tool");
                    let empty = Map::new();
                    let input = b.get("input").and_then(Value::as_object).unwrap_or(&empty);
                    let e = self.tool(name, input, at);
                    if let Some(id) = b.get("id").and_then(Value::as_str) {
                        self.pending.push((id.to_owned(), self.out.len()));
                    }
                    self.out.push(e);
                }
                _ => {}
            }
        }
    }

    fn user(&mut self, json: &Value, at: i64) {
        let who = if self.subagent { "Task" } else { "You" };
        match json.pointer("/message/content") {
            Some(Value::String(s)) => self.message(who, s, at),
            Some(Value::Array(blocks)) => {
                for b in blocks {
                    match b.get("type").and_then(Value::as_str) {
                        Some("tool_result") => {
                            let output = ToolOutput {
                                text: self.relative(&truncate(
                                    strip_reminders(&content_text(b.get("content"))).trim_end(),
                                )),
                                is_error: b
                                    .get("is_error")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(false),
                                at,
                            };
                            let id = b.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                            match self.pending.iter().position(|(p, _)| p == id) {
                                Some(i) => {
                                    let (_, ix) = self.pending.remove(i);
                                    if let Some(e) = self.out.get_mut(ix) {
                                        e.output = Some(output);
                                    }
                                }
                                None => {
                                    let mut e = TranscriptEntry::new(
                                        EntryKind::ToolResult,
                                        "Tool result",
                                        at,
                                    );
                                    e.output = Some(output);
                                    self.out.push(e);
                                }
                            }
                        }
                        Some("text") => {
                            let text = b.get("text").and_then(Value::as_str).unwrap_or_default();
                            self.message(who, text, at);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn message(&mut self, who: &str, text: &str, at: i64) {
        let text = strip_reminders(text);
        if !text.trim().is_empty() {
            self.out.push(
                TranscriptEntry::new(EntryKind::User, who, at).body(text.trim(), BodyStyle::Prose),
            );
        }
    }

    /// A tool call: its name, the one line saying what it does, and the rest
    /// of its arguments.
    fn tool(&self, name: &str, input: &Map<String, Value>, at: i64) -> TranscriptEntry {
        let (title, berth) = match name.strip_prefix("mcp__") {
            Some(rest) => match rest.split_once("__") {
                Some(("berth", tool)) => (tool.to_owned(), true),
                Some((server, tool)) => (format!("{server} · {tool}"), false),
                None => (rest.to_owned(), false),
            },
            None => (name.to_owned(), false),
        };
        let mut e = TranscriptEntry::new(EntryKind::Tool, title.clone(), at);
        let name = if berth { title.as_str() } else { name };
        e.code = !berth;
        let s = |key: &str| input.get(key).and_then(Value::as_str).unwrap_or("");
        let path = |key: &str| self.relative(s(key));
        // Arguments the summary or body already show.
        let mut used: Vec<&str> = Vec::new();
        match (berth, name) {
            (false, "Bash") => {
                let cmd = s("command");
                e.summary = one_line(cmd);
                if cmd.trim().contains('\n') {
                    e = e.body(cmd.trim(), BodyStyle::Code);
                }
                used.push("command");
            }
            (false, "Read") => {
                e.summary = path("file_path");
                let offset = input.get("offset").and_then(Value::as_u64);
                let limit = input.get("limit").and_then(Value::as_u64);
                if let Some(first) = offset.or(limit.map(|_| 1)) {
                    match limit {
                        Some(n) => {
                            let last = first + n.saturating_sub(1);
                            e.summary.push_str(&format!(" · lines {first}–{last}"));
                        }
                        None => e.summary.push_str(&format!(" · from line {first}")),
                    }
                }
                used.extend(["file_path", "offset", "limit"]);
            }
            (false, "Write") => {
                e.summary = path("file_path");
                e = e.body(truncate(s("content")), BodyStyle::Code);
                used.extend(["file_path", "content"]);
            }
            (false, "Edit") => {
                e.summary = path("file_path");
                if input.get("replace_all").and_then(Value::as_bool) == Some(true) {
                    e.summary.push_str(" · every occurrence");
                }
                e = e.body(diff(s("old_string"), s("new_string")), BodyStyle::Diff);
                used.extend(["file_path", "old_string", "new_string", "replace_all"]);
            }
            (false, "MultiEdit") => {
                e.summary = path("file_path");
                let edits = input.get("edits").and_then(Value::as_array);
                let body = edits
                    .into_iter()
                    .flatten()
                    .map(|ed| {
                        let get = |k: &str| ed.get(k).and_then(Value::as_str).unwrap_or("");
                        diff(get("old_string"), get("new_string"))
                    })
                    .collect::<Vec<_>>()
                    .join("\n⋯\n");
                e = e.body(body, BodyStyle::Diff);
                used.extend(["file_path", "edits"]);
            }
            (false, "NotebookEdit") => {
                e.summary = path("notebook_path");
                e = e.body(truncate(s("new_source")), BodyStyle::Code);
                used.extend(["notebook_path", "new_source"]);
            }
            (false, "Grep" | "Glob") => {
                e.summary = s("pattern").to_owned();
                let p = path("path");
                if !p.is_empty() && p != "." {
                    e.summary.push_str(&format!(" in {p}"));
                }
                used.extend(["pattern", "path"]);
            }
            (false, "WebFetch") => {
                e.summary = s("url").to_owned();
                used.push("url");
            }
            (false, "WebSearch") => {
                e.summary = s("query").to_owned();
                used.push("query");
            }
            (false, "Agent" | "Task") => {
                e.code = false;
                e.summary = s("description").to_owned();
                e = e.body(s("prompt"), BodyStyle::Prose);
                used.extend(["description", "prompt"]);
            }
            (false, "TodoWrite") => {
                e.code = false;
                let todos = input.get("todos").and_then(Value::as_array);
                let n = todos.map_or(0, Vec::len);
                e.summary = format!("{n} {}", if n == 1 { "item" } else { "items" });
                let body = todos
                    .into_iter()
                    .flatten()
                    .map(|t| {
                        let mark = match t.get("status").and_then(Value::as_str) {
                            Some("completed") => "✓",
                            Some("in_progress") => "→",
                            _ => "·",
                        };
                        let text = t.get("content").and_then(Value::as_str).unwrap_or("");
                        format!("{mark} {text}")
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                e = e.body(body, BodyStyle::Code);
                used.push("todos");
            }
            (true, "record_decision" | "present_artifact") => {
                e.summary = s("title").to_owned();
                used.push("title");
            }
            (true, "ask_user") => {
                e.summary = one_line(s("question"));
                used.push("question");
            }
            (true, "write_summary") => {
                let n = |k: &str| input.get(k).and_then(Value::as_array).map_or(0, Vec::len);
                e.summary = format!("{} done · {} remaining", n("done"), n("remaining"));
                if n("follow_ups") > 0 {
                    e.summary
                        .push_str(&format!(" · {} follow-ups", n("follow_ups")));
                }
            }
            (true, "reply") => {
                e.summary = one_line(s("message"));
                e = e.body(s("message"), BodyStyle::Prose);
                used.push("message");
            }
            (true, "record_outcome") => {
                e.summary = format!("{} {}", s("kind"), s("ref")).trim().to_owned();
                used.extend(["kind", "ref"]);
            }
            (true, "present_review") => {
                let n = input
                    .get("findings")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                e.summary = format!("{n} {}", if n == 1 { "finding" } else { "findings" });
            }
            (true, "request_slot") => {
                e.summary = s("name").to_owned();
                used.push("name");
            }
            _ => {
                // The first short string argument says it best.
                if let Some((k, v)) = input
                    .iter()
                    .find_map(|(k, v)| v.as_str().filter(|v| !v.contains('\n')).map(|v| (k, v)))
                {
                    e.summary = self.relative(v);
                    used.push(k.as_str());
                }
            }
        }
        // Empty arguments say nothing the summary doesn't.
        let empty = |v: &Value| match v {
            Value::Null => true,
            Value::Array(a) => a.is_empty(),
            Value::Object(o) => o.is_empty(),
            Value::String(s) => s.is_empty(),
            _ => false,
        };
        e.fields = input
            .iter()
            .filter(|(k, v)| !used.contains(&k.as_str()) && !empty(v))
            .map(|(k, v)| (k.clone(), self.relative(&value_text(v))))
            .collect();
        e
    }

    /// `text` with the checkout's path taken out of the paths in it.
    fn relative(&self, text: &str) -> String {
        match self.cwd.as_deref().filter(|c| !c.is_empty() && *c != "/") {
            Some(cwd) if text == cwd => ".".to_owned(),
            Some(cwd) => text.replace(&format!("{cwd}/"), ""),
            None => text.to_owned(),
        }
    }
}

/// An argument as text: strings as they are, lists of strings as a Markdown
/// list ("- a"), anything else as indented JSON.
fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => truncate(s),
        Value::Array(items) if items.iter().all(Value::is_string) => items
            .iter()
            .filter_map(Value::as_str)
            .map(|s| format!("- {s}"))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(_) | Value::Array(_) => {
            truncate(&serde_json::to_string_pretty(v).unwrap_or_default())
        }
        other => other.to_string(),
    }
}

/// An edit as removed and added lines.
fn diff(old: &str, new: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    if !old.is_empty() {
        out.extend(old.lines().map(|l| format!("- {l}")));
    }
    if !new.is_empty() {
        out.extend(new.lines().map(|l| format!("+ {l}")));
    }
    truncate(&out.join("\n"))
}

fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| match i.get("type").and_then(Value::as_str) {
                Some("image") => Some("[image]"),
                _ => i.get("text").and_then(Value::as_str),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// `text` without Claude Code's `<system-reminder>…</system-reminder>` asides.
fn strip_reminders(text: &str) -> String {
    const OPEN: &str = "<system-reminder>";
    const CLOSE: &str = "</system-reminder>";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        match rest[start..].find(CLOSE) {
            Some(end) => rest = &rest[start + end + CLOSE.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    if out.len() == text.len() {
        out
    } else {
        out.trim_end().to_owned()
    }
}

fn one_line(s: &str) -> String {
    let first = s.trim().lines().next().unwrap_or("");
    if s.trim().contains('\n') {
        format!("{first} …")
    } else {
        first.to_owned()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: &str, json: &str, at: i64) -> RawEventRow {
        RawEventRow {
            id: 1,
            workflow_id: "w".into(),
            round_id: None,
            branch_id: None,
            seq: 1,
            kind: kind.into(),
            json: json.into(),
            created_at: at,
        }
    }

    #[test]
    fn pairs_tools_with_their_results() {
        let events = [
            row(
                "assistant",
                r#"{"message":{"content":[{"type":"text","text":"Looking."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls","description":"List"}}]}}"#,
                1_000,
            ),
            row(
                "user",
                r#"{"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"a\nb"}],"is_error":true}]}}"#,
                3_500,
            ),
            row(
                "result",
                r#"{"subtype":"success","total_cost_usd":0.5,"duration_ms":1500,"num_turns":2}"#,
                4_000,
            ),
            row("assistant", "not json", 4_000),
        ];
        let t = render(&events, Context::default());
        assert_eq!(t.len(), 3);
        assert_eq!(t[0].body, "Looking.");
        assert_eq!((t[1].title.as_str(), t[1].summary.as_str()), ("Bash", "ls"));
        assert_eq!(t[1].fields, vec![("description".into(), "List".into())]);
        let out = t[1]
            .output
            .as_ref()
            .map(|o| (o.text.as_str(), o.is_error, o.at));
        assert_eq!(out, Some(("a\nb", true, 3_500)));
        assert!(t[1].failed());
        assert_eq!(t[2].summary, "1.5s · $0.5000 · 2 turns");
    }

    #[test]
    fn paths_are_relative_and_reminders_dropped() {
        let events = [
            row(
                "system",
                r#"{"subtype":"init","model":"m","cwd":"/r/co"}"#,
                0,
            ),
            row(
                "assistant",
                r#"{"message":{"content":[{"type":"tool_use","id":"t","name":"Edit","input":{"file_path":"/r/co/src/a.rs","old_string":"x","new_string":"y\nz"}}]}}"#,
                0,
            ),
            row(
                "user",
                r#"{"message":{"content":[{"type":"tool_result","tool_use_id":"t","content":"Updated /r/co/src/a.rs\n<system-reminder>\nnoise\n</system-reminder>"}]}}"#,
                0,
            ),
            row(
                "assistant",
                r#"{"message":{"content":[{"type":"tool_use","id":"d","name":"mcp__berth__record_decision","input":{"title":"T","rationale":"R","affects":["a","b"],"none":[]}}]}}"#,
                0,
            ),
        ];
        let t = render(&events, Context::default());
        assert_eq!(t[1].summary, "src/a.rs");
        assert_eq!(t[1].body, "- x\n+ y\n+ z");
        assert_eq!(t[1].style, BodyStyle::Diff);
        assert_eq!(
            t[1].output.as_ref().map(|o| o.text.as_str()),
            Some("Updated src/a.rs")
        );
        assert_eq!(
            (t[2].title.as_str(), t[2].summary.as_str(), t[2].code),
            ("record_decision", "T", false)
        );
        // Arguments keep the agent's order where serde_json preserves it (the
        // app's build); compare them sorted.
        let mut fields = t[2].fields.clone();
        fields.sort();
        assert_eq!(
            fields,
            vec![
                ("affects".into(), "- a\n- b".into()),
                ("rationale".into(), "R".into())
            ]
        );
    }

    #[test]
    fn a_result_without_its_call_stands_alone() {
        let events = [row(
            "user",
            r#"{"message":{"content":[{"type":"tool_result","tool_use_id":"gone","content":"ok"}]}}"#,
            0,
        )];
        let t = render(
            &events,
            Context {
                cwd: None,
                subagent: true,
            },
        );
        assert_eq!(t[0].kind, EntryKind::ToolResult);
    }
}
