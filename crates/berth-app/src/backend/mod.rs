//! The backend: tokio-side orchestration. It owns the store and engine, runs
//! slot scripts, git, `gh`, and sidecars, and executes the engine's effects.
//! Views talk to it only through [`Command`]s and [`UiEvent`]s.

mod env;
mod files;
mod launch;
mod review;
mod session;
mod slots;
mod terminal;
pub mod types;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context as _, Result, anyhow};
use berth_connectors::{Connector, KeychainStore, MemoryStore, SecretStore};
use berth_core::{Engine, Repo, Slot, Store, paths};
use berth_slot::SlotType;
use futures::channel::mpsc::UnboundedSender;
use tokio::sync::mpsc;

pub use files::stage_pasted_image;
pub use launch::SidecarLaunch;
use session::Session;
pub use types::*;

/// Cheap, cloneable handle the UI uses to send commands.
#[derive(Clone)]
pub struct BackendHandle {
    tx: mpsc::UnboundedSender<Command>,
}

impl BackendHandle {
    pub fn send(&self, cmd: Command) {
        if self.tx.send(cmd).is_err() {
            tracing::error!("backend is gone");
        }
    }
}

struct Core {
    store: Store,
    engine: Engine,
}

struct RepoCtx {
    repo: Repo,
    slot_types: Vec<SlotType>,
}

pub(crate) struct Shared {
    core: Mutex<Core>,
    repos: Mutex<HashMap<String, RepoCtx>>,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    session_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    connectors: Mutex<Vec<Connector>>,
    secrets: Arc<dyn SecretStore>,
    launch: SidecarLaunch,
    ui: UnboundedSender<UiEvent>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Open the database, settle workflows left running by a previous run, and
/// start the command loop on `rt`.
pub fn start(rt: &tokio::runtime::Handle, ui: UnboundedSender<UiEvent>) -> Result<BackendHandle> {
    let store = Store::open(&paths::db_path()).context("open database")?;
    let mut engine = Engine::new();
    engine.recover(&store).context("recover workflows")?;
    let secrets: Arc<dyn SecretStore> = if std::env::var("BERTH_SECRETS").as_deref() == Ok("memory")
    {
        Arc::new(MemoryStore::new())
    } else {
        Arc::new(KeychainStore)
    };
    let launch = SidecarLaunch::resolve();
    tracing::info!(program = %launch.program.display(), args = ?launch.args, "sidecar launch");
    let shared = Arc::new(Shared {
        core: Mutex::new(Core { store, engine }),
        repos: Mutex::default(),
        sessions: Mutex::default(),
        session_locks: Mutex::default(),
        connectors: Mutex::default(),
        secrets,
        launch,
        ui,
    });
    shared.reload_connectors();

    let (tx, mut rx) = mpsc::unbounded_channel::<Command>();
    rt.spawn(async move {
        while let Some(cmd) = rx.recv().await {
            let shared = shared.clone();
            tokio::spawn(async move {
                let repo_hint = shared.repo_hint(&cmd);
                if let Err(e) = shared.clone().handle(cmd).await {
                    tracing::error!(error = %format!("{e:#}"), "command failed");
                    shared.toast(repo_hint, ToastLevel::Error, format!("{e:#}"));
                }
            });
        }
    });
    Ok(BackendHandle { tx })
}

impl Shared {
    fn emit(&self, e: UiEvent) {
        let _ = self.ui.unbounded_send(e);
    }

    fn toast(&self, repo_id: Option<String>, level: ToastLevel, message: impl Into<String>) {
        self.emit(UiEvent::Toast {
            repo_id,
            level,
            message: message.into(),
        });
    }

    fn core(&self) -> MutexGuard<'_, Core> {
        lock(&self.core)
    }

