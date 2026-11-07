//! Repos, slot lifecycle (AGENTS.md §6.3), origins, and GitHub glue.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use berth_core::ids::{new_id, now_ms, short};
use berth_core::{Engine, Slot, SlotState, SnapshotKind, Workflow, paths, prompt};
use berth_gh::{Gh, ListState, ReviewCommentInput, ReviewSide};
use berth_git::{LocalGit, SnapshotIndex};
use berth_protocol::{
    AppToSidecar, DiffSide, NodePayload, Origin, OriginKind, SlotRequest, SlotRequestResolved,
};
use berth_slot::{Placement, SlotEnv, SlotScript, SlotType};

use super::{
    GhInfo, NewWorkflow, OriginChoice, OriginSource, RepoCtx, RepoSnapshot, Shared, ToastLevel,
    UiEvent, lock,
};

/// The type a new slot gets when none is picked: the one the repository's
/// last slot was made with, else its `.berth/berth.toml` default, else
/// `worktree`, else the first. A name no type has any more is passed over.
fn default_slot_type<'a>(
    types: &'a [SlotType],
    last: Option<&str>,
    configured: Option<&str>,
) -> Option<&'a SlotType> {
    let named = |n: &str| types.iter().find(|t| t.name == n);
    last.and_then(named)
        .or_else(|| configured.and_then(named))
        .or_else(|| named("worktree"))
        .or_else(|| types.first())
}

/// Slot types shipped with Berth, installed on first run if none exist.
const BUNDLED_SLOT_TYPES: &[&str] = &["worktree"];
/// How long a new slot may report "not ready" through `slot.sh status`, how
/// often it is asked, and how long one answer may take.
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);
const STATUS_POLL: std::time::Duration = std::time::Duration::from_secs(2);
const STATUS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

impl Shared {
    pub(super) async fn open_repo(self: &Arc<Self>, path: &Path) -> Result<()> {
        let root = match berth_git::repo_root(path).await {
            Ok(root) => root,
            Err(e) => {
                self.emit(UiEvent::RepoFailed {
                    path: path.to_path_buf(),
                    message: format!("{} is not a git repository ({e})", path.display()),
                });
                self.emit(UiEvent::ShowWelcome {
                    recent: self.recent_repo_entries(),
                    error: Some(format!("{} is not a git repository.", path.display())),
                });
                return Ok(());
            }
        };
        let repo = self.core().store.upsert_repo(&root)?;
        self.ensure_bundled_slot_types();
        let found = berth_slot::discover_slot_types(&berth_slot::global_slots_dir(), &root);
        let slot_types = found.types;
        let config = berth_slot::load_repo_config(&root)?;
        let default_slot = default_slot_type(
            &slot_types,
            repo.default_slot_type.as_deref(),
            config.default_slot.as_deref(),
        )
        .map(|t| t.name.clone());
        let infos = self.slot_type_infos(&repo.id, &slot_types);
        lock(&self.repos).insert(
            repo.id.clone(),
            RepoCtx {
                repo: repo.clone(),
                slot_types,
                configured_slot: config.default_slot,
            },
        );

        let gh = match Gh::new(&root).auth_status().await {
            Ok(s) => {
                tracing::info!(installed = s.installed, logged_in = s.logged_in, detail = %s.detail, "gh auth status");
                GhInfo {
                    installed: s.installed,
                    logged_in: s.logged_in,
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "gh auth status failed");
                GhInfo {
                    installed: false,
                    logged_in: false,
                }
            }
        };
        // Slots from before windows join the first; a repo without slots gets one.
        let windows = self.core().store.repo_windows(&repo.id, &new_id())?;
        let slots = self.slot_entries(&repo.id)?;
        self.remember_repo(&root);
        let repo_id = repo.id.clone();
        self.emit(UiEvent::RepoOpened(Box::new(RepoSnapshot {
            repo,
            slot_types: infos,
            default_slot,
            slots,
            windows,
            gh,
            connectors: self.connector_infos(),
        })));
        // A window that opens after launch missed the first report of these.
        self.emit_settings()?;
        self.emit_models();
        self.emit_quota();
        self.emit_account();
        // A type that can't be read is left out; the others still work.
        for e in found.problems {
            tracing::warn!(error = %e, "slot type left out");
            self.toast(
                Some(repo_id.clone()),
                ToastLevel::Error,
                format!("A slot type was left out: {e}"),
            );
        }
        self.emit(UiEvent::Commands {
            commands: self.commands(&repo_id),
            repo_id,
        });
        Ok(())
    }

