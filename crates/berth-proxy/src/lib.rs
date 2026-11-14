//! Claude Code's Messages API in front of an OpenAI-compatible API (ADR 0023).
//!
//! Berth's agents are Claude Code, always. This lets them run on a model
//! behind an OpenAI-compatible API, speaking OpenAI's Responses API or Chat
//! Completions ([`Api`]): OpenAI, OpenRouter, DeepSeek, Groq, a local
//! Ollama, LM Studio or vLLM. Claude Code sends its requests to a local
//! server (`ANTHROPIC_BASE_URL`); each goes upstream translated, and its
//! answer, streamed or not, comes back in the Messages API's shape. The
//! provider's key never leaves this process: Claude Code holds only a token
//! for the local server, new with each launch (ADR 0023, ADR 0025).

mod errors;
mod models;
mod reasoning;
mod request;
mod response;
mod server;
mod shape;
mod stream;

pub use models::{CatalogModel, list_models};
pub use server::{Proxy, SessionPrefs};

use std::collections::HashMap;
use std::fmt;

/// Which of OpenAI's APIs the provider speaks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Api {
    /// `/responses`: OpenAI's current API, and the only one some models
    /// (Codex) take.
    #[default]
    Responses,
    /// `/chat/completions`: what most compatible servers speak.
    ChatCompletions,
}

impl Api {
    /// The endpoint requests go to, under the base URL.
    #[must_use]
    pub fn path(self) -> &'static str {
        match self {
            Self::Responses => "/responses",
            Self::ChatCompletions => "/chat/completions",
        }
    }
}

/// How hard a reasoning model thinks: Berth's effort, as OpenAI's
/// `reasoning.effort` takes it (ADR 0037). A model that takes less, or none,
/// is learned from what it refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Effort {
    Low,
    Medium,
    High,
    /// OpenAI's `xhigh`; Berth's Max too.
    XHigh,
}

impl Effort {
    pub const ALL: [Self; 4] = [Self::Low, Self::Medium, Self::High, Self::XHigh];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.as_str() == s)
    }
}

/// The OpenAI-compatible API agents run on, and the models they ask for.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Upstream {
    /// Where `/chat/completions` and `/models` are: `https://api.openai.com/v1`.
    /// A URL with no path gets `/v1`, as most servers have it.
    pub base_url: String,
    /// Sent as `Authorization: Bearer …`; local servers often take none.
    pub api_key: Option<String>,
    /// What Claude Code's own model names (`claude-opus-…`, `sonnet`) run on.
    pub model: String,
    /// What its Haiku-class requests run on (titles, summaries), else `model`.
    pub fast_model: Option<String>,
    /// The models on offer, for Claude Code's model listing.
    pub models: Vec<String>,
    pub api: Api,
    /// Each model's context window, where known: a request's output budget
    /// is cut to what the window leaves it, and a full window reads as
    /// "prompt is too long" with its size.
    pub contexts: HashMap<String, u64>,
}

impl fmt::Debug for Upstream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Upstream")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "…"))
            .field("model", &self.model)
            .field("fast_model", &self.fast_model)
            .field("models", &self.models)
            .field("api", &self.api)
            .field("contexts", &self.contexts)
            .finish()
    }
}

impl Upstream {
    /// `path` (`/responses`) under the base URL, before its query
    /// (`?api-version=…`), which stays. An Azure OpenAI resource's URL, as
    /// the portal gives it (the resource, a deployment's "Target URI"),
    /// becomes its v1 API: `https://x.openai.azure.com/openai/v1`.
    #[must_use]
    pub fn endpoint(&self, path: &str) -> String {
        let url = self.base_url.trim();
        let (url, query) = url
            .split_once('?')
            .map_or((url, None), |(u, q)| (u, Some(q)));
        let url = url.trim_end_matches('/');
        let (scheme, rest) = url.split_once("://").unwrap_or(("https", url));
        let (host, base_path) = rest.split_once('/').map_or((rest, ""), |(h, p)| (h, p));
        let base_path = base_path.trim_end_matches('/');
        if is_azure_openai(host) {
            // Everything from `/openai` on is the API's; v1 takes no api-version.
            let segments: Vec<&str> = base_path.split('/').filter(|s| !s.is_empty()).collect();
            let at = segments.iter().position(|s| *s == "openai");
            if segments.is_empty() || at.is_some() {
                let prefix: String = segments[..at.unwrap_or(0)]
                    .iter()
                    .map(|s| format!("/{s}"))
                    .collect();
                return format!("{scheme}://{host}{prefix}/openai/v1{path}");
            }
        }
        let base_path = if base_path.is_empty() {
            "/v1".to_owned()
        } else {
            format!("/{base_path}")
        };
        match query.filter(|q| !q.is_empty()) {
            Some(q) => format!("{scheme}://{host}{base_path}{path}?{q}"),
            None => format!("{scheme}://{host}{base_path}{path}"),
        }
    }

