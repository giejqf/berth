//! The provider's errors as the Messages API's, so Claude Code reacts as
//! it does to Anthropic's: tries again on rate limits and overloads, stops
//! on a bad key, and compacts on a prompt too long for the model.

use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApiError {
    pub status: u16,
    pub kind: &'static str,
    pub message: String,
}

impl ApiError {
    pub(crate) fn new(status: u16, kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            kind,
            message: message.into(),
        }
    }

    pub(crate) fn body(&self) -> Value {
        json!({ "type": "error", "error": { "type": self.kind, "message": self.message } })
    }

    /// The provider answered `status` with `body`.
    pub(crate) fn from_upstream(status: u16, body: &str, host: &str, estimate: u64) -> Self {
        let (message, _) = upstream_message(body);
        if let Some(o) = Overflow::read(&message) {
            return Self::new(400, "invalid_request_error", o.too_long(estimate));
        }
        let message = format!("{host}: {message}");
        match status {
            401 => Self::new(401, "authentication_error", message),
            403 => Self::new(403, "permission_error", message),
            404 => Self::new(404, "not_found_error", message),
            413 => Self::new(413, "request_too_large", message),
            429 => Self::new(429, "rate_limit_error", message),
            400..=499 => Self::new(400, "invalid_request_error", message),
            503 | 529 => Self::new(529, "overloaded_error", message),
            _ => Self::new(500, "api_error", message),
        }
    }

    /// An error the provider sent inside a stream that had begun.
    pub(crate) fn in_stream(err: &Value) -> Self {
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .map_or_else(|| err.to_string(), str::to_owned);
        if let Some(o) = Overflow::read(&message) {
            return Self::new(400, "invalid_request_error", o.too_long(0));
        }
        let code = err.get("code").and_then(Value::as_u64);
        match code {
            Some(429) => Self::new(429, "rate_limit_error", message),
            _ => Self::new(529, "overloaded_error", message),
        }
    }

    pub(crate) fn unreachable(host: &str, detail: &str) -> Self {
        Self::new(502, "api_error", format!("Couldn't reach {host}: {detail}"))
    }
}