    /// First run: copy the reference `worktree` slot type into the global dir.
    fn ensure_bundled_slot_types(&self) {
        let global = berth_slot::global_slots_dir();
        let examples = bundled_examples_dir();
        for name in BUNDLED_SLOT_TYPES {
            let target = global.join(name);
            let source = examples.join(name);
            if target.exists() || !source.exists() {
                continue;
            }
            let copied = std::fs::create_dir_all(&target).and_then(|()| {
                for file in ["slot.toml", "slot.sh"] {
                    std::fs::copy(source.join(file), target.join(file))?;
                }
                Ok(())
            });
            match copied {
                Ok(()) => {
                    tracing::info!(slot_type = name, "installed bundled slot type");
                    self.toast(
                        None,
                        ToastLevel::Info,
                        format!("Installed the `{name}` slot type in {}", global.display()),
                    );
                }
                Err(e) => tracing::warn!(error = %e, "install bundled slot type"),
            }
        }
    }

    pub(super) fn slot_script(&self, slot: &Slot) -> Result<SlotScript> {
        let ty = self.slot_type_for(slot)?;
        self.require_trusted(&slot.repo_id, &ty)?;
        let repo = self.repo_path(&slot.repo_id)?;
        let env = SlotEnv {
            repo,
            slot_id: slot.id.clone(),
            state_dir: paths::slot_state_dir(&slot.id),
            base_ref: slot.base_ref.clone(),
            branch: slot.branch.clone(),
            sidecar_bundle: self.launch.bundle_dir.clone(),
            extra: self.secret_env(),
        };
        let mut script = SlotScript::new(ty, env);
        if let Some(path) = &slot.checkout_path {
            script = script.with_checkout(path.clone());
        }
        Ok(script)
    }

    pub(super) fn snapshot_index(slot: &Slot) -> SnapshotIndex {
        if slot.placement == Placement::Inside.as_str() {
            SnapshotIndex::GitDir
        } else {
            SnapshotIndex::Path(
                paths::slot_state_dir(&slot.id)
                    .join("snap.idx")
                    .to_string_lossy()
                    .into_owned(),
            )
        }
    }

    /// Snapshot the slot's live tree.
    pub(super) async fn snapshot_slot(&self, slot: &Slot) -> Result<String> {
        let script = self.slot_script(slot)?;
        let tree = berth_git::snapshot(&script, &Self::snapshot_index(slot)).await?;
        Ok(tree.0)
    }

    pub(super) async fn take_snapshot(
        self: Arc<Self>,
        workflow_id: String,
        section_no: u32,
        kind: SnapshotKind,
    ) {
        let result = async {
            let slot = {
                let core = self.core();
                core.store
                    .slot(&core.store.workflow(&workflow_id)?.slot_id)?
            };
            let tree = self.snapshot_slot(&slot).await?;
            Engine::record_snapshot(&self.core().store, &workflow_id, section_no, kind, &tree)?;
            anyhow::Ok(())
        }
        .await;
        match result {
            Ok(()) => self.emit_workflow(&workflow_id),
            Err(e) => tracing::warn!(error = %format!("{e:#}"), workflow_id, "snapshot failed"),
        }
    }

    pub(super) async fn take_branch_snapshot(
        self: Arc<Self>,
        workflow_id: String,
        branch_id: String,
        at_collect: bool,
    ) {
        let result = async {
            let slot = {
                let core = self.core();
                core.store
                    .slot(&core.store.workflow(&workflow_id)?.slot_id)?
            };
            let tree = self.snapshot_slot(&slot).await?;
            let (spawn, collect) = if at_collect {
                (None, Some(tree.as_str()))
            } else {
                (Some(tree.as_str()), None)
            };
            self.core()
                .store
                .set_branch_tree(&branch_id, spawn, collect)?;
            anyhow::Ok(())
        }
        .await;
        if let Err(e) = result {
            tracing::debug!(error = %format!("{e:#}"), "branch snapshot failed (best effort)");
        }
    }

