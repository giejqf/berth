//! Where agents on this Mac send their model requests (ADR 0023): Claude
//! Code's own account, or an OpenAI-compatible API. On that API the agent
//! is Claude Code through Berth's proxy, or Pi, which speaks it itself
//! (ADR 0033).
//!
//! Settings: `provider` ("openai", else Claude Code's account),
//! `openai_agent` ("pi", else Claude Code),
//! `openai_base_url`, `openai_api` ("chat" for Chat Completions, else the
//! Responses API), `openai_models` (the ids on offer, JSON), `openai_model`
//! (the default), `openai_fast_model` (Claude Code's quick requests),
//! `openai_contexts` (context windows set by hand, JSON), `openai_catalog`
//! (what the provider listed at its last check, JSON). The key is in the
//! Keychain ([`KEY_SERVICE`]).

use std::collections::BTreeMap;

use berth_protocol::Agent;
use serde::{Deserialize, Serialize};

use crate::{Result, Store};

/// The Keychain item holding the API key: service, account.
pub const KEY_SERVICE: (&str, &str) = ("com.berth.provider.openai", "api_key");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProviderKind {
    /// Claude Code's own sign-in (ADR 0017).
    #[default]
    Claude,
    /// An OpenAI-compatible API, through the proxy.
    OpenAi,
}

/// Which of OpenAI's APIs the provider is spoken to in (ADR 0025).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProviderApi {
    /// `/responses`.
    #[default]
    Responses,
    /// `/chat/completions`.
    ChatCompletions,
}

/// One model the provider listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Its context window, for providers that say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Provider {
    pub kind: ProviderKind,
    /// The agent new conversations run on under the OpenAI-compatible API
    /// (ADR 0033); Claude Code on Claude Code's account, whatever this says.
    pub agent: Agent,
    pub base_url: String,
    pub api: ProviderApi,
    /// The models on offer, in the order they were added.
    pub models: Vec<String>,
    /// The default for slots without their own; else the first on offer.
    pub model: Option<String>,
    /// What Claude Code's quick requests (titles, summaries) run on; else
    /// the agent's own model.
    pub fast_model: Option<String>,
    /// Context windows set by hand, by model: over what the provider lists.
    pub contexts: BTreeMap<String, u64>,
    pub catalog: Vec<ProviderModel>,
}

impl Provider {
    /// # Errors
    /// The settings can't be read.
    pub fn load(store: &Store) -> Result<Self> {
        let text = |k: &str| -> Result<Option<String>> {
            Ok(store.setting(k)?.filter(|v| !v.trim().is_empty()))
        };
        let kind = match text("provider")?.as_deref() {
            Some("openai") => ProviderKind::OpenAi,
            _ => ProviderKind::Claude,
        };
        let models = text("openai_models")?
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default();
        let catalog = text("openai_catalog")?
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default();
        let contexts = text("openai_contexts")?
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default();
        let api = match text("openai_api")?.as_deref() {
            Some("chat") => ProviderApi::ChatCompletions,
            _ => ProviderApi::Responses,
        };
        Ok(Self {
            kind,
            agent: Agent::parse(text("openai_agent")?.as_deref().unwrap_or_default()),
            base_url: text("openai_base_url")?.unwrap_or_default(),
            api,
            contexts,
            models,
            model: text("openai_model")?,
            fast_model: text("openai_fast_model")?,
            catalog,
        })
    }

    /// # Errors
    /// The settings can't be written.
    pub fn save(&self, store: &Store) -> Result<()> {
        let put = |k: &str, v: Option<&str>| match v.filter(|v| !v.is_empty()) {
            Some(v) => store.set_setting(k, v),
            None => store.delete_setting(k),
        };
        let kind = (self.kind == ProviderKind::OpenAi).then_some("openai");
        put("provider", kind)?;
        put("openai_agent", (self.agent == Agent::Pi).then_some("pi"))?;
        put("openai_base_url", Some(self.base_url.trim()))?;
        let models = serde_json::to_string(&self.models).unwrap_or_default();
        put(
            "openai_models",
            (!self.models.is_empty()).then_some(&*models),
        )?;
        put("openai_model", self.model.as_deref())?;
        put("openai_fast_model", self.fast_model.as_deref())?;
        let api = (self.api == ProviderApi::ChatCompletions).then_some("chat");
        put("openai_api", api)?;
        let contexts = serde_json::to_string(&self.contexts).unwrap_or_default();
        put(
            "openai_contexts",
            (!self.contexts.is_empty()).then_some(&*contexts),
        )?;
        let catalog = serde_json::to_string(&self.catalog).unwrap_or_default();
        put(
            "openai_catalog",
            (!self.catalog.is_empty()).then_some(&*catalog),
        )
    }