    /// Repo id a command concerns, for routing error toasts.
    fn repo_hint(&self, cmd: &Command) -> Option<String> {
        let wf = match cmd {
            Command::NewWorkflow(n) => return Some(n.repo_id.clone()),
            Command::RefreshRepo { repo_id }
            | Command::ListOrigins { repo_id, .. }
            | Command::TerminalSpec { repo_id, .. } => {
                return Some(repo_id.clone());
            }
            Command::SendPrompt { workflow_id, .. }
            | Command::Answer { workflow_id, .. }
            | Command::Interrupt { workflow_id }
            | Command::ResolveSlotRequest { workflow_id, .. }
            | Command::LoadWorkflow { workflow_id }
            | Command::DeleteReviewComment { workflow_id, .. }
            | Command::PostCommentsToPr { workflow_id }
            | Command::LoadDetails { workflow_id, .. }
            | Command::LoadRoundDetails { workflow_id }
            | Command::FollowUpToSlot { workflow_id, .. } => workflow_id.clone(),
            Command::LoadDiff(r) => r.workflow_id.clone(),
            Command::AddReviewComment(c) => c.workflow_id.clone(),
            Command::DestroySlot { slot_id }
            | Command::RenameSlot { slot_id, .. }
            | Command::SetSlotAgent { slot_id, .. } => {
                return self.core().store.slot(slot_id).ok().map(|s| s.repo_id);
            }
            Command::OpenLastRepo
            | Command::OpenFile { .. }
            | Command::OpenRepo { .. }
            | Command::LoadConnectors
            | Command::SetSecret { .. }
            | Command::LoadSettings
            | Command::SetSetting { .. }
            | Command::CheckEnvironment
            | Command::ToolAuth { .. }
            | Command::RevealConnectors => return None,
        };
        self.repo_of_workflow(&wf).ok()
    }

    fn repo_of_workflow(&self, workflow_id: &str) -> Result<String> {
        let core = self.core();
        let wf = core.store.workflow(workflow_id)?;
        Ok(core.store.slot(&wf.slot_id)?.repo_id)
    }

