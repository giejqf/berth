//! The model provider (ADR 0023): agents on this Mac run on Claude Code's
//! own account, or on an OpenAI-compatible API through Berth's proxy. The
//! proxy starts the first time a session needs it, on `127.0.0.1`, and
//! reads the setup again on every change; the API key goes no further
//! than this process.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use berth_core::provider::KEY_SERVICE;
use berth_core::{Provider, ProviderApi, ProviderModel};
use berth_proxy::{Api, Proxy, SessionPrefs, Upstream};

use super::types::{ProviderCheck, ProviderEdit, ProviderInfo, ToolState, ToolStatus, UiEvent};
use super::{Shared, lock};

/// What a session's environment loses when its requests go through the
/// proxy: each would send them elsewhere, or with another key.
pub(super) const DISPLACED_ENV: [&str; 10] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_CUSTOM_HEADERS",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_GATEWAY",
];

/// What a session going through the proxy started with. When Settings
/// changes either for it, it restarts on the new ones once it is quiet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProxySession {
    pub model: String,
    /// Its model's context window (`CLAUDE_CODE_MAX_CONTEXT_TOKENS`).
    pub window: Option<u64>,
}

/// The slot's effort as the provider's reasoning effort: Max is its most,
/// `xhigh` (a model that takes less gets the most it takes; ADR 0037).
pub(super) fn reasoning_effort(e: Option<berth_protocol::Effort>) -> Option<berth_proxy::Effort> {
    use berth_protocol::Effort as E;
    Some(match e? {
        E::Low => berth_proxy::Effort::Low,
        E::Medium => berth_proxy::Effort::Medium,
        E::High => berth_proxy::Effort::High,
        E::Xhigh | E::Max => berth_proxy::Effort::XHigh,
    })
}

impl Shared {
    pub(super) fn provider(&self) -> Provider {
        Provider::load(&self.core().store).unwrap_or_default()
    }

    pub(super) fn provider_key(&self) -> Option<String> {
        let (service, account) = KEY_SERVICE;
        self.secrets
            .get(service, account)
            .ok()
            .flatten()
            .filter(|k| !k.trim().is_empty())
    }

    /// The proxy's upstream for `p`; `None` when it isn't set up. It stays
    /// while agents switch back to Claude Code's account: a session that
    /// started on the API finishes its round there, then restarts (ADR 0037).
    fn upstream(&self, p: &Provider) -> Option<Upstream> {
        if p.base_url.trim().is_empty() || p.models.is_empty() {
            return None;
        }
        Some(Upstream {
            base_url: p.base_url.clone(),
            api_key: self.provider_key(),
            model: p.default_model()?.to_owned(),
            fast_model: p.fast_model.clone().filter(|m| p.offers(m)),
            models: p.models.clone(),
            api: match p.api {
                ProviderApi::Responses => Api::Responses,
                ProviderApi::ChatCompletions => Api::ChatCompletions,
            },
            contexts: p
                .models
                .iter()
                .filter_map(|m| Some((m.clone(), p.context_of(m)?)))
                .collect(),
        })
    }

    /// Give a running proxy the current setup.
    fn configure_proxy(&self) {
        if let Some(proxy) = self.proxy.get() {
            proxy.configure(self.upstream(&self.provider()));
        }
    }

    /// What a host session's environment needs to run on `model` through
    /// the proxy: where to send requests, its own token (which carries its
    /// effort), Claude Code's own model names mapped to the provider's, and
    /// the model's window. With what it starts on, for a later change.
    pub(super) async fn provider_env(
        &self,
        workflow_id: &str,
        model: &str,
        effort: Option<berth_protocol::Effort>,
    ) -> Result<(BTreeMap<String, String>, ProxySession)> {
        let proxy = self
            .proxy
            .get_or_try_init(|| async { Proxy::start().await })
            .await
            .context("start the model provider proxy")?;
        let p = self.provider();
        proxy.configure(self.upstream(&p));
        let fast = p
            .fast_model
            .clone()
            .filter(|m| p.offers(m))
            .unwrap_or_else(|| model.to_owned());
        let token = proxy.session_token(
            workflow_id,
            SessionPrefs {
                effort: reasoning_effort(effort),
            },
        );
        let mut env = BTreeMap::new();
        env.insert("ANTHROPIC_BASE_URL".to_owned(), proxy.base_url());
        env.insert("ANTHROPIC_AUTH_TOKEN".to_owned(), token);
        for alias in ["OPUS", "SONNET", "FABLE"] {
            env.insert(format!("ANTHROPIC_DEFAULT_{alias}_MODEL"), model.to_owned());
        }
        env.insert("ANTHROPIC_DEFAULT_HAIKU_MODEL".to_owned(), fast.clone());
        env.insert("ANTHROPIC_SMALL_FAST_MODEL".to_owned(), fast);
        let window = p.context_of(model);
        if let Some(window) = window {
            env.insert(
                "CLAUDE_CODE_MAX_CONTEXT_TOKENS".to_owned(),
                window.to_string(),
            );
        }
        Ok((
            env,
            ProxySession {
                model: model.to_owned(),
                window,
            },
        ))
    }

