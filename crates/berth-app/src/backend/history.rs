//! The repo's session history (ADR 0010): Claude Code's sessions for the repo
//! and its worktrees (listed by a short-lived sidecar, since the SDK keeps the
//! list) together with Berth's workflows, and opening one of them in a slot.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use berth_core::Slot;
use berth_protocol::{
    AppToSidecar, Origin, OriginKind, PastSession, SessionsList, SessionsRename, SidecarToApp,
};
use berth_sidecar_client::{Sidecar, SidecarEvent};
use berth_slot::Placement;

use super::{HistoryEntry, HistoryOpen, HistoryWorkflow, Shared, UiEvent};

/// How long the listing may take before the history shows Berth's side only.
const LIST_TIMEOUT: Duration = Duration::from_secs(20);

impl Shared {
    pub(super) async fn load_history(self: &Arc<Self>, repo_id: &str) -> Result<()> {
        let repo_path = self.repo_path(repo_id)?;
        let listed = self.list_sessions(&repo_path).await;
        let (sessions, unlisted) = match listed {
            Ok(s) => (s, None),
            Err(e) => {
                tracing::warn!(error = %e, "listing Claude Code sessions");
                (Vec::new(), Some(e.to_string()))
            }
        };
        let entries = self.history_entries(repo_id, sessions)?;
        self.emit(UiEvent::History {
            repo_id: repo_id.to_owned(),
            entries,
            unlisted,
        });
        Ok(())
    }

    /// Claude Code's sessions for `dir` and its git worktrees, newest first.
    async fn list_sessions(&self, dir: &Path) -> Result<Vec<PastSession>> {
        let mut cmd = self.launch.command()?;
        cmd.current_dir(dir);
        let (sidecar, mut events) = Sidecar::spawn(cmd)?;
        sidecar.send(AppToSidecar::SessionsList(SessionsList {
            dir: dir.to_string_lossy().into_owned(),
        }))?;
        let reply = tokio::time::timeout(LIST_TIMEOUT, async {
            while let Some(event) = events.recv().await {
                match event {
                    SidecarEvent::Message(m) => match m.body {
                        SidecarToApp::SessionsListed(l) => return Ok(l.sessions),
                        SidecarToApp::SessionError(e) => return Err(anyhow!(e.message)),
                        _ => {}
                    },
                    SidecarEvent::ProtocolError { .. } => {}
                    SidecarEvent::Exited { .. } => break,
                }
            }
            Err(anyhow!("the sidecar exited without listing sessions"))
        })
        .await
        .unwrap_or_else(|_| Err(anyhow!("listing sessions timed out")));
        sidecar.shutdown(Duration::from_secs(2)).await;
        reply
    }

    /// Give the slot's session the slot's name in Claude Code's history
    /// (`renameSession`, ADR 0016), or its workflow's title when the name was
    /// cleared. Best effort: only where the transcript is on this Mac (a host
    /// slot), and a failure is only logged.
    pub(super) async fn retitle_session(&self, slot: &Slot) {
        if slot.placement == Placement::Inside.as_str() {
            return;
        }
        let latest = self
            .core()
            .store
            .workflows_for_slot(&slot.id)
            .ok()
            .and_then(|mut w| w.pop());
        let (Some(wf), Some(dir)) = (latest, slot.checkout_path.clone()) else {
            return;
        };
        let Some(session_id) = wf.claude_session_id.clone() else {
            return;
        };
        let title = slot
            .custom_name
            .clone()
            .unwrap_or_else(|| wf.origin.title.clone());
        let renamed = async {
            let mut cmd = self.launch.command()?;
            cmd.current_dir(&dir);
            let (sidecar, mut events) = Sidecar::spawn(cmd)?;
            sidecar.send(AppToSidecar::SessionsRename(SessionsRename {
                session_id,
                title,
                dir: dir.clone(),
            }))?;
            let reply = tokio::time::timeout(LIST_TIMEOUT, async {
                while let Some(event) = events.recv().await {
                    if let SidecarEvent::Message(m) = event
                        && let SidecarToApp::SessionsRenamed(r) = m.body
                    {
                        return r.error.map_or(Ok(()), |e| Err(anyhow!(e)));
                    }
                }
                Err(anyhow!("the sidecar exited without renaming"))
            })
            .await
            .unwrap_or_else(|_| Err(anyhow!("renaming timed out")));
            sidecar.shutdown(Duration::from_secs(2)).await;
            reply
        };
        if let Err(e) = renamed.await {
            tracing::warn!(error = %e, "renaming the Claude Code session");
        }
    }