    async fn handle(self: Arc<Self>, cmd: Command) -> Result<()> {
        match cmd {
            Command::OpenLastRepo => {
                let last = self
                    .core()
                    .store
                    .setting("last_repo")?
                    .map(std::path::PathBuf::from);
                match last.filter(|p| p.exists()) {
                    Some(path) => self.open_repo(&path).await,
                    None => {
                        self.emit(UiEvent::ShowWelcome {
                            recent: self.recent_repo_entries(),
                            error: None,
                        });
                        Ok(())
                    }
                }
            }
            Command::OpenRepo { path } => self.open_repo(&path).await,
            Command::RefreshRepo { repo_id } => {
                self.emit_slots(&repo_id);
                Ok(())
            }
            Command::NewWorkflow(req) => {
                let (wf, _slot) = self.prepare_workflow(&req).await?;
                self.provision(&wf).await
            }
            Command::SendPrompt {
                workflow_id,
                text,
                attachments,
            } => self.send_prompt(&workflow_id, &text, &attachments).await,
            Command::OpenFile { path } => files::quick_look(&path),
            Command::Answer {
                workflow_id,
                question_id,
                text,
            } => self.answer(&workflow_id, &question_id, &text).await,
            Command::Interrupt { workflow_id } => {
                let session = lock(&self.sessions).get(&workflow_id).cloned();
                if let Some(s) = session {
                    s.send(berth_protocol::AppToSidecar::SessionInterrupt)?;
                }
                Ok(())
            }
            Command::ResolveSlotRequest {
                workflow_id,
                request,
                approved,
            } => {
                self.resolve_slot_request(&workflow_id, request, approved)
                    .await
            }
            Command::DestroySlot { slot_id } => self.destroy_slot(&slot_id).await,
            Command::SetSlotAgent {
                slot_id,
                model,
                effort,
            } => self.set_slot_agent(&slot_id, model, effort),
            Command::RenameSlot { slot_id, name } => {
                let name = name.as_deref().map(str::trim).filter(|n| !n.is_empty());
                let slot = self.core().store.set_slot_custom_name(&slot_id, name)?;
                self.emit_slots(&slot.repo_id);
                Ok(())
            }
            Command::LoadWorkflow { workflow_id } => {
                self.emit_workflow(&workflow_id);
                self.emit_comments(&workflow_id);
                Ok(())
            }
            Command::LoadDiff(req) => {
                let wf = req.workflow_id.clone();
                match self.load_diff(req).await {
                    Ok(diff) => self.emit(UiEvent::Diff(Arc::new(diff))),
                    Err(e) => self.emit(UiEvent::DiffFailed {
                        workflow_id: wf,
                        message: format!("{e:#}"),
                    }),
                }
                Ok(())
            }
            Command::AddReviewComment(c) => {
                let wf = c.workflow_id.clone();
                self.core()
                    .store
                    .insert_review_comment(&berth_core::NewReviewComment {
                        workflow_id: c.workflow_id,
                        from_snapshot: c.from,
                        to_snapshot: c.to,
                        preset: c.preset,
                        path: c.path,
                        side: c.side,
                        line: c.line,
                        body: c.body,
                    })?;
                self.emit_comments(&wf);
                Ok(())
            }
            Command::DeleteReviewComment {
                workflow_id,
                comment_id,
            } => {
                self.core().store.delete_review_comment(&comment_id)?;
                self.emit_comments(&workflow_id);
                Ok(())
            }
            Command::PostCommentsToPr { workflow_id } => self.post_comments(&workflow_id).await,
            Command::LoadDetails {
                workflow_id,
                node_id,
            } => self.load_details(&workflow_id, &node_id),
            Command::ListOrigins { repo_id, source } => {
                let result = self
                    .list_origins(&repo_id, &source)
                    .await
                    .map_err(|e| format!("{e:#}"));
                self.emit(UiEvent::Origins {
                    repo_id,
                    source,
                    result,
                });
                Ok(())
            }
            Command::FollowUpToSlot {
                workflow_id,
                follow_up,
                reuse,
            } => {
                let repo_id = self.repo_of_workflow(&workflow_id)?;
                let slot_id = if reuse {
                    Some(self.core().store.workflow(&workflow_id)?.slot_id)
                } else {
                    None
                };
                let req = NewWorkflow {
                    repo_id,
                    slot_type: None,
                    slot_id,
                    origin: berth_core::prompt::follow_up_origin(&follow_up),
                };
                let (wf, _) = self.prepare_workflow(&req).await?;
                self.provision(&wf).await
            }
            Command::LoadConnectors => {
                self.reload_connectors();
                self.emit(UiEvent::Connectors(self.connector_infos()));
                Ok(())
            }
            Command::LoadSettings => self.emit_settings(),
            Command::SetSetting { key, value } => {
                self.core().store.set_setting(&key, value.trim())?;
                Ok(())
            }
            Command::CheckEnvironment => {
                self.check_environment().await;
                Ok(())
            }
            Command::ToolAuth { tool, sign_in } => self.tool_auth(&tool, sign_in).await,
            Command::RevealConnectors => {
                let dir = berth_connectors::default_connectors_dir();
                std::fs::create_dir_all(&dir)?;
                std::process::Command::new("/usr/bin/open")
                    .arg(&dir)
                    .spawn()?;
                Ok(())
            }
            Command::LoadRoundDetails { workflow_id } => self.load_round_details(&workflow_id),
            Command::TerminalSpec { repo_id, slot_id } => {
                let result = self.terminal_spec(&slot_id).map_err(|e| format!("{e:#}"));
                self.emit(UiEvent::TerminalSpec {
                    repo_id,
                    slot_id,
                    result,
                });
                Ok(())
            }
            Command::SetSecret {
                connector,
                key,
                value,
            } => {
                let service = format!("com.berth.connector.{connector}");
                if value.is_empty() {
                    self.secrets.delete(&service, &key)?;
                } else {
                    self.secrets.set(&service, &key, &value)?;
                }
                self.emit(UiEvent::Connectors(self.connector_infos()));
                self.toast(
                    None,
                    ToastLevel::Success,
                    format!("Saved {key} for {connector}"),
                );
                Ok(())
            }
        }
    }

    // ---- views ----

    fn emit_workflow(&self, workflow_id: &str) {
        let loaded = {
            let core = self.core();
            core.store.workflow_view(workflow_id).and_then(|v| {
                let repo_id = core.store.slot(&v.workflow.slot_id)?.repo_id;
                Ok((repo_id, v))
            })
        };
        match loaded {
            Ok((repo_id, view)) => self.emit(UiEvent::Workflow {
                repo_id,
                view: Arc::new(view),
            }),
            Err(e) => tracing::warn!(error = %e, workflow_id, "load workflow"),
        }
    }

    fn emit_slots(&self, repo_id: &str) {
        match self.slot_entries(repo_id) {
            Ok(slots) => self.emit(UiEvent::Slots {
                repo_id: repo_id.to_owned(),
                slots,
            }),
            Err(e) => tracing::warn!(error = %e, repo_id, "load slots"),
        }
    }