    /// The slot's effort changed: the session's next request takes it.
    pub(super) fn set_proxy_effort(
        &self,
        workflow_id: &str,
        effort: Option<berth_protocol::Effort>,
    ) {
        if let Some(proxy) = self.proxy.get() {
            proxy.set_session(
                workflow_id,
                SessionPrefs {
                    effort: reasoning_effort(effort),
                },
            );
        }
    }

    pub(super) fn end_proxy_session(&self, workflow_id: &str) {
        if let Some(proxy) = self.proxy.get() {
            proxy.end_session(workflow_id);
        }
    }

    /// What a session through the proxy would start with now, for its
    /// slot's model.
    fn proxy_session_now(&self, p: &Provider, workflow_id: &str) -> Option<ProxySession> {
        let slot_model = {
            let core = self.core();
            let wf = core.store.workflow(workflow_id).ok()?;
            core.store.slot(&wf.slot_id).ok()?.model
        };
        let model = p.agent_model(slot_model.as_deref())?;
        let window = p.context_of(&model);
        Some(ProxySession { model, window })
    }

    /// Settings changed the provider's models or windows: each session that
    /// started on another model or window than its slot's now restarts on
    /// it once it is quiet, as an account switch does (ADR 0017), so Claude
    /// Code's model and window are the ones Settings says.
    fn restart_for_models(self: &Arc<Self>) {
        let p = self.provider();
        if !p.is_openai() {
            return;
        }
        let started: Vec<(String, ProxySession)> = lock(&self.via_proxy)
            .iter()
            .map(|(id, s)| (id.clone(), s.clone()))
            .collect();
        for (workflow_id, was) in started {
            if self
                .proxy_session_now(&p, &workflow_id)
                .is_some_and(|now| now != was)
            {
                tracing::info!(workflow_id, "the session's model or window changed");
                self.restart_when_quiet(&workflow_id);
            }
        }
    }

    pub(super) fn emit_provider(&self) {
        let info = ProviderInfo {
            provider: self.provider(),
            key_set: self.provider_key().is_some(),
            check: lock(&self.provider_check).clone(),
            pi: self.pi_status(),
        };
        self.emit(UiEvent::Provider(Box::new(info)));
    }

    pub(super) async fn edit_provider(self: &Arc<Self>, edit: ProviderEdit) -> Result<()> {
        let (before, p, recheck) = self.apply_edit(edit)?;
        self.configure_proxy();
        if before.kind == p.kind {
            self.restart_for_models();
        } else {
            tracing::info!(openai = p.is_openai(), "agents switch model provider");
            // Sessions on this Mac go on with the other provider, each once
            // it is quiet (as for an account switch, ADR 0017).
            self.restart_host_sessions();
            // The plan's limits and the account are Claude Code's sign-in's.
            if p.is_openai() {
                self.emit(UiEvent::Quota(None));
                self.emit(UiEvent::Account(None));
            } else {
                self.emit_quota();
                self.emit_account();
            }
        }
        // Conversations keep the agent they started on; new ones need Pi
        // downloaded (ADR 0033).
        if p.is_openai() && p.agent == berth_protocol::Agent::Pi {
            self.install_pi_soon();
        }
        if !p.is_openai() || p.base_url.is_empty() {
            *lock(&self.provider_check) = ProviderCheck::Unchecked;
        }
        self.emit_provider();
        self.emit_settings()?;
        if before.kind != p.kind || before.not_ready() != p.not_ready() {
            self.check_environment().await;
        }
        if recheck {
            self.check_provider().await;
        }
        Ok(())
    }

