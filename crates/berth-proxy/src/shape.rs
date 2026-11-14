//! What each model takes, learned from what it refuses.
//!
//! Providers differ in small ways: OpenAI's reasoning models want
//! `max_completion_tokens` and only the default temperature, many models
//! cap their output below the 32k Claude Code asks for, some servers know no
//! `stream_options`, some models reason and some don't, some take only one
//! of OpenAI's APIs (Codex the Responses API, most of Azure's other models
//! Chat Completions). A refusal that names one of these is answered by
//! sending the request again without it, and the model's shape is kept for
//! its next requests (until Berth quits) once a request in it went through.

use serde_json::{Map, Value, json};

use crate::errors::numbers;
use crate::{Api, Effort};

/// Parameters a request can go without.
const OPTIONAL: [&str; 7] = [
    "temperature",
    "top_p",
    "stop",
    "parallel_tool_calls",
    "stream_options",
    "tool_choice",
    "store",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Shape {
    /// `max_completion_tokens` in place of `max_tokens`.
    pub completion_field: bool,
    /// The most output the model takes.
    pub max_tokens: Option<u64>,
    /// Parameters it refuses.
    pub dropped: Vec<&'static str>,
    /// The API it is spoken to in, when it takes only the other one.
    pub api: Option<Api>,
    /// It takes no reasoning effort (it doesn't reason, or not on request).
    pub no_effort: bool,
    /// The efforts it takes, once it refused one (`xhigh`).
    pub efforts: Option<Vec<Effort>>,
    /// It gives no reasoning summaries or encrypted reasoning (Responses API).
    pub no_reasoning_out: bool,
    /// Summaries alone are refused (an organization not verified for them).
    pub no_summary: bool,
    /// Its reasoning can't go back to it (it can't be verified, or the
    /// server takes no `reasoning_content`).
    pub no_reasoning_back: bool,
    /// It reads no images: they go as a note.
    pub no_images: bool,
    /// The longest tool description it takes (Azure's 1024): longer ones
    /// are cut, and go whole in the system prompt.
    pub description_max: Option<usize>,
}

impl Shape {
    /// The request in this shape: its output budget `max_tokens` (within the
    /// model's cap) in the field `api` names for it, the reasoning `effort`
    /// the model takes, and, from the Responses API, its reasoning to keep.
    pub(crate) fn apply(
        &self,
        body: &mut Map<String, Value>,
        max_tokens: Option<u64>,
        api: Api,
        effort: Option<Effort>,
    ) {
        for p in &self.dropped {
            body.remove(*p);
        }
        if let Some(k) = max_tokens {
            let k = self.max_tokens.map_or(k, |cap| k.min(cap));
            let field = match api {
                Api::Responses => "max_output_tokens",
                Api::ChatCompletions if self.completion_field => "max_completion_tokens",
                Api::ChatCompletions => "max_tokens",
            };
            body.insert(field.into(), k.into());
        }
        let effort = effort.and_then(|e| self.effort(e));
        match api {
            Api::Responses => {
                let mut reasoning = Map::new();
                if let Some(e) = effort {
                    reasoning.insert("effort".into(), e.as_str().into());
                }
                if !self.no_reasoning_out {
                    if !self.no_summary {
                        reasoning.insert("summary".into(), "auto".into());
                    }
                    body.insert("include".into(), json!(["reasoning.encrypted_content"]));
                }
                if !reasoning.is_empty() {
                    body.insert("reasoning".into(), Value::Object(reasoning));
                }
            }
            Api::ChatCompletions => {
                if let Some(e) = effort {
                    body.insert("reasoning_effort".into(), e.as_str().into());
                }
            }
        }
    }

    /// The effort the model takes for `wanted`: the most it takes up to
    /// that, else the least it takes.
    pub(crate) fn effort(&self, wanted: Effort) -> Option<Effort> {
        if self.no_effort {
            return None;
        }
        let Some(takes) = &self.efforts else {
            return Some(wanted);
        };
        takes
            .iter()
            .copied()
            .filter(|e| *e <= wanted)
            .max()
            .or_else(|| takes.iter().copied().min())
    }

    /// The shape to try after the model refused a request in `api` with
    /// `message` (naming `param`), when that is the request's shape at fault.
    pub(crate) fn adjust(
        &self,
        message: &str,
        param: Option<&str>,
        sent_max: Option<u64>,
        api: Api,
    ) -> Option<Self> {
        let lower = message.to_ascii_lowercase();
        let param = param.unwrap_or("").to_ascii_lowercase();
        let refused = [
            "unsupported",
            "not supported",
            "unrecognized",
            "unknown",
            "not allowed",
            "not permitted",
            "extra inputs",
            "does not support",
            "does not work",
            "invalid",
            "unexpected",
        ]
        .iter()
        .any(|w| lower.contains(w));
        let mut next = self.clone();

        // A model that takes only the other API: "This model is only
        // supported in v1/responses", Azure's "The chatCompletion operation
        // does not work with the specified model".
        let other = match api {
            Api::ChatCompletions => Api::Responses,
            Api::Responses => Api::ChatCompletions,
        };
        let only_other = match api {
            Api::ChatCompletions => {
                lower.contains("v1/responses")
                    || (lower.contains("responses api") && lower.contains("only"))
                    || lower.contains("chatcompletion operation does not work")
            }
            Api::Responses => {
                (lower.contains("responses api") || lower.contains("responses operation"))
                    && refused
            }
        };
        if only_other && self.api.is_none() {
            next.api = Some(other);
            return Some(next);
        }

        // Reasoning it can't take back: Berth's copy of it can't be verified
        // (another resource's key), or the server takes no `reasoning_content`.
        let reasoning_back = lower.contains("could not be verified")
            || lower.contains("required following item")
            || (lower.contains("reasoning_content") && !lower.contains("missing"));
        if reasoning_back && !self.no_reasoning_back {
            next.no_reasoning_back = true;
            return Some(next);
        }
        // The effort: a value it doesn't take ("Supported values are: 'low',
        // 'medium', and 'high'"), or none at all.
        let names_effort = param.contains("reasoning_effort")
            || param == "reasoning.effort"
            || lower.contains("reasoning_effort")
            || lower.contains("reasoning.effort");
        if names_effort {
            let listed = [
                "supported values",
                "one of",
                "valid values",
                "allowed values",
            ]
            .iter()
            .find_map(|p| lower.find(p).map(|at| &lower[at..]))
            .map(|tail| {
                Effort::ALL
                    .into_iter()
                    .filter(|e| {
                        [format!("'{}'", e.as_str()), format!("\"{}\"", e.as_str())]
                            .iter()
                            .any(|q| tail.contains(q.as_str()))
                            || tail
                                .split(|c: char| !c.is_ascii_alphanumeric())
                                .any(|w| w == e.as_str())
                    })
                    .collect::<Vec<_>>()
            });
            match listed {
                Some(takes) if !takes.is_empty() && self.efforts.as_ref() != Some(&takes) => {
                    next.efforts = Some(takes);
                }
                _ => next.no_effort = true,
            }
            return (next != *self).then_some(next);
        }
        // Summaries, or the encrypted reasoning, or reasoning at all.
        if param == "reasoning.summary" || lower.contains("reasoning summar") {
            next.no_summary = true;
            return (next != *self).then_some(next);
        }
        if param == "include"
            || param.starts_with("reasoning")
            || lower.contains("encrypted content")
            || lower.contains("encrypted_content")
            || (lower.contains("reasoning") && refused)
        {
            next.no_reasoning_out = true;
            if param.starts_with("reasoning") || lower.contains("reasoning is not") {
                next.no_effort = true;
            }
            return (next != *self).then_some(next);
        }
        // "Invalid 'tools[0].function.description': string too long.
        // Expected a string with maximum length 1024".
        if lower.contains("description")
            && (lower.contains("too long") || lower.contains("maximum length"))
        {
            let max = ["maximum length", "at most"]
                .iter()
                .find_map(|p| {
                    let at = lower.find(p)? + p.len();
                    numbers(&lower[at..]).into_iter().next()
                })
                .and_then(|n| usize::try_from(n).ok())
                .filter(|&n| n >= 64)?;
            if self.description_max.is_some_and(|m| m <= max) {
                return None;
            }
            next.description_max = Some(max);
            return Some(next);
        }
        // Images, for a model without vision.
        if (lower.contains("image") || param.contains("image"))
            && (refused || lower.contains("vision"))
            && !self.no_images
        {
            next.no_images = true;
            return Some(next);
        }

        let names_max = param.starts_with("max_")
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
            param == p
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

    const CHAT: Api = Api::ChatCompletions;
    const RESPONSES: Api = Api::Responses;

    #[test]
    fn reasoning_models_get_max_completion_tokens() {
        let s = Shape::default();
        let next = s.adjust(
            "Unsupported parameter: 'max_tokens' is not supported with this model. Use 'max_completion_tokens' instead.",
            Some("max_tokens"),
            Some(32000),
            CHAT,
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
            n.apply(&mut body, Some(32000), CHAT, None);
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
            CHAT,
        );
        assert_eq!(next.as_ref().and_then(|n| n.max_tokens), Some(16384));
        let deepseek = s.adjust(
            "Invalid max_tokens value, the valid range of max_tokens is [1, 8192]",
            None,
            Some(32000),
            CHAT,
        );
        assert_eq!(deepseek.and_then(|n| n.max_tokens), Some(8192));
        // Already within it: nothing more to learn.
        let capped = Shape {
            max_tokens: Some(8192),
            ..Shape::default()
        };
        assert_eq!(
            capped.adjust("max_tokens: valid range [1, 8192]", None, Some(8192), CHAT),
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
            CHAT,
        );
        assert_eq!(
            t.as_ref().map(|t| t.dropped.clone()),
            Some(vec!["temperature"])
        );
        let so = s.adjust(
            "Extra inputs are not permitted: stream_options",
            None,
            None,
            CHAT,
        );
        assert_eq!(so.map(|t| t.dropped), Some(vec!["stream_options"]));
        assert_eq!(
            s.adjust("The model `x` does not exist", None, None, CHAT),
            None
        );
        assert_eq!(s.adjust("Invalid API key", None, None, CHAT), None);
    }

    #[test]
    fn a_model_on_the_other_api_moves_there() {
        let s = Shape::default();
        let codex = s.adjust(
            "This model is only supported in v1/responses and not in v1/chat/completions.",
            Some("model"),
            None,
            CHAT,
        );
        assert_eq!(codex.and_then(|n| n.api), Some(RESPONSES));
        let azure = s.adjust(
            "The chatCompletion operation does not work with the specified model, gpt-5-codex. Please choose different model and try again.",
            None,
            None,
            CHAT,
        );
        assert_eq!(azure.and_then(|n| n.api), Some(RESPONSES));
        let chat_only = s.adjust(
            "The model 'DeepSeek-R1' is not supported with the Responses API.",
            Some("model"),
            None,
            RESPONSES,
        );
        assert_eq!(chat_only.and_then(|n| n.api), Some(CHAT));
    }

    #[test]
    fn efforts_a_model_takes_are_learned() {
        let s = Shape::default();
        let xhigh = s.adjust(
            "Unsupported value: 'xhigh' is not supported with the 'gpt-5' model. Supported values are: 'minimal', 'low', 'medium', and 'high'.",
            Some("reasoning.effort"),
            None,
            RESPONSES,
        );
        let xhigh = xhigh.unwrap_or_default();
        assert_eq!(
            xhigh.efforts,
            Some(vec![Effort::Low, Effort::Medium, Effort::High])
        );
        assert_eq!(xhigh.effort(Effort::XHigh), Some(Effort::High));
        assert_eq!(xhigh.effort(Effort::Low), Some(Effort::Low));
        let none = s
            .adjust(
                "Unsupported parameter: 'reasoning_effort' is not supported with this model.",
                Some("reasoning_effort"),
                None,
                CHAT,
            )
            .unwrap_or_default();
        assert!(none.no_effort);
        assert_eq!(none.effort(Effort::High), None);
        let mut body = Map::new();
        none.apply(&mut body, None, CHAT, Some(Effort::High));
        assert!(body.is_empty());
    }

    #[test]
    fn reasoning_is_kept_from_the_responses_api_until_refused() {
        let s = Shape::default();
        let mut body = Map::new();
        s.apply(&mut body, Some(1000), RESPONSES, Some(Effort::Medium));
        assert_eq!(
            Value::Object(body),
            json!({
                "max_output_tokens": 1000,
                "reasoning": { "effort": "medium", "summary": "auto" },
                "include": ["reasoning.encrypted_content"],
            })
        );
        let plain = s
            .adjust(
                "Encrypted content is not supported with this model.",
                Some("include"),
                None,
                RESPONSES,
            )
            .unwrap_or_default();
        assert!(plain.no_reasoning_out && !plain.no_effort);
        let mut body = Map::new();
        plain.apply(&mut body, None, RESPONSES, None);
        assert!(body.is_empty());
        let unverified = s
            .adjust(
                "Your organization must be verified to generate reasoning summaries.",
                Some("reasoning.summary"),
                None,
                RESPONSES,
            )
            .unwrap_or_default();
        assert!(unverified.no_summary && !unverified.no_reasoning_out);
        let lost = s
            .adjust(
                "The encrypted content for item rs_1 could not be verified.",
                None,
                None,
                RESPONSES,
            )
            .unwrap_or_default();
        assert!(lost.no_reasoning_back);
    }

    #[test]
    fn a_cap_on_tool_descriptions_is_learned() {
        let next = Shape::default().adjust(
            "Invalid 'tools[0].function.description': string too long. Expected a string with maximum length 1024, but got a string with length 9178 instead.",
            Some("tools[0].function.description"),
            Some(32000),
            CHAT,
        );
        assert_eq!(next.and_then(|n| n.description_max), Some(1024));
    }

    #[test]
    fn a_model_without_vision_gets_no_images() {
        let s = Shape::default();
        let next = s.adjust(
            "Invalid content type. image_url is only supported by certain models.",
            Some("messages.[1].content.[1].type"),
            None,
            CHAT,
        );
        assert!(next.is_some_and(|n| n.no_images));
    }
}
