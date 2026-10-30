//! Claude Code's Messages API in front of an OpenAI-compatible API (ADR 0023).
//!
//! Berth's agents are Claude Code, always. This lets them run on a model
//! behind any API that speaks OpenAI's Chat Completions: OpenAI, OpenRouter,
//! DeepSeek, Groq, a local Ollama, LM Studio or vLLM. Claude Code sends its
//! requests to a local server (`ANTHROPIC_BASE_URL`); each goes upstream
//! translated, and its answer, streamed or not, comes back in the Messages
//! API's shape. The provider's key never leaves this process: Claude Code
//! holds only a token for the local server, new with each launch.

mod errors;
mod models;
mod request;
mod response;
mod server;
mod shape;
mod stream;

pub use models::{CatalogModel, list_models};
pub use server::Proxy;

use std::fmt;

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
}

impl fmt::Debug for Upstream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Upstream")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "…"))
            .field("model", &self.model)
            .field("fast_model", &self.fast_model)
            .field("models", &self.models)
            .finish()
    }
}

impl Upstream {
    /// `path` (`/chat/completions`) under the base URL.
    #[must_use]
    pub fn endpoint(&self, path: &str) -> String {
        let base = self.base_url.trim().trim_end_matches('/');
        let after_scheme = base.split_once("://").map_or(base, |(_, rest)| rest);
        if after_scheme.contains('/') {
            format!("{base}{path}")
        } else {
            format!("{base}/v1{path}")
        }
    }

    /// The host requests go to, as messages name it: `openrouter.ai`.
    #[must_use]
    pub fn host(&self) -> String {
        host_of(&self.base_url)
    }

    /// The upstream model a request for `requested` runs on. Claude Code's
    /// own names (a session started before, a subagent's `haiku`) map to the
    /// configured models; anything else is the provider's id, as it is.
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
        if !claude {
            return id.to_owned();
        }
        match &self.fast_model {
            Some(fast) if lower.contains("haiku") => fast.clone(),
            _ => self.model.clone(),
        }
    }
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