    /// Berth's workflows of the repo, each with Claude Code's record of its
    /// session when there is one, and the sessions Berth has no workflow for.
    fn history_entries(
        &self,
        repo_id: &str,
        sessions: Vec<PastSession>,
    ) -> Result<Vec<HistoryEntry>> {
        let core = self.core();
        let mut by_id: HashMap<String, PastSession> = sessions
            .into_iter()
            .map(|s| (s.session_id.clone(), s))
            .collect();
        let mut entries = Vec::new();
        let mut shown: HashMap<String, String> = HashMap::new();
        for (wf, slot) in core.store.repo_workflows(repo_id)? {
            let facts = core.store.workflow_facts(&wf.id)?;
            let session = wf
                .claude_session_id
                .as_ref()
                .and_then(|id| by_id.remove(id));
            let last_active = [
                Some(wf.created_at),
                wf.closed_at,
                facts.last_round_at,
                session.as_ref().map(|s| s.last_modified),
            ]
            .into_iter()
            .flatten()
            .max()
            .unwrap_or(wf.created_at);
            if slot.state != berth_core::SlotState::Destroyed {
                shown.insert(slot.id.clone(), wf.id.clone());
            }
            entries.push(HistoryEntry {
                title: wf.origin.title.clone(),
                session,
                workflow: Some(HistoryWorkflow {
                    workflow: wf,
                    slot,
                    shown: false,
                    facts,
                }),
                last_active,
            });
        }
        // `repo_workflows` lists each slot's in hosting order: its last is shown.
        for e in &mut entries {
            if let Some(w) = &mut e.workflow {
                w.shown = shown.get(&w.slot.id) == Some(&w.workflow.id);
            }
        }
        entries.extend(by_id.into_values().map(|s| HistoryEntry {
            title: s.title.clone(),
            last_active: s.last_modified,
            session: Some(s),
            workflow: None,
        }));
        entries.sort_by_key(|e| std::cmp::Reverse(e.last_active));
        Ok(entries)
    }

    /// Open a history entry in `slot_id` (ADR 0010): the slot's open workflow
    /// closes first, as for reuse; a Berth workflow moves in, a Claude Code
    /// session becomes a workflow there. Either resumes on the next prompt.
    pub(super) async fn open_from_history(
        self: &Arc<Self>,
        repo_id: &str,
        slot_id: &str,
        open: HistoryOpen,
    ) -> Result<()> {
        let (target, latest) = {
            let core = self.core();
            (
                core.store.slot(slot_id)?,
                core.store.workflows_for_slot(slot_id)?.pop(),
            )
        };
        if target.repo_id != repo_id {
            return Err(anyhow!("that slot belongs to another repository"));
        }
        let host = target.placement != Placement::Inside.as_str();
        // What moves in, and from where; checked before anything closes.
        let moving = match &open {
            HistoryOpen::Workflow(id) => {
                let core = self.core();
                let wf = core.store.workflow(id)?;
                let home = core.store.slot(&wf.slot_id)?;
                if home.repo_id != repo_id {
                    return Err(anyhow!("that session belongs to another repository"));
                }
                if latest.as_ref().is_some_and(|l| l.id == wf.id) {
                    return Ok(());
                }
                let home_shows = core
                    .store
                    .workflows_for_slot(&home.id)?
                    .pop()
                    .is_some_and(|l| l.id == wf.id);
                if wf.state.is_open() && home_shows {
                    return Err(anyhow!(
                        "It is open in another slot; finish or close it there first."
                    ));
                }
                if home.id != target.id && (home.placement == Placement::Inside.as_str() || !host) {
                    return Err(anyhow!(
                        "Its transcript stays where the agent ran, inside {}'s environment.",
                        home.name
                    ));
                }
                Some(wf)
            }
            HistoryOpen::Session(_) if !host => {
                return Err(anyhow!(
                    "This slot runs the agent inside its environment, where Claude Code's \
                     sessions on this Mac aren't."
                ));
            }
            HistoryOpen::Session(_) => None,
        };
        let closes = target
            .reuse(latest.as_ref())
            .map_err(|blocked| anyhow!(blocked.reason()))?;
        if let Some(old) = latest.filter(|_| closes) {
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
        let target = self.refresh_branch(&target).await?;
        // A workflow moving in brings no agent along: its next session starts
        // in this slot's checkout.
        if let HistoryOpen::Workflow(id) = &open {
            let session = self.take_session(id);
            if let Some(s) = session {
                s.shutdown().await;
            }
        }
        let (workflow_id, fx) = {
            let mut core = self.core();
            let super::Core { store, engine } = &mut *core;
            match (&open, moving) {
                (HistoryOpen::Workflow(id), Some(_)) => {
                    (id.clone(), engine.reopen(store, id, &target)?)
                }
                (HistoryOpen::Session(s), _) => {
                    let origin = Origin {
                        kind: OriginKind::Session,
                        r#ref: s.session_id.clone(),
                        title: s.title.clone(),
                        body: s.first_prompt.clone().unwrap_or_default(),
                        custom_prompt: None,
                        url: None,
                    };
                    let (wf, fx) = engine.adopt_session(
                        store,
                        &target,
                        origin,
                        &s.session_id,
                        (s.cwd.clone(), s.git_branch.clone()),
                    )?;
                    (wf.id, fx)
                }
                (HistoryOpen::Workflow(_), None) => return Ok(()),
            }
        };
        if target.state == berth_core::SlotState::Ready {
            self.core()
                .store
                .transition_slot(&target.id, berth_core::SlotState::Running, None)?;
        }
        self.run_effects(&workflow_id, fx);
        // The slot it came from shows what it hosted before.
        self.emit_slots(repo_id);
        self.emit_workflow(&workflow_id);
        Ok(())
    }

    /// Record the branch the slot's checkout is on now; earlier work, or a
    /// person in its terminal, may have moved it. Asked through the slot
    /// (`slot.sh exec -- git`, §6.2), as every git question about a checkout is.
    pub(super) async fn refresh_branch(&self, slot: &Slot) -> Result<Slot> {
        let script = self.slot_script(slot)?;
        if let Ok(out) = berth_git::git_ok(&script, &["branch", "--show-current"], &[]).await {
            let branch = out.trim();
            if !branch.is_empty() && branch != slot.branch {
                self.core().store.set_slot_branch(&slot.id, branch)?;
            }
        }
        Ok(self.core().store.slot(&slot.id)?)
    }
}
