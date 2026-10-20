//! What the agent is doing in the open round, distilled from the raw stream:
//! the live card in the event flow and the slot rail's second line read this,
//! so raw events never reach the views (AGENTS.md §11).

use std::collections::HashMap;

use serde_json::Value;

/// The live card's line while Claude Code compacts the conversation.
pub const COMPACTING: &str = "Compacting the conversation";

/// "The API is overloaded; trying again (2 of 10) in 8s".
fn retrying(json: &Value) -> String {
    let why = match json.get("error").and_then(Value::as_str) {
        Some("overloaded") => "The API is overloaded",
        Some("rate_limit") => "Rate limited",
        Some("server_error") => "The API had an error",
        Some("authentication_failed" | "oauth_org_not_allowed") => "Sign-in failed",
        _ => match json.get("error_status").and_then(Value::as_u64) {
            Some(status) => return format!("The API answered {status}; trying again"),
            None => "The request failed",
        },
    };
    let attempt = json.get("attempt").and_then(Value::as_u64).unwrap_or(1);
    let max = json.get("max_retries").and_then(Value::as_u64).unwrap_or(0);
    let delay = json
        .get("retry_delay_ms")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let wait = if delay >= 1_000 {
        format!(" in {}s", (delay + 500) / 1_000)
    } else {
        String::new()
    };
    if max > 0 {
        format!("{why}; trying again ({attempt} of {max}){wait}")
    } else {
        format!("{why}; trying again{wait}")
    }
}

/// One tool call, phrased for people ("Reading `main.rs`").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub tool_use_id: String,
    /// Present-tense label while running ("Reading `main.rs`").
    pub doing: String,
    /// Past-tense label once done ("Read `main.rs`").
    pub done: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub failed: bool,
}

/// The open round of one workflow.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoundActivity {
    pub started_at: i64,
    /// The agent's latest prose on the main lane, first sentence.
    pub title: Option<String>,
    /// What Claude Code itself is doing (compacting the conversation); shown
    /// over the title while set, which stays for after.
    pub status: Option<String>,
    /// Main-lane tool calls, oldest first.
    pub steps: Vec<Step>,
    /// Every tool call this round, branches included.
    pub tool_calls: usize,
    /// Subagents spawned this round.
    pub subagents: usize,
    /// The latest step per branch (live lanes show it inline).
    pub branch_steps: HashMap<String, Step>,
    /// Claude Code's one-line summary of what each subagent is doing, every
    /// ~30s (`agentProgressSummaries`); shown over its latest step.
    pub branch_summaries: HashMap<String, String>,
}

impl RoundActivity {
    #[must_use]
    pub fn new(started_at: i64) -> Self {
        Self {
            started_at,
            ..Self::default()
        }
    }

    /// Claude Code's own status: compacting the conversation
    /// (`system/status`), or retrying a failed API request (`api_retry`).
    fn observe_status(&mut self, json: &Value) -> bool {
        let status = match json.get("subtype").and_then(Value::as_str) {
            Some("status") => (json.get("status").and_then(Value::as_str) == Some("compacting"))
                .then(|| COMPACTING.to_owned()),
            Some("api_retry") => Some(retrying(json)),
            _ => return false,
        };
        let changed = status != self.status;
        self.status = status;
        changed
    }

    /// The step running now on the main lane, if any.
    #[must_use]
    pub fn current(&self) -> Option<&Step> {
        self.steps.iter().rev().find(|s| s.finished_at.is_none())
    }

    /// One line for the slot rail: what is happening now, without markup.
    #[must_use]
    pub fn headline(&self) -> String {
        self.status
            .clone()
            .or_else(|| self.current().map(|s| strip_ticks(&s.doing)))
            .or_else(|| self.title.clone())
            .unwrap_or_else(|| "Working".to_owned())
    }

    /// The live card's line: Claude Code's status while it has one, else the
    /// agent's latest prose.
    #[must_use]
    pub fn line(&self) -> Option<&str> {
        self.status.as_deref().or(self.title.as_deref())
    }