    fn slot_entries(&self, repo_id: &str) -> Result<Vec<SlotEntry>> {
        let core = self.core();
        core.store
            .live_slots(repo_id)?
            .into_iter()
            .map(|slot| {
                let workflow = core.store.workflows_for_slot(&slot.id)?.pop();
                let facts = match &workflow {
                    Some(w) => core.store.workflow_facts(&w.id)?,
                    None => berth_core::WorkflowFacts::default(),
                };
                Ok(SlotEntry {
                    slot,
                    workflow,
                    facts,
                })
            })
            .collect()
    }

    fn emit_comments(&self, workflow_id: &str) {
        if let Ok(comments) = self.core().store.review_comments(workflow_id) {
            self.emit(UiEvent::ReviewComments {
                workflow_id: workflow_id.to_owned(),
                comments,
            });
        }
    }

    fn load_details(&self, workflow_id: &str, node_id: &str) -> Result<()> {
        let raw = {
            let core = self.core();
            let node = core.store.node(node_id)?;
            let (Some(first), Some(last)) = (node.raw_first, node.raw_last) else {
                return Err(anyhow!("this node has no recorded transcript"));
            };
            let lane = match &node.payload {
                berth_protocol::NodePayload::CollectResults(c) => Some(c.branch_id.clone()),
                _ => node.branch_id.clone(),
            };
            core.store
                .raw_events(workflow_id, first, last, Some(lane.as_deref()))?
        };
        let entries = berth_core::transcript::render(&raw);
        self.emit(UiEvent::Details {
            workflow_id: workflow_id.to_owned(),
            node_id: node_id.to_owned(),
            entries,
        });
        Ok(())
    }

    /// The latest round's transcript (the live card's "Show transcript").
    fn load_round_details(&self, workflow_id: &str) -> Result<()> {
        let raw = {
            let core = self.core();
            let round = match core.store.open_round(workflow_id)? {
                Some(r) => r,
                None => core
                    .store
                    .workflow_view(workflow_id)?
                    .rounds
                    .pop()
                    .ok_or_else(|| anyhow!("no rounds yet"))?,
            };
            let Some((first, last)) = core.store.round_raw_span(&round.id)? else {
                return Err(anyhow!("this round has no recorded transcript yet"));
            };
            core.store
                .raw_events(workflow_id, first, last, Some(None))?
        };
        self.emit(UiEvent::Details {
            workflow_id: workflow_id.to_owned(),
            node_id: String::new(),
            entries: berth_core::transcript::render(&raw),
        });
        Ok(())
    }

    // ---- connectors ----

    fn reload_connectors(&self) {
        let found = berth_connectors::discover_layers(
            builtin_connectors_dir().as_deref(),
            &berth_connectors::default_connectors_dir(),
        );
        for (path, err) in &found.errors {
            tracing::warn!(path = %path.display(), error = %err, "invalid connector");
        }
        {
            let core = self.core();
            for c in &found.connectors {
                let _ = core
                    .store
                    .upsert_connector(&c.name, &c.dir.to_string_lossy());
            }
        }
        *lock(&self.connectors) = found.connectors;
    }

    /// Active connectors: discovered, not disabled in the DB, and (for
    /// built-in ones) with their required keys stored.
    fn enabled_connectors(&self) -> Vec<Connector> {
        let disabled: Vec<String> = self
            .core()
            .store
            .connectors()
            .map(|rows| {
                rows.into_iter()
                    .filter(|r| !r.enabled)
                    .map(|r| r.name)
                    .collect()
            })
            .unwrap_or_default();
        let connectors = lock(&self.connectors).clone();
        connectors
            .into_iter()
            .filter(|c| !disabled.contains(&c.name) && (!c.builtin || self.keys_stored(c)))
            .collect()
    }

    fn secret_stored(&self, c: &Connector, key: &str) -> bool {
        matches!(self.secrets.get(&c.keychain_service(), key), Ok(Some(_)))
    }

    /// Every required key is in the Keychain.
    fn keys_stored(&self, c: &Connector) -> bool {
        c.secrets.iter().all(|k| self.secret_stored(c, k))
    }