/// The message in a provider's error body, and the parameter it names:
/// `{"error":{"message","param"}}` as OpenAI writes it, or the shapes
/// others use (`{"message"}`, `{"error":"…"}`, `{"detail"}`).
pub(crate) fn upstream_message(body: &str) -> (String, Option<String>) {
    let parsed = serde_json::from_str::<Value>(body).ok();
    let err = parsed.as_ref().map(|v| v.get("error").unwrap_or(v));
    let text = |v: Option<&Value>| match v {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
        _ => None,
    };
    let message = err
        .and_then(|e| text(e.get("message")).or_else(|| text(e.get("detail"))))
        .or_else(|| text(err))
        .or_else(|| text(parsed.as_ref().and_then(|v| v.get("message"))))
        .unwrap_or_else(|| {
            let b = body.trim();
            if b.is_empty() {
                "no details".to_owned()
            } else {
                b.chars().take(300).collect()
            }
        });
    let param = err
        .and_then(|e| e.get("param"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    (message, param)
}

/// A request too long for the model's context window, as providers word it:
/// "This model's maximum context length is 32768 tokens. However, you
/// requested 34000 tokens (30000 in the messages, 4000 in the completion)."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Overflow {
    pub maximum: Option<u64>,
    /// The whole request, prompt and completion budget.
    pub requested: Option<u64>,
    /// The prompt alone, when the message names it.
    pub prompt: Option<u64>,
}

impl Overflow {
    pub(crate) fn read(message: &str) -> Option<Self> {
        let lower = message.to_ascii_lowercase();
        let about = [
            "context length",
            "context_length",
            "context window",
            "prompt is too long",
            "too many tokens",
            "maximum context",
            "input is too long",
        ];
        if !about.iter().any(|a| lower.contains(a)) {
            return None;
        }
        let maximum = number_after(
            &lower,
            &["length is", "window is", "limit is", "maximum of"],
        )
        .or_else(|| number_after(&lower, &["> "]));
        let requested = number_after(
            &lower,
            &[
                "requested about",
                "requested",
                "resulted in",
                "you have",
                "got",
            ],
        );
        let prompt = number_before(&lower, " in the messages");
        Some(Self {
            maximum,
            requested,
            prompt,
        })
    }

    /// The error Claude Code compacts on: "prompt is too long: N tokens > M
    /// maximum".
    pub(crate) fn too_long(&self, estimate: u64) -> String {
        let maximum = self.maximum.unwrap_or(estimate.saturating_sub(1).max(1));
        let actual = self
            .prompt
            .or(self.requested)
            .unwrap_or(estimate)
            .max(maximum + 1);
        format!("prompt is too long: {actual} tokens > {maximum} maximum")
    }

    /// A smaller `max_tokens` that would leave the prompt room, when the
    /// prompt fits and it was the completion budget that didn't.
    pub(crate) fn budget(&self, sent_max: u64) -> Option<u64> {
        let maximum = self.maximum?;
        let prompt = self
            .prompt
            .or_else(|| self.requested.map(|r| r.saturating_sub(sent_max)))?;
        let room = maximum.checked_sub(prompt)?.checked_sub(256)?;
        (room >= 1024 && room < sent_max).then_some(room)
    }
}

/// The first number after any of `phrases` in `text`.
fn number_after(text: &str, phrases: &[&str]) -> Option<u64> {
    phrases.iter().find_map(|p| {
        let at = text.find(p)? + p.len();
        leading_number(&text[at..])
    })
}

fn number_before(text: &str, phrase: &str) -> Option<u64> {
    let at = text.find(phrase)?;
    let head = text[..at].trim_end();
    let digits: String = head
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .collect();
    digits
        .chars()
        .rev()
        .filter(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

/// The number `s` starts with, past spaces, a colon or a backtick: `` `8192` ``,
/// "128,000".
fn leading_number(s: &str) -> Option<u64> {
    let s = s.trim_start_matches([' ', ':', '`', '\'', '"']);
    let digits: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .filter(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// Every whole number in `text`, thousands separators read through: "8,192".
pub(crate) fn numbers(text: &str) -> Vec<u64> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut digits = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let separator =
            c == ',' && !digits.is_empty() && chars.get(i + 1).is_some_and(char::is_ascii_digit);
        if c.is_ascii_digit() {
            digits.push(c);
        } else if !separator && !digits.is_empty() {
            out.extend(digits.parse::<u64>().ok());
            digits.clear();
        }
    }
    out.extend(digits.parse::<u64>().ok());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_map_to_what_claude_code_retries() {
        let body = r#"{"error":{"message":"Rate limit reached","type":"requests"}}"#;
        let e = ApiError::from_upstream(429, body, "api.openai.com", 0);
        assert_eq!((e.status, e.kind), (429, "rate_limit_error"));
        assert_eq!(e.message, "api.openai.com: Rate limit reached");
        let e = ApiError::from_upstream(503, "upstream down", "x", 0);
        assert_eq!((e.status, e.kind), (529, "overloaded_error"));
        assert_eq!(e.message, "x: upstream down");
        let e = ApiError::from_upstream(401, r#"{"error":"Invalid API key"}"#, "x", 0);
        assert_eq!(
            (e.status, e.kind, e.message.as_str()),
            (401, "authentication_error", "x: Invalid API key")
        );
    }

    #[test]
    fn a_full_context_window_reads_as_a_prompt_too_long() {
        let openai = r#"{"error":{"message":"This model's maximum context length is 128000 tokens. However, your messages resulted in 130512 tokens. Please reduce the length of the messages.","code":"context_length_exceeded"}}"#;
        let e = ApiError::from_upstream(400, openai, "api.openai.com", 0);
        assert_eq!(
            e.message,
            "prompt is too long: 130512 tokens > 128000 maximum"
        );
        let vllm = "This model's maximum context length is 32768 tokens. However, you requested 40000 tokens (36000 in the messages, 4000 in the completion).";
        let o = Overflow::read(vllm);
        assert_eq!(
            o,
            Some(Overflow {
                maximum: Some(32768),
                requested: Some(40000),
                prompt: Some(36000)
            })
        );
        assert_eq!(
            o.map(|o| o.too_long(0)).as_deref(),
            Some("prompt is too long: 36000 tokens > 32768 maximum")
        );
        assert!(Overflow::read("Invalid API key").is_none());
    }

    #[test]
    fn a_completion_budget_too_big_for_the_window_shrinks() {
        let msg = "This model's maximum context length is 32768 tokens. However, you requested 52000 tokens (20000 in the messages, 32000 in the completion).";
        let o = Overflow::read(msg);
        assert_eq!(o.and_then(|o| o.budget(32000)), Some(12512));
        // The prompt alone overflows: no budget helps.
        let msg = "maximum context length is 32768 tokens. However, you requested 70000 tokens (38000 in the messages, 32000 in the completion).";
        assert_eq!(Overflow::read(msg).and_then(|o| o.budget(32000)), None);
    }

    #[test]
    fn messages_come_from_the_shapes_providers_use() {
        assert_eq!(upstream_message(r#"{"detail":"Not Found"}"#).0, "Not Found");
        assert_eq!(upstream_message(r#"{"message":"bad"}"#).0, "bad");
        let (m, p) = upstream_message(
            r#"{"error":{"message":"Unsupported parameter","param":"temperature"}}"#,
        );
        assert_eq!(
            (m.as_str(), p.as_deref()),
            ("Unsupported parameter", Some("temperature"))
        );
        assert_eq!(upstream_message("").0, "no details");
        assert_eq!(numbers("between 1 and 8,192, at most."), [1, 8192]);
    }
}
