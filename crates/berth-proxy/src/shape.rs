//! What each model takes, learned from what it refuses.
//!
//! Providers differ in small ways: OpenAI's reasoning models want
//! `max_completion_tokens` and only the default temperature, many models
//! cap their output below the 32k Claude Code asks for, some servers know no
//! `stream_options`. A refusal that names one of these is answered by
//! sending the request again without it, and the model's shape is kept for
//! its next requests (until Berth quits).

use serde_json::{Map, Value};

use crate::errors::numbers;

/// Parameters a request can go without.
const OPTIONAL: [&str; 6] = [
    "temperature",
    "top_p",
    "stop",
    "parallel_tool_calls",
    "stream_options",
    "tool_choice",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Shape {
    /// `max_completion_tokens` in place of `max_tokens`.
    pub completion_field: bool,
    /// The most output the model takes.
    pub max_tokens: Option<u64>,
    /// Parameters it refuses.
    pub dropped: Vec<&'static str>,
}

impl Shape {
    pub(crate) fn apply(&self, body: &mut Map<String, Value>, max_tokens: Option<u64>) {
        for p in &self.dropped {
            body.remove(*p);
        }
        if let Some(k) = max_tokens {
            let k = self.max_tokens.map_or(k, |cap| k.min(cap));
            let field = if self.completion_field {
                "max_completion_tokens"
            } else {
                "max_tokens"
            };
            body.insert(field.into(), k.into());
        }
    }

    /// The shape to try after the model refused a request with `message`
    /// (naming `param`), when that is the request's shape at fault.
    pub(crate) fn adjust(
        &self,
        message: &str,
        param: Option<&str>,
        sent_max: Option<u64>,
    ) -> Option<Self> {
        let lower = message.to_ascii_lowercase();
        let refused = [
            "unsupported",
            "not supported",
            "unrecognized",
            "unknown",
            "not allowed",
            "not permitted",
            "extra inputs",
            "does not support",
            "invalid",
        ]
        .iter()
        .any(|w| lower.contains(w));
        let mut next = self.clone();

        let names_max = param.is_some_and(|p| p.starts_with("max_"))
            || ["max_tokens", "max_completion_tokens", "max_output_tokens"]
                .iter()
                .any(|f| lower.contains(f));
        if names_max {
            if !self.completion_field
                && lower.contains("max_completion_tokens")
                && (refused || lower.contains("instead"))
            {
                next.completion_field = true;
                return Some(next);
            }
            // "max_tokens is too large: 32000. This model supports at most
            // 16384", "the valid range of max_tokens is [1, 8192]".
            let sent = sent_max?;
            let cap = numbers(&lower)
                .into_iter()
                .filter(|&n| (256..sent).contains(&n))
                .max()?;
            if self.max_tokens.is_some_and(|m| m <= cap) {
                return None;
            }
            next.max_tokens = Some(cap);
            return Some(next);
        }

        if !refused {
            return None;
        }
        let named = |p: &str| {
            param == Some(p)
                || [format!("'{p}'"), format!("\"{p}\""), format!("`{p}`")]
                    .iter()
                    .any(|q| lower.contains(q.as_str()))
                || (p.contains('_') && lower.contains(p))
        };
        let p = OPTIONAL
            .iter()
            .find(|p| named(p) && !self.dropped.contains(p))?;
        next.dropped.push(p);
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reasoning_models_get_max_completion_tokens() {
        let s = Shape::default();
        let next = s.adjust(
            "Unsupported parameter: 'max_tokens' is not supported with this model. Use 'max_completion_tokens' instead.",
            Some("max_tokens"),
            Some(32000),
        );
        assert_eq!(
            next,
            Some(Shape {
                completion_field: true,
                ..Shape::default()
            })
        );
        let mut body = Map::new();
        if let Some(n) = next {
            n.apply(&mut body, Some(32000));
        }
        assert_eq!(
            Value::Object(body),
            json!({ "max_completion_tokens": 32000 })
        );
    }

    #[test]
    fn an_output_cap_is_learned() {
        let s = Shape::default();
        let next = s.adjust(
            "max_tokens is too large: 32000. This model supports at most 16384 completion tokens, whereas you provided 32000.",
            Some("max_tokens"),
            Some(32000),
        );
        assert_eq!(next.as_ref().and_then(|n| n.max_tokens), Some(16384));
        let deepseek = s.adjust(
            "Invalid max_tokens value, the valid range of max_tokens is [1, 8192]",
            None,
            Some(32000),
        );
        assert_eq!(deepseek.and_then(|n| n.max_tokens), Some(8192));
        // Already within it: nothing more to learn.
        let capped = Shape {
            max_tokens: Some(8192),
            ..Shape::default()
        };
        assert_eq!(
            capped.adjust("max_tokens: valid range [1, 8192]", None, Some(8192)),
            None
        );
    }

    #[test]
    fn refused_parameters_are_dropped_one_at_a_time() {
        let s = Shape::default();
        let t = s.adjust(
            "Unsupported value: 'temperature' does not support 0.2 with this model.",
            Some("temperature"),
            None,
        );
        assert_eq!(
            t.as_ref().map(|t| t.dropped.clone()),
            Some(vec!["temperature"])
        );
        let so = s.adjust("Extra inputs are not permitted: stream_options", None, None);
        assert_eq!(so.map(|t| t.dropped), Some(vec!["stream_options"]));
        assert_eq!(s.adjust("The model `x` does not exist", None, None), None);
        assert_eq!(s.adjust("Invalid API key", None, None), None);
    }
}