    fn connector_infos(&self) -> Vec<ConnectorInfo> {
        let active: Vec<String> = self
            .enabled_connectors()
            .into_iter()
            .map(|c| c.name)
            .collect();
        let connectors = lock(&self.connectors).clone();
        connectors
            .iter()
            .map(|c| {
                let required = c.secrets.iter().map(|k| (k, false));
                let optional = c.optional_secrets.iter().map(|k| (k, true));
                ConnectorInfo {
                    name: c.name.clone(),
                    title: c.title.clone(),
                    description: c.description.clone(),
                    keys: required
                        .chain(optional)
                        .map(|(k, optional)| ConnectorKey {
                            name: k.clone(),
                            set: self.secret_stored(c, k),
                            optional,
                        })
                        .collect(),
                    builtin: c.builtin,
                    active: active.contains(&c.name),
                    origin_kind: c.origin.as_ref().map(|o| o.kind.clone()),
                }
            })
            .collect()
    }

    fn secret_env(&self) -> Vec<(String, String)> {
        match berth_connectors::secret_env(&self.enabled_connectors(), self.secrets.as_ref()) {
            Ok(env) => env.vars,
            Err(e) => {
                tracing::warn!(error = %e, "connector secrets unavailable");
                Vec::new()
            }
        }
    }

    // ---- lookups ----

    fn slot_type_for(&self, slot: &Slot) -> Result<SlotType> {
        if let Some(ctx) = lock(&self.repos).get(&slot.repo_id)
            && let Some(t) = ctx.slot_types.iter().find(|t| t.name == slot.slot_type)
        {
            return Ok(t.clone());
        }
        let repo = self.core().store.repo(&slot.repo_id)?;
        let types = berth_slot::discover_slot_types(&berth_slot::global_slots_dir(), &repo.path)?;
        types
            .into_iter()
            .find(|t| t.name == slot.slot_type)
            .ok_or_else(|| anyhow!("slot type `{}` is not installed", slot.slot_type))
    }

    /// Recently opened repositories (most recent first), from the settings table.
    /// Recent repositories with what Berth knows about them (Welcome).
    fn recent_repo_entries(&self) -> Vec<RecentRepo> {
        let paths = self.recent_repos();
        let core = self.core();
        paths
            .into_iter()
            .map(|path| {
                let slots = core
                    .store
                    .repo_by_path(&path)
                    .ok()
                    .flatten()
                    .and_then(|r| core.store.live_slots(&r.id).ok())
                    .unwrap_or_default();
                let last_active = slots
                    .iter()
                    .filter_map(|s| {
                        let wfs = core.store.workflows_for_slot(&s.id).ok()?;
                        let wf = wfs.last()?;
                        let facts = core.store.workflow_facts(&wf.id).ok()?;
                        Some(
                            facts
                                .last_round_at
                                .unwrap_or(wf.created_at)
                                .max(s.created_at),
                        )
                    })
                    .max();
                RecentRepo {
                    path,
                    slots: slots.len(),
                    last_active,
                }
            })
            .collect()
    }

    fn recent_repos(&self) -> Vec<std::path::PathBuf> {
        self.core()
            .store
            .setting("recent_repos")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<Vec<std::path::PathBuf>>(&s).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.exists())
            .collect()
    }

    fn remember_repo(&self, path: &std::path::Path) {
        let mut recent = self.recent_repos();
        recent.retain(|p| p != path);
        recent.insert(0, path.to_path_buf());
        recent.truncate(10);
        let core = self.core();
        let _ = core.store.set_setting("last_repo", &path.to_string_lossy());
        if let Ok(json) = serde_json::to_string(&recent) {
            let _ = core.store.set_setting("recent_repos", &json);
        }
    }

    fn repo_path(&self, repo_id: &str) -> Result<std::path::PathBuf> {
        Ok(self.core().store.repo(repo_id)?.path)
    }
}

/// Berth's own connectors: the app bundle's `Contents/Resources/connectors`,
/// else the workspace's `connectors/` (dev; `just fetch-connectors` fills
/// their `bin/`).
fn builtin_connectors_dir() -> Option<PathBuf> {
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("../Resources/connectors")));
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../connectors");
    bundled.into_iter().chain([workspace]).find(|d| d.is_dir())
}