    #[must_use]
    pub fn is_openai(&self) -> bool {
        self.kind == ProviderKind::OpenAi
    }

    /// The agent a new conversation starts on in a slot on this Mac
    /// (`host`); an `inside` slot's is always Claude Code (ADR 0033).
    #[must_use]
    pub fn new_agent(&self, host: bool) -> Agent {
        if host && self.is_openai() {
            self.agent
        } else {
            Agent::Claude
        }
    }

    /// The model slots without their own run on.
    #[must_use]
    pub fn default_model(&self) -> Option<&str> {
        self.model
            .as_deref()
            .filter(|m| self.offers(m))
            .or_else(|| self.models.first().map(String::as_str))
    }

    #[must_use]
    pub fn offers(&self, id: &str) -> bool {
        self.models.iter().any(|m| m == id)
    }

    /// Which of a slot's own model applies now. A slot keeps the model it
    /// was given under either provider; under the other it runs on that
    /// one's default instead.
    #[must_use]
    pub fn slot_model<'a>(&self, slot_model: Option<&'a str>) -> Option<&'a str> {
        self.slot_model_on(slot_model, self.is_openai())
    }

    /// [`Self::slot_model`] for an agent that does or doesn't run through the
    /// proxy (an `inside` slot's never does).
    #[must_use]
    pub fn slot_model_on<'a>(
        &self,
        slot_model: Option<&'a str>,
        through_proxy: bool,
    ) -> Option<&'a str> {
        let m = slot_model?;
        (self.offers(m) == through_proxy).then_some(m)
    }

    /// The model a session on this Mac runs on through the proxy: the
    /// slot's when it is on offer, else the default.
    #[must_use]
    pub fn agent_model(&self, slot_model: Option<&str>) -> Option<String> {
        self.slot_model_on(slot_model, true)
            .or_else(|| self.default_model())
            .map(str::to_owned)
    }

    /// Why agents on this Mac can't run on the provider yet.
    #[must_use]
    pub fn not_ready(&self) -> Option<&'static str> {
        if !self.is_openai() {
            None
        } else if self.base_url.trim().is_empty() {
            Some("The OpenAI-compatible API has no URL yet, so agents can't run.")
        } else if self.models.is_empty() {
            Some("No model is chosen for the OpenAI-compatible API, so agents can't run.")
        } else {
            None
        }
    }

    /// `model`'s context window: as set by hand, else as the provider lists it.
    #[must_use]
    pub fn context_of(&self, model: &str) -> Option<u64> {
        self.contexts
            .get(model)
            .copied()
            .or_else(|| self.listed_context(model))
    }

    /// The context window the provider lists for `model`.
    #[must_use]
    pub fn listed_context(&self, model: &str) -> Option<u64> {
        self.catalog
            .iter()
            .find(|m| m.id == model)
            .and_then(|m| m.context)
    }

    /// What the provider calls `model`, else its id.
    #[must_use]
    pub fn name_of(&self, model: &str) -> String {
        self.catalog
            .iter()
            .find(|m| m.id == model)
            .and_then(|m| m.name.clone())
            .unwrap_or_else(|| model.to_owned())
    }
}