    /// Read, change and save the setup; what it was, what it is, and
    /// whether the models need listing again.
    fn apply_edit(&self, edit: ProviderEdit) -> Result<(Provider, Provider, bool)> {
        let _edit = lock(&self.provider_edit);
        let before = self.provider();
        let mut p = before.clone();
        let mut recheck = false;
        match edit {
            ProviderEdit::Kind(kind) => {
                p.kind = kind;
                recheck = p.is_openai();
            }
            ProviderEdit::BaseUrl(url) => {
                let url = url.trim().to_owned();
                if url != p.base_url {
                    p.base_url = url;
                    p.catalog.clear();
                    recheck = true;
                }
            }
            ProviderEdit::Key(key) => {
                let (service, account) = KEY_SERVICE;
                if key.trim().is_empty() {
                    self.secrets.delete(service, account)?;
                } else {
                    self.secrets.set(service, account, key.trim())?;
                }
                recheck = true;
            }
            ProviderEdit::AddModel(id) => {
                let id = id.trim().to_owned();
                if !id.is_empty() && !p.offers(&id) {
                    p.models.push(id);
                }
            }
            ProviderEdit::RemoveModel(id) => {
                p.models.retain(|m| *m != id);
                p.contexts.remove(&id);
                for chosen in [&mut p.model, &mut p.fast_model] {
                    if chosen.as_deref() == Some(id.as_str()) {
                        *chosen = None;
                    }
                }
            }
            ProviderEdit::Model(m) => p.model = m,
            ProviderEdit::FastModel(m) => p.fast_model = m,
            ProviderEdit::Api(api) => p.api = api,
            ProviderEdit::Agent(agent) => p.agent = agent,
            ProviderEdit::Context(model, tokens) => match tokens {
                Some(t) => {
                    p.contexts.insert(model, t);
                }
                None => {
                    p.contexts.remove(&model);
                }
            },
        }
        p.save(&self.core().store)?;
        Ok((before, p, recheck))
    }

    /// List the provider's models again, keeping them for the picker in
    /// Settings and for each model's context window.
    pub(super) async fn check_provider(self: &Arc<Self>) {
        let p = self.provider();
        if !p.is_openai() || p.base_url.is_empty() {
            *lock(&self.provider_check) = ProviderCheck::Unchecked;
            self.emit_provider();
            return;
        }
        *lock(&self.provider_check) = ProviderCheck::Checking;
        self.emit_provider();
        let up = Upstream {
            base_url: p.base_url.clone(),
            api_key: self.provider_key(),
            ..Upstream::default()
        };
        let check = match berth_proxy::list_models(&up).await {
            Ok(models) => {
                let n = models.len();
                // Saved only if the URL is still the one that was asked.
                let _edit = lock(&self.provider_edit);
                let mut now = self.provider();
                if now.base_url == p.base_url {
                    now.catalog = models
                        .into_iter()
                        .map(|m| ProviderModel {
                            id: m.id,
                            name: m.name,
                            context: m.context,
                        })
                        .collect();
                    if let Err(e) = now.save(&self.core().store) {
                        tracing::warn!(error = %e, "saving the provider's models");
                    }
                }
                ProviderCheck::Ok(n)
            }
            Err(e) => {
                tracing::warn!(error = %e, "listing the provider's models");
                ProviderCheck::Failed(e.to_string())
            }
        };
        if self.provider().base_url == p.base_url {
            *lock(&self.provider_check) = check;
        }
        self.emit_provider();
    }
}

/// The "Claude Code" row under an OpenAI-compatible API: what agents run
/// on, or what is missing for them to run.
pub(super) fn provider_status(p: &Provider) -> ToolStatus {
    if let Some(why) = p.not_ready() {
        let mut s = ToolStatus::new(
            ToolState::Warn,
            "not set up",
            &format!("{why} Set it up under Model provider."),
        );
        s.blocks_agents = Some(why);
        return s;
    }
    let host = berth_proxy::host_of(&p.base_url);
    let model = p.default_model().map(|m| p.name_of(m)).unwrap_or_default();
    let agent = p.agent.label();
    let mut s = ToolStatus::new(
        ToolState::Ok,
        &format!("via {host}"),
        &format!(
            "New agents on this Mac are {agent}, running on {model} at {host}, an OpenAI-compatible API"
        ),
    );
    s.tip = Some(format!("{agent}, running on {model} at {host}"));
    s
}