    /// Feed one raw SDK event. Returns whether anything changed.
    pub fn observe(&mut self, kind: &str, json: &Value, branch_id: Option<&str>, now: i64) -> bool {
        if kind == "system" {
            return self.observe_status(json);
        }
        // The request went through after all: the retry line goes.
        let retried = kind == "assistant"
            && branch_id.is_none()
            && self.status.as_deref().is_some_and(|s| s != COMPACTING);
        if retried {
            self.status = None;
        }
        let Some(content) = json.pointer("/message/content").and_then(Value::as_array) else {
            return false;
        };
        let mut changed = false;
        match kind {
            "assistant" => {
                for block in content {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") if branch_id.is_none() => {
                            if let Some(t) = block
                                .get("text")
                                .and_then(Value::as_str)
                                .and_then(first_sentence)
                            {
                                self.title = Some(t);
                                changed = true;
                            }
                        }
                        Some("tool_use") => {
                            let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                            let id = block
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned();
                            let input = block.get("input").cloned().unwrap_or(Value::Null);
                            self.tool_calls += 1;
                            changed = true;
                            if matches!(name, "Agent" | "Task") {
                                if branch_id.is_none() {
                                    self.subagents += 1;
                                }
                                continue;
                            }
                            let Some((doing, done)) = describe(name, &input) else {
                                continue;
                            };
                            let step = Step {
                                tool_use_id: id,
                                doing,
                                done,
                                started_at: now,
                                finished_at: None,
                                failed: false,
                            };
                            match branch_id {
                                None => self.steps.push(step),
                                Some(b) => {
                                    self.branch_steps.insert(b.to_owned(), step);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            "user" => {
                for block in content {
                    if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                        continue;
                    }
                    let Some(id) = block.get("tool_use_id").and_then(Value::as_str) else {
                        continue;
                    };
                    let failed = block
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let finish = |s: &mut Step| {
                        if s.tool_use_id == id && s.finished_at.is_none() {
                            s.finished_at = Some(now);
                            s.failed = failed;
                            true
                        } else {
                            false
                        }
                    };
                    for s in &mut self.steps {
                        changed |= finish(s);
                    }
                    for s in self.branch_steps.values_mut() {
                        changed |= finish(s);
                    }
                }
            }
            _ => {}
        }
        changed
    }
}

/// `(doing, done)` labels for a tool call; `None` for tools that show up as
/// flow nodes instead (Berth's own tools, todo lists).
#[must_use]
pub fn describe(name: &str, input: &Value) -> Option<(String, String)> {
    let s = |k: &str| input.get(k).and_then(Value::as_str).map(str::trim);
    let file = |k: &str| s(k).map(|p| p.rsplit('/').next().unwrap_or(p).to_owned());
    let tick = |x: &str| format!("`{}`", one_line(x, 60));
    if name.starts_with("mcp__berth__") || matches!(name, "TodoWrite" | "ExitPlanMode") {
        return None;
    }
    Some(match name {
        "Read" | "NotebookRead" => {
            let f = file("file_path")
                .or_else(|| file("notebook_path"))
                .unwrap_or_default();
            (
                format!("Reading {}", tick(&f)),
                format!("Read {}", tick(&f)),
            )
        }
        "Edit" | "MultiEdit" | "NotebookEdit" => {
            let f = file("file_path")
                .or_else(|| file("notebook_path"))
                .unwrap_or_default();
            (
                format!("Editing {}", tick(&f)),
                format!("Edited {}", tick(&f)),
            )
        }
        "Write" => {
            let f = file("file_path").unwrap_or_default();
            (
                format!("Writing {}", tick(&f)),
                format!("Wrote {}", tick(&f)),
            )
        }
        "Bash" => match s("description").filter(|d| !d.is_empty()) {
            Some(d) => (one_line(d, 80), one_line(d, 80)),
            None => {
                let c = s("command").unwrap_or_default();
                (format!("Running {}", tick(c)), format!("Ran {}", tick(c)))
            }
        },
        "Grep" => {
            let p = s("pattern").unwrap_or_default();
            (
                format!("Searching for {}", tick(p)),
                format!("Searched for {}", tick(p)),
            )
        }
        "Glob" => {
            let p = s("pattern").unwrap_or_default();
            (format!("Finding {}", tick(p)), format!("Found {}", tick(p)))
        }
        "WebFetch" => {
            let u = s("url").unwrap_or_default();
            (format!("Fetching {u}"), format!("Fetched {u}"))
        }
        "WebSearch" => {
            let q = s("query").unwrap_or_default();
            (
                format!("Searching the web for “{q}”"),
                format!("Searched the web for “{q}”"),
            )
        }
        other => {
            let n = other
                .strip_prefix("mcp__")
                .unwrap_or(other)
                .replace("__", " ");
            (format!("Using {n}"), format!("Used {n}"))
        }
    })
}

fn one_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.chars().count() > max {
        format!("{}…", line.chars().take(max).collect::<String>())
    } else {
        line.to_owned()
    }
}

/// The first sentence of some prose, without a trailing colon.
fn first_sentence(text: &str) -> Option<String> {
    let line = text.trim().lines().find(|l| !l.trim().is_empty())?.trim();
    let end = line
        .char_indices()
        .find(|(i, c)| matches!(c, '.' | '!' | '?') && line[i + c.len_utf8()..].starts_with(' '))
        .map_or(line.len(), |(i, c)| i + c.len_utf8());
    let s = line[..end].trim().trim_end_matches([':', '.']).trim();
    (!s.is_empty()).then(|| one_line(s, 90))
}

fn strip_ticks(s: &str) -> String {
    s.replace('`', "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn api_retries_show_until_the_agent_answers() {
        let mut a = RoundActivity::new(0);
        a.title = Some("Reading the config".to_owned());
        let retry = serde_json::json!({
            "type": "system", "subtype": "api_retry", "attempt": 2, "max_retries": 10,
            "retry_delay_ms": 8_000, "error_status": 529, "error": "overloaded"
        });
        assert!(a.observe("system", &retry, None, 1));
        assert_eq!(
            a.headline(),
            "The API is overloaded; trying again (2 of 10) in 8s"
        );
        let answer =
            serde_json::json!({"message": {"content": [{"type": "text", "text": "Done."}]}});
        assert!(a.observe("assistant", &answer, None, 2));
        assert_eq!(a.status, None);
    }

    #[test]
    fn compaction_shows_while_it_runs() {
        let mut a = RoundActivity::new(0);
        let status =
            |s: Value| serde_json::json!({"type": "system", "subtype": "status", "status": s});
        a.title = Some("Reading the config".to_owned());
        assert!(a.observe("system", &status(Value::from("compacting")), None, 1));
        assert_eq!(a.headline(), COMPACTING);
        assert_eq!(a.line(), Some(COMPACTING));
        assert!(a.observe("system", &status(Value::Null), None, 2));
        // The agent's line is still there once it is done.
        assert_eq!(a.headline(), "Reading the config");
        assert!(!a.observe("system", &serde_json::json!({"subtype": "init"}), None, 3));
    }

    #[test]
    fn steps_follow_tool_calls() {
        let mut a = RoundActivity::new(0);
        let msg = json!({"message": {"content": [
            {"type": "text", "text": "Running both checks in parallel. Then I'll ask."},
            {"type": "tool_use", "id": "t1", "name": "Read", "input": {"file_path": "/w/main.rs"}},
            {"type": "tool_use", "id": "t2", "name": "Agent", "input": {}},
        ]}});
        assert!(a.observe("assistant", &msg, None, 1000));
        assert_eq!(a.title.as_deref(), Some("Running both checks in parallel"));
        assert_eq!((a.tool_calls, a.subagents, a.steps.len()), (2, 1, 1));
        assert_eq!(a.headline(), "Reading main.rs");
        let res = json!({"message": {"content": [{"type": "tool_result", "tool_use_id": "t1"}]}});
        assert!(a.observe("user", &res, None, 4000));
        assert_eq!(a.steps[0].finished_at, Some(4000));
        assert_eq!(a.steps[0].done, "Read `main.rs`");
        assert_eq!(a.headline(), "Running both checks in parallel");

        let branch = json!({"message": {"content": [
            {"type": "tool_use", "id": "b1", "name": "Bash", "input": {"command": "ls tests/", "description": "Reading tests/"}},
        ]}});
        a.observe("assistant", &branch, Some("br"), 5000);
        assert_eq!(a.branch_steps["br"].doing, "Reading tests/");
        assert_eq!(a.tool_calls, 3);
        // Berth tools are nodes, not steps.
        assert!(describe("mcp__berth__record_decision", &json!({})).is_none());
    }
}