    /// Insert the slot and workflow rows (fast) so the UI and `request_slot`
    /// get ids immediately; [`Shared::provision`] does the slow part.
    pub(super) async fn prepare_workflow(
        self: &Arc<Self>,
        req: &NewWorkflow,
    ) -> Result<(Workflow, Slot)> {
        if let Some(slot_id) = &req.slot_id {
            return self.prepare_reuse(req, slot_id).await;
        }
        let repo_path = self.repo_path(&req.repo_id)?;
        let ty = {
            let repos = lock(&self.repos);
            let ctx = repos
                .get(&req.repo_id)
                .ok_or_else(|| anyhow!("repository is not open"))?;
            match &req.slot_type {
                Some(n) => ctx.slot_types.iter().find(|t| &t.name == n),
                None => default_slot_type(
                    &ctx.slot_types,
                    ctx.repo.default_slot_type.as_deref(),
                    ctx.configured_slot.as_deref(),
                ),
            }
            .cloned()
            .ok_or_else(|| anyhow!("no slot type available; add one under ~/.config/berth/slots"))?
        };
        self.require_trusted(&req.repo_id, &ty)?;
        let origin = self.enrich_origin(&repo_path, req.origin.clone()).await;
        let slot_id = new_id();
        // The app reads the repo to choose the branch and its base; making
        // the checkout, fetching that base included, is the slot script's job.
        let git = LocalGit::new(&repo_path);
        let (branch, base_ref) = if origin.kind == OriginKind::GhPr {
            let pr = Gh::new(&repo_path).pr(&origin.r#ref).await?;
            (
                pr.head_ref_name.clone(),
                format!("origin/{}", pr.head_ref_name),
            )
        } else {
            (
                prompt::branch_name(&origin.title, &slot_id),
                berth_git::default_base_ref(&git).await?,
            )
        };
        let mut slot = Slot {
            id: slot_id,
            repo_id: req.repo_id.clone(),
            slot_type: ty.name.clone(),
            name: slot_name(&origin),
            custom_name: None,
            model: None,
            effort: None,
            state: SlotState::Creating,
            state_detail: None,
            checkout_path: None,
            placement: ty.agent_placement.as_str().to_owned(),
            branch,
            base_ref,
            created_at: now_ms(),
            number: 0,
        };
        let wf = {
            let core = self.core();
            core.store.insert_slot(&mut slot)?;
            // In the window that asked for it, else the repo's first.
            let window = match &req.window_id {
                Some(w) => w.clone(),
                None => core.store.repo_windows(&req.repo_id, &new_id())?.remove(0),
            };
            core.store.set_slot_window(&slot.id, &window)?;
            let wf = Engine::create_workflow(&core.store, &slot.id, origin.clone())?;
            if let Some(from) = &req.from_workflow {
                core.store.set_workflow_from(&wf.id, from)?;
            }
            wf
        };
        self.remember_slot_type(&req.repo_id, &ty.name)?;
        self.link_origin(&req.repo_id, &wf.id, &origin).await;
        self.emit_slots(&req.repo_id);
        self.emit_workflow(&wf.id);
        Ok((wf, slot))
    }

    /// The repository's next slot defaults to the type its last one was made
    /// with; every window's New workflow starts on it.
    fn remember_slot_type(&self, repo_id: &str, name: &str) -> Result<()> {
        {
            let mut repos = lock(&self.repos);
            let Some(ctx) = repos.get_mut(repo_id) else {
                return Ok(());
            };
            if ctx.repo.default_slot_type.as_deref() == Some(name) {
                return Ok(());
            }
            ctx.repo.default_slot_type = Some(name.to_owned());
        }
        self.core()
            .store
            .set_repo_default_slot(repo_id, Some(name))?;
        self.emit(UiEvent::DefaultSlotType {
            repo_id: repo_id.to_owned(),
            slot_type: name.to_owned(),
        });
        Ok(())
    }

    /// A new workflow in an existing slot (§6.3): its open workflow, if any, is
    /// closed first; its checkout stays exactly as it is (no reset, no branch
    /// switch). The agent starts with a fresh context.
    async fn prepare_reuse(
        self: &Arc<Self>,
        req: &NewWorkflow,
        slot_id: &str,
    ) -> Result<(Workflow, Slot)> {
        let repo_path = self.repo_path(&req.repo_id)?;
        let (slot, latest) = {
            let core = self.core();
            (
                core.store.slot(slot_id)?,
                core.store.workflows_for_slot(slot_id)?.pop(),
            )
        };
        if slot.repo_id != req.repo_id {
            return Err(anyhow!("that slot belongs to another repository"));
        }
        let closes = slot
            .reuse(latest.as_ref())
            .map_err(|blocked| anyhow!(blocked.reason()))?;
        if let Some(old) = latest.filter(|_| closes) {
            // Its sidecar goes first, so nothing it still sends lands after the close.
            let session = self.take_session(&old.id);
            if let Some(s) = session {
                s.shutdown().await;
            }
            let fx = {
                let mut core = self.core();
                let super::Core { store, engine } = &mut *core;
                engine.close_for_reuse(store, &old.id)?
            };
            self.run_effects(&old.id, fx);
        }
        // Record the branch the checkout is on now; earlier work may have moved it.
        self.refresh_branch(&slot).await?;
        let origin = self.enrich_origin(&repo_path, req.origin.clone()).await;
        let (wf, slot) = {
            let core = self.core();
            let wf = Engine::create_workflow(&core.store, slot_id, origin.clone())?;
            if let Some(from) = &req.from_workflow {
                core.store.set_workflow_from(&wf.id, from)?;
            }
            (wf, core.store.slot(slot_id)?)
        };
        self.link_origin(&req.repo_id, &wf.id, &origin).await;
        self.emit_slots(&req.repo_id);
        self.emit_workflow(&wf.id);
        Ok((wf, slot))
    }

    /// Create the slot via `slot.sh create` (a reused slot already exists),
    /// snapshot, start the session, and send the origin turn.
    pub(super) async fn provision(self: &Arc<Self>, wf: &Workflow) -> Result<()> {
        let slot = self.core().store.slot(&wf.slot_id)?;
        if slot.state != SlotState::Creating {
            self.core()
                .store
                .transition_slot(&slot.id, SlotState::Running, None)?;
            return self.start_or_fail(wf).await;
        }
        let script = self.slot_script(&slot)?;
        std::fs::create_dir_all(paths::slot_state_dir(&slot.id))?;
        // An `inside` slot's script may copy the sidecar bundle in (§6.1).
        let bundle = if slot.placement == Placement::Inside.as_str() {
            self.agent_installed(false).await
        } else {
            Ok(())
        };
        let created = match bundle {
            Err(e) => Err(e),
            Ok(()) => match script.create().await {
                Ok(out) => {
                    self.core().store.set_slot_checkout(&slot.id, &out.path)?;
                    self.wait_until_ready(&slot.id, &script).await
                }
                Err(e) => Err(anyhow!("slot creation failed: {e}")),
            },
        };
        match created {
            Ok(()) => {
                let core = self.core();
                core.store
                    .transition_slot(&slot.id, SlotState::Ready, None)?;
                core.store
                    .transition_slot(&slot.id, SlotState::Running, None)?;
            }
            Err(e) => {
                let message = e.to_string();
                {
                    let core = self.core();
                    core.store
                        .transition_slot(&slot.id, SlotState::Failed, Some(&message))?;
                    core.store.set_workflow_error(&wf.id, Some(&message))?;
                }
                self.emit_slots(&slot.repo_id);
                self.emit_workflow(&wf.id);
                return Err(anyhow!(message));
            }
        }
        self.start_or_fail(wf).await
    }

    /// After `create`: while the script's optional `slot.sh status` says the
    /// environment is not ready (a VM booting, a container provisioning), the
    /// slot stays Creating with the status's detail on show (§6.2). A script
    /// without `status` is ready once `create` returns.
    async fn wait_until_ready(&self, slot_id: &str, script: &SlotScript) -> Result<()> {
        let script = script.clone().with_timeout(STATUS_TIMEOUT);
        let deadline = std::time::Instant::now() + READY_TIMEOUT;
        let mut shown = String::new();
        loop {
            let detail = match script.status().await {
                Ok(None) => return Ok(()),
                Ok(Some(s)) if s.ready => return Ok(()),
                Ok(Some(s)) => s.detail,
                // A status that hangs or fails to run says nothing yet; ask again.
                Err(e) => format!("status: {e}"),
            };
            if std::time::Instant::now() >= deadline {
                return Err(anyhow!(
                    "slot not ready after {} minutes: {detail}",
                    READY_TIMEOUT.as_secs() / 60
                ));
            }
            if detail != shown {
                let repo_id = {
                    let core = self.core();
                    let detail = Some(detail.as_str()).filter(|d| !d.trim().is_empty());
                    core.store
                        .transition_slot(slot_id, SlotState::Creating, detail)?
                        .repo_id
                };
                self.emit_slots(&repo_id);
                shown = detail;
            }
            tokio::time::sleep(STATUS_POLL).await;
        }
    }

    /// [`Self::start_workflow`], and if it fails (the first snapshot, the skill
    /// install, the sidecar), say so on the workflow and settle it, so it shows
    /// Failed instead of starting forever; the next prompt tries again.
    async fn start_or_fail(self: &Arc<Self>, wf: &Workflow) -> Result<()> {
        let started = self.start_workflow(wf).await;
        if let Err(e) = &started {
            let message = format!("The agent did not start: {e:#}");
            self.retire_session(&wf.id);
            let fx = {
                let mut core = self.core();
                let super::Core { store, engine } = &mut *core;
                engine.sidecar_exited(store, &wf.id, Some(&message))
            };
            match fx {
                Ok(fx) => self.run_effects(&wf.id, fx),
                Err(e) => tracing::error!(error = %e, "settling a failed start"),
            }
            self.emit_workflow(&wf.id);
        }
        started
    }

    /// Snapshot the checkout, start the session, and send the origin turn.
    async fn start_workflow(self: &Arc<Self>, wf: &Workflow) -> Result<()> {
        let slot = self.core().store.slot(&wf.slot_id)?;
        self.emit_slots(&slot.repo_id);
        let tree = self
            .snapshot_slot(&slot)
            .await
            .context("initial snapshot")?;
        Engine::record_snapshot(&self.core().store, &wf.id, 1, SnapshotKind::Start, &tree)?;

        let session = self.ensure_session(&wf.id).await?;
        let text = {
            let mut core = self.core();
            let super::Core { store, engine } = &mut *core;
            engine.begin_origin_round(store, &wf.id)?
        };
        session.send(AppToSidecar::SessionTurn(berth_protocol::SessionTurn {
            text,
            attachments: vec![],
            uuid: None,
        }))?;
        self.emit_workflow(&wf.id);
        self.emit_slots(&slot.repo_id);
        Ok(())
    }

    pub(super) async fn destroy_slot(self: &Arc<Self>, slot_id: &str) -> Result<()> {
        let (slot, active, all) = {
            let core = self.core();
            (
                core.store.slot(slot_id)?,
                core.store.active_workflow(slot_id)?,
                core.store.workflows_for_slot(slot_id)?,
            )
        };
        // Its `create` (or `status` wait) is still running: destroying now
        // would leave whatever it makes with no slot to clean it up.
        if slot.state == SlotState::Creating {
            return Err(anyhow!(
                "The slot is still being created; destroy it once it is ready."
            ));
        }
        // No agent outlives its slot, whichever of its workflows it served.
        for wf in &all {
            let session = self.take_session(&wf.id);
            if let Some(s) = session {
                s.shutdown().await;
            }
        }
        self.core()
            .store
            .transition_slot(slot_id, SlotState::Destroying, None)?;
        self.emit_slots(&slot.repo_id);
        let result = match self.slot_script(&slot) {
            Ok(script) => script.destroy().await.map_err(anyhow::Error::from),
            Err(e) => Err(e),
        };
        let closed = {
            let mut core = self.core();
            let super::Core { store, engine } = &mut *core;
            match &result {
                Ok(()) => {
                    store.transition_slot(slot_id, SlotState::Destroyed, None)?;
                    let _ = std::fs::remove_dir_all(paths::slot_state_dir(slot_id));
                }
                Err(e) => {
                    store.transition_slot(
                        slot_id,
                        SlotState::Failed,
                        Some(&format!("destroy failed: {e}")),
                    )?;
                }
            }
            // Its open workflow closes the way any does: open round ended,
            // running subagents cancelled (§5.4).
            match &active {
                Some(wf) => {
                    let fx = engine.close_for_reuse(store, &wf.id)?;
                    store.set_workflow_error(&wf.id, Some("Slot destroyed before an outcome."))?;
                    Some((wf.id.clone(), fx))
                }
                None => None,
            }
        };
        if let Some((wf, fx)) = closed {
            self.run_effects(&wf, fx);
        }
        self.emit_slots(&slot.repo_id);
        if let Some(wf) = active {
            self.emit_workflow(&wf.id);
        }
        result.map(|()| {
            self.toast(
                Some(slot.repo_id.clone()),
                ToastLevel::Success,
                format!("Destroyed slot {}", slot.name),
            );
        })
    }

    pub(super) async fn resolve_slot_request(
        self: &Arc<Self>,
        workflow_id: &str,
        request: SlotRequest,
        approved: bool,
    ) -> Result<()> {
        let session = lock(&self.sessions).get(workflow_id).cloned();
        let reply = |slot_id: Option<String>| {
            if let Some(s) = &session {
                let _ = s.send(AppToSidecar::SlotRequestResolved(SlotRequestResolved {
                    request_id: request.request_id.clone(),
                    approved: slot_id.is_some(),
                    slot_id,
                }));
            }
        };
        if !approved {
            reply(None);
            return Ok(());
        }
        // The agent may have stopped waiting (interrupted, or its session
        // ended): it was told no, so nothing is made.
        let waiting = session.is_some()
            && self.core().store.workflow(workflow_id)?.state == berth_core::WorkflowState::Running;
        if !waiting {
            return Err(anyhow!(
                "The agent stopped waiting for that slot, so none was created."
            ));
        }
        let repo_id = self.repo_of_workflow(workflow_id)?;
        let origin = self.slot_request_origin(workflow_id, &request)?;
        // The agent's new slot opens beside its own.
        let window_id = {
            let core = self.core();
            core.store
                .slot_window(&core.store.workflow(workflow_id)?.slot_id)?
        };
        match self
            .prepare_workflow(&NewWorkflow {
                repo_id,
                slot_type: None,
                slot_id: None,
                window_id,
                origin,
                from_workflow: Some(workflow_id.to_owned()),
            })
            .await
        {
            Ok((wf, slot)) => {
                reply(Some(slot.id.clone()));
                self.provision(&wf).await
            }
            Err(e) => {
                reply(None);
                Err(e)
            }
        }
    }

    /// A follow-up origin when `from_follow_up` names one of this workflow's
    /// follow-ups; otherwise a thought built from the request.
    fn slot_request_origin(&self, workflow_id: &str, request: &SlotRequest) -> Result<Origin> {
        if let Some(fu_id) = &request.from_follow_up {
            let nodes = self.core().store.nodes(workflow_id)?;
            let found = nodes.iter().find_map(|n| match &n.payload {
                NodePayload::Summary(s) => s.follow_ups.iter().find(|f| &f.id == fu_id).cloned(),
                NodePayload::Outcome(o) => o.follow_ups.iter().find(|f| &f.id == fu_id).cloned(),
                _ => None,
            });
            if let Some(fu) = found {
                return Ok(prompt::follow_up_origin(&fu));
            }
        }
        Ok(Origin {
            kind: OriginKind::FollowUp,
            r#ref: request.from_follow_up.clone().unwrap_or_default(),
            title: request.name.clone(),
            body: request.purpose.clone(),
            custom_prompt: None,
            url: None,
        })
    }

    /// Fill in an issue/PR body from GitHub when the picker only had the title.
    async fn enrich_origin(&self, repo: &Path, mut origin: Origin) -> Origin {
        let gh = Gh::new(repo);
        match origin.kind {
            OriginKind::GhIssue if origin.body.is_empty() => {
                if let Ok(n) = origin.r#ref.parse::<u64>()
                    && let Ok(issue) = gh.issue(n).await
                {
                    origin.body = with_comments(&issue.body, &issue.comments);
                }
            }
            OriginKind::GhPr if origin.body.is_empty() => {
                if let Ok(pr) = gh.pr(&origin.r#ref).await {
                    origin.body = with_comments(&pr.body, &pr.comments);
                }
            }
            _ => {}
        }
        origin
    }

    async fn link_origin(&self, repo_id: &str, workflow_id: &str, origin: &Origin) {
        let kind = match origin.kind {
            OriginKind::GhIssue => "issue",
            OriginKind::GhPr => "pr",
            _ => return,
        };
        let json = serde_json::json!({ "number": origin.r#ref, "title": origin.title }).to_string();
        let core = self.core();
        if let Ok(e) = core
            .store
            .upsert_gh_entity(repo_id, kind, &origin.r#ref, &json)
        {
            let _ = core.store.link_workflow(workflow_id, &e.id, "origin");
        }
    }

    /// Link a PR outcome to a synced GitHub entity (§8.1).
    pub(super) async fn link_outcome(
        self: Arc<Self>,
        workflow_id: String,
        outcome: berth_protocol::Outcome,
    ) {
        if outcome.kind != berth_protocol::OutcomeKind::Pr || outcome.r#ref.is_empty() {
            return;
        }
        let Ok(repo_id) = self.repo_of_workflow(&workflow_id) else {
            return;
        };
        let Ok(repo) = self.repo_path(&repo_id) else {
            return;
        };
        match Gh::new(&repo).pr(&outcome.r#ref).await {
            Ok(pr) => {
                let json = serde_json::to_string(&pr).unwrap_or_default();
                let core = self.core();
                if let Ok(e) =
                    core.store
                        .upsert_gh_entity(&repo_id, "pr", &pr.number.to_string(), &json)
                {
                    let _ = core.store.link_workflow(&workflow_id, &e.id, "outcome");
                }
            }
            Err(e) => tracing::warn!(error = %e, "could not sync outcome PR"),
        }
    }

    pub(super) async fn list_origins(
        &self,
        repo_id: &str,
        source: &OriginSource,
    ) -> Result<Vec<OriginChoice>> {
        let repo = self.repo_path(repo_id)?;
        let gh = Gh::new(&repo);
        Ok(match source {
            OriginSource::GhIssues => gh
                .list_issues(ListState::Open, 100)
                .await?
                .into_iter()
                .map(|i| OriginChoice {
                    kind: OriginKind::GhIssue,
                    r#ref: i.number.to_string(),
                    title: i.title,
                    body: String::new(),
                    url: Some(i.url),
                })
                .collect(),
            OriginSource::GhPrs => gh
                .list_prs(ListState::Open, 100)
                .await?
                .into_iter()
                .map(|p| OriginChoice {
                    kind: OriginKind::GhPr,
                    r#ref: p.number.to_string(),
                    title: p.title,
                    body: String::new(),
                    url: Some(p.url),
                })
                .collect(),
            OriginSource::Connector(name) => {
                let connector = self
                    .enabled_connectors()
                    .into_iter()
                    .find(|c| &c.name == name)
                    .ok_or_else(|| anyhow!("connector {name} is not installed"))?;
                let kind = match connector.origin.as_ref().map(|o| o.kind.as_str()) {
                    Some("linear") => OriginKind::Linear,
                    Some("posthog") => OriginKind::Posthog,
                    _ => OriginKind::Thought,
                };
                connector
                    .list_origins(&self.secret_env())
                    .await?
                    .into_iter()
                    .map(|o| OriginChoice {
                        kind,
                        r#ref: o.r#ref,
                        title: o.title,
                        body: o.body.unwrap_or_default(),
                        url: o.url,
                    })
                    .collect()
            }
        })
    }

    /// "Post to PR": inline review comments, falling back to one PR comment.
    pub(super) async fn post_comments(&self, workflow_id: &str) -> Result<()> {
        let (repo_id, slot, comments, linked_pr) = {
            let core = self.core();
            let wf = core.store.workflow(workflow_id)?;
            let slot = core.store.slot(&wf.slot_id)?;
            let comments: Vec<_> = core
                .store
                .review_comments(workflow_id)?
                .into_iter()
                .filter(|c| !c.posted_to_pr)
                .collect();
            let linked = core
                .store
                .workflow_links(workflow_id)?
                .into_iter()
                .find(|(role, e)| role == "outcome" && e.kind == "pr")
                .map(|(_, e)| e.number_or_sha);
            (slot.repo_id.clone(), slot, comments, linked)
        };
        if comments.is_empty() {
            self.toast(
                Some(repo_id),
                ToastLevel::Info,
                "No unposted review comments.",
            );
            return Ok(());
        }
        let gh = Gh::new(self.repo_path(&repo_id)?);
        let pr = match linked_pr {
            Some(n) => gh.pr(&n).await?,
            None => gh
                .pr_for_branch(&slot.branch)
                .await?
                .ok_or_else(|| anyhow!("no pull request for {}", slot.branch))?,
        };
        let inputs: Vec<ReviewCommentInput> = comments
            .iter()
            .map(|c| ReviewCommentInput {
                path: c.path.clone(),
                line: c.line,
                side: if c.side == DiffSide::Old {
                    ReviewSide::Left
                } else {
                    ReviewSide::Right
                },
                body: c.body.clone(),
            })
            .collect();
        let head = gh
            .pr_commits(pr.number)
            .await?
            .last()
            .map(|c| c.oid.clone());
        let inline = match head {
            Some(sha) => gh.pr_review_comments(pr.number, &sha, &inputs).await,
            None => Err(berth_gh::GhError::Failed {
                args: "pr commits".into(),
                status: 1,
                stderr: "no commits".into(),
            }),
        };
        if let Err(e) = inline {
            tracing::info!(error = %e, "inline review failed; posting one comment");
            let body = comments
                .iter()
                .map(|c| {
                    format!(
                        "- `{}:{}` ({}): {}",
                        c.path,
                        c.line,
                        c.side.as_str(),
                        c.body
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            gh.pr_comment(pr.number, &format!("Review comments from Berth:\n\n{body}"))
                .await?;
        }
        let ids: Vec<String> = comments.into_iter().map(|c| c.id).collect();
        self.core().store.mark_comments_posted(&ids)?;
        self.emit_comments(workflow_id);
        self.toast(
            Some(repo_id),
            ToastLevel::Success,
            format!("Posted to PR #{}", pr.number),
        );
        Ok(())
    }
}

fn with_comments(body: &str, comments: &[berth_gh::Comment]) -> String {
    let mut out = body.trim().to_owned();
    for c in comments {
        out.push_str(&format!(
            "\n\n---\n{} commented:\n{}",
            c.author.as_deref().unwrap_or("someone"),
            c.body.trim()
        ));
    }
    out
}

fn slot_name(origin: &Origin) -> String {
    let base = match origin.kind {
        OriginKind::GhIssue => format!("#{} {}", origin.r#ref, origin.title),
        OriginKind::GhPr => format!("PR #{} {}", origin.r#ref, origin.title),
        _ => origin.title.clone(),
    };
    let trimmed: String = base.chars().take(48).collect();
    if trimmed.trim().is_empty() {
        format!("slot {}", short(&new_id()))
    } else {
        trimmed
    }
}

/// `slots/examples` next to the app bundle (release) or in the workspace (dev).
fn bundled_examples_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("../Resources/slots")))
        .filter(|d| d.exists())
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../slots/examples"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use berth_slot::SlotTypeSource;

    fn types(names: &[&str]) -> Vec<SlotType> {
        names
            .iter()
            .map(|n| SlotType {
                name: (*n).to_owned(),
                description: String::new(),
                agent_placement: Placement::Host,
                dir: PathBuf::from(n),
                source: SlotTypeSource::Global,
            })
            .collect()
    }

    fn pick(types: &[SlotType], last: Option<&str>, configured: Option<&str>) -> Option<String> {
        default_slot_type(types, last, configured).map(|t| t.name.clone())
    }

    #[test]
    fn the_last_used_type_comes_first() {
        let t = types(&["docker", "vm", "worktree"]);
        assert_eq!(pick(&t, Some("vm"), Some("docker")).as_deref(), Some("vm"));
        assert_eq!(pick(&t, None, Some("docker")).as_deref(), Some("docker"));
        assert_eq!(pick(&t, None, None).as_deref(), Some("worktree"));
    }

    #[test]
    fn a_type_that_is_gone_is_passed_over() {
        let t = types(&["docker", "vm"]);
        assert_eq!(pick(&t, Some("lima"), Some("vm")).as_deref(), Some("vm"));
        assert_eq!(
            pick(&t, Some("lima"), Some("gone")).as_deref(),
            Some("docker")
        );
        assert_eq!(pick(&[], Some("vm"), None), None);
    }
}