/// A context window as people write it: "128000", "128k", "128K", "1m",
/// "1.5M", "131,072". `None` for anything else, or nothing.
#[must_use]
pub fn parse_tokens(text: &str) -> Option<u64> {
    let t = text
        .trim()
        .replace([',', '_', ' '], "")
        .to_ascii_lowercase();
    let t = t.strip_suffix("tokens").unwrap_or(&t);
    let (number, unit) = match t.chars().last()? {
        'k' => (&t[..t.len() - 1], 1_000.0),
        'm' => (&t[..t.len() - 1], 1_000_000.0),
        _ => (t, 1.0),
    };
    let n: f64 = number.parse().ok()?;
    let tokens = (n * unit).round();
    (1.0..1e10).contains(&tokens).then_some(tokens as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn openai() -> Provider {
        Provider {
            kind: ProviderKind::OpenAi,
            base_url: "https://openrouter.ai/api/v1".into(),
            models: vec!["qwen/qwen3-coder".into(), "deepseek/deepseek-chat".into()],
            ..Provider::default()
        }
    }

    #[test]
    fn a_slot_keeps_its_model_for_the_provider_it_was_chosen_under() {
        let p = openai();
        assert_eq!(
            p.slot_model(Some("deepseek/deepseek-chat")),
            Some("deepseek/deepseek-chat")
        );
        assert_eq!(p.slot_model(Some("claude-opus-5-5")), None);
        assert_eq!(
            p.agent_model(Some("claude-opus-5-5")).as_deref(),
            Some("qwen/qwen3-coder")
        );
        let claude = Provider {
            kind: ProviderKind::Claude,
            ..p
        };
        assert_eq!(
            claude.slot_model(Some("claude-opus-5-5")),
            Some("claude-opus-5-5")
        );
        assert_eq!(claude.slot_model(Some("deepseek/deepseek-chat")), None);
    }

    #[test]
    fn the_default_is_the_chosen_one_on_offer_else_the_first() {
        let mut p = openai();
        assert_eq!(p.default_model(), Some("qwen/qwen3-coder"));
        p.model = Some("deepseek/deepseek-chat".into());
        assert_eq!(p.default_model(), Some("deepseek/deepseek-chat"));
        p.model = Some("gone".into());
        assert_eq!(p.default_model(), Some("qwen/qwen3-coder"));
    }

    #[test]
    fn not_ready_until_it_has_a_url_and_a_model() {
        let mut p = openai();
        assert_eq!(p.not_ready(), None);
        p.models.clear();
        assert!(p.not_ready().is_some());
        p.base_url.clear();
        assert!(p.not_ready().is_some_and(|w| w.contains("URL")));
        assert_eq!(Provider::default().not_ready(), None);
    }

    #[test]
    fn it_round_trips_through_the_settings() -> Result<()> {
        let store = Store::open_in_memory()?;
        assert_eq!(Provider::load(&store)?, Provider::default());
        let mut p = openai();
        p.fast_model = Some("deepseek/deepseek-chat".into());
        p.api = ProviderApi::ChatCompletions;
        p.contexts.insert("deepseek/deepseek-chat".into(), 65_536);
        p.catalog = vec![ProviderModel {
            id: "qwen/qwen3-coder".into(),
            name: Some("Qwen3 Coder".into()),
            context: Some(262_144),
        }];
        p.save(&store)?;
        let back = Provider::load(&store)?;
        assert_eq!(back, p);
        assert_eq!(back.context_of("qwen/qwen3-coder"), Some(262_144));
        assert_eq!(back.context_of("deepseek/deepseek-chat"), Some(65_536));
        assert_eq!(Provider::default().api, ProviderApi::Responses);
        assert_eq!(back.name_of("qwen/qwen3-coder"), "Qwen3 Coder");
        // Back to Claude: the provider's setup stays for next time.
        Provider {
            kind: ProviderKind::Claude,
            ..back.clone()
        }
        .save(&store)?;
        let claude = Provider::load(&store)?;
        assert!(!claude.is_openai() && claude.models == back.models);
        Ok(())
    }
}

#[cfg(test)]
mod token_tests {
    use super::parse_tokens;

    #[test]
    fn windows_read_as_people_write_them() {
        assert_eq!(parse_tokens("128000"), Some(128_000));
        assert_eq!(parse_tokens("128k"), Some(128_000));
        assert_eq!(parse_tokens(" 1M "), Some(1_000_000));
        assert_eq!(parse_tokens("1.5m"), Some(1_500_000));
        assert_eq!(parse_tokens("131,072"), Some(131_072));
        assert_eq!(parse_tokens("64k tokens"), Some(64_000));
        assert_eq!(parse_tokens(""), None);
        assert_eq!(parse_tokens("lots"), None);
        assert_eq!(parse_tokens("0"), None);
    }
}