    /// Azure's servers take the key as `api-key`, its classic APIs only so.
    #[must_use]
    pub fn is_azure(&self) -> bool {
        let host = self.host();
        let host = host.split(':').next().unwrap_or(&host).to_ascii_lowercase();
        [".azure.com", ".azure.us", ".azure.cn", ".azure-api.net"]
            .iter()
            .any(|d| host.ends_with(d))
    }

    /// The request with the API key: as a bearer token, and to Azure as
    /// `api-key` too.
    pub(crate) fn authorize(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let Some(key) = self.api_key.as_deref().filter(|k| !k.is_empty()) else {
            return req;
        };
        let req = req.bearer_auth(key);
        if self.is_azure() {
            req.header("api-key", key)
        } else {
            req
        }
    }

    /// The model's context window, where known.
    #[must_use]
    pub fn window(&self, model: &str) -> Option<u64> {
        self.contexts.get(model).copied().filter(|&w| w > 0)
    }

    /// The host requests go to, as messages name it: `openrouter.ai`.
    #[must_use]
    pub fn host(&self) -> String {
        host_of(&self.base_url)
    }

    /// The upstream model a request for `requested` runs on. Claude Code's
    /// own names (a session started before, a subagent's `haiku`) map to the
    /// configured models, and so does a model no longer on offer (a session
    /// started before it was taken out of Settings); one on offer is the
    /// provider's id, as it is.
    #[must_use]
    pub fn resolve(&self, requested: &str) -> String {
        let id = requested.trim();
        let id = id.strip_suffix("[1m]").unwrap_or(id);
        let lower = id.to_ascii_lowercase();
        let claude = lower.starts_with("claude-")
            || matches!(
                lower.as_str(),
                "" | "default" | "best" | "opus" | "sonnet" | "haiku" | "fable" | "opusplan"
            );
        let offered = self.models.is_empty() || self.models.iter().any(|m| m == id);
        if !claude && offered {
            return id.to_owned();
        }
        match &self.fast_model {
            Some(fast) if lower.contains("haiku") => fast.clone(),
            _ => self.model.clone(),
        }
    }
}

/// An Azure OpenAI resource (or Azure AI Foundry's), whose OpenAI API is
/// under `/openai/v1`.
fn is_azure_openai(host: &str) -> bool {
    let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    [
        ".openai.azure.com",
        ".cognitiveservices.azure.com",
        ".services.ai.azure.com",
        ".openai.azure.us",
        ".openai.azure.cn",
    ]
    .iter()
    .any(|d| host.ends_with(d))
}

/// `openrouter.ai` for `https://openrouter.ai/api/v1`.
#[must_use]
pub fn host_of(url: &str) -> String {
    let rest = url.trim().split_once("://").map_or(url.trim(), |(_, r)| r);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .rsplit('@')
        .next()
        .unwrap_or(rest)
        .to_owned()
}

/// What went wrong talking to the provider outside a request from Claude
/// Code (listing its models).
#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("Couldn't reach {host}: {detail}")]
    Unreachable { host: String, detail: String },
    #[error("{host} answered {status}: {message}")]
    Status {
        host: String,
        status: u16,
        message: String,
    },
    #[error("{host} sent something that isn't a model list")]
    Invalid { host: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upstream(base: &str) -> Upstream {
        Upstream {
            base_url: base.to_owned(),
            model: "deepseek-chat".to_owned(),
            fast_model: Some("small".to_owned()),
            ..Upstream::default()
        }
    }

    #[test]
    fn endpoints_keep_a_path_and_add_v1_to_a_bare_host() {
        assert_eq!(
            upstream("https://openrouter.ai/api/v1/").endpoint("/chat/completions"),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        assert_eq!(
            upstream("http://localhost:11434").endpoint("/models"),
            "http://localhost:11434/v1/models"
        );
        assert_eq!(
            upstream("https://openrouter.ai/api/v1").host(),
            "openrouter.ai"
        );
        assert_eq!(host_of("http://user@127.0.0.1:8000/v1"), "127.0.0.1:8000");
    }

    #[test]
    fn azure_urls_as_the_portal_gives_them_become_its_v1_api() {
        for url in [
            "https://contoso.openai.azure.com",
            "https://contoso.openai.azure.com/",
            "https://contoso.openai.azure.com/openai",
            "https://contoso.openai.azure.com/openai/v1/",
            "https://contoso.openai.azure.com/openai/deployments/gpt-5/chat/completions?api-version=2025-01-01-preview",
            "https://contoso.openai.azure.com/openai/responses?api-version=2025-04-01-preview",
        ] {
            assert_eq!(
                upstream(url).endpoint("/responses"),
                "https://contoso.openai.azure.com/openai/v1/responses",
                "{url}"
            );
        }
        assert_eq!(
            upstream("https://contoso.cognitiveservices.azure.com/openai/v1").endpoint("/models"),
            "https://contoso.cognitiveservices.azure.com/openai/v1/models"
        );
        // Another Azure API, and any other server, keep their path and query.
        assert_eq!(
            upstream("https://contoso.services.ai.azure.com/models?api-version=2024-05-01-preview")
                .endpoint("/chat/completions"),
            "https://contoso.services.ai.azure.com/models/chat/completions?api-version=2024-05-01-preview"
        );
        assert_eq!(
            upstream("https://gw.example.com/llm?key=1").endpoint("/chat/completions"),
            "https://gw.example.com/llm/chat/completions?key=1"
        );
        assert!(upstream("https://contoso.openai.azure.com").is_azure());
        assert!(upstream("https://apim.azure-api.net/openai").is_azure());
        assert!(!upstream("https://api.openai.com/v1").is_azure());
    }

    #[test]
    fn azure_gets_the_key_as_api_key_too() {
        let client = reqwest::Client::new();
        let header = |base: &str| {
            let u = Upstream {
                api_key: Some("k".to_owned()),
                ..upstream(base)
            };
            u.authorize(client.post(u.endpoint("/responses")))
                .build()
                .ok()
                .map(|r| {
                    (
                        r.headers().get("api-key").is_some(),
                        r.headers().get("authorization").is_some(),
                    )
                })
        };
        assert_eq!(
            header("https://contoso.openai.azure.com"),
            Some((true, true))
        );
        assert_eq!(header("https://api.openai.com/v1"), Some((false, true)));
    }

    #[test]
    fn a_model_no_longer_on_offer_runs_on_the_default() {
        let u = Upstream {
            models: vec!["deepseek-chat".into(), "small".into()],
            ..upstream("https://api.deepseek.com")
        };
        assert_eq!(u.resolve("small"), "small");
        assert_eq!(u.resolve("removed-model"), "deepseek-chat");
    }

    #[test]
    fn claude_names_map_to_the_configured_models() {
        let u = upstream("https://api.deepseek.com");
        assert_eq!(u.resolve("claude-opus-5-5"), "deepseek-chat");
        assert_eq!(u.resolve("claude-sonnet-5[1m]"), "deepseek-chat");
        assert_eq!(u.resolve("claude-haiku-5-5"), "small");
        assert_eq!(u.resolve("haiku"), "small");
        assert_eq!(u.resolve("qwen/qwen3-coder"), "qwen/qwen3-coder");
        let no_fast = Upstream {
            fast_model: None,
            ..u
        };
        assert_eq!(no_fast.resolve("claude-haiku-5-5"), "deepseek-chat");
    }

    #[test]
    fn debug_never_shows_the_key() {
        let u = Upstream {
            api_key: Some("sk-secret".to_owned()),
            ..upstream("https://api.openai.com/v1")
        };
        assert!(!format!("{u:?}").contains("sk-secret"));
    }
}
