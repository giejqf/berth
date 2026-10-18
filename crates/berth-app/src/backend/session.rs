//! One sidecar per active workflow: start/resume, pump events through the
//! engine, and execute the resulting effects.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use berth_core::{AnswerDelivery, Effect, WorkflowState, prompt};
use berth_protocol::{
    AppToSidecar, ConnectorSkill, QuestionAnswer, SessionResume, SessionStart, SessionTurn,
    TurnAttachment,
};
use berth_sidecar_client::{Sidecar, SidecarEvent};
use berth_slot::Placement;
use tokio::sync::mpsc::UnboundedReceiver;

use super::{Shared, ToastLevel, UiEvent, lock};

pub(crate) struct Session {
    sidecar: Sidecar,
}

impl Session {
    pub(crate) fn send(&self, msg: AppToSidecar) -> Result<()> {
        self.sidecar.send(msg).map(|_| ()).map_err(Into::into)
    }

    pub(crate) async fn shutdown(&self) {
        self.sidecar.shutdown(Duration::from_secs(3)).await;
    }
}

impl Shared {
    /// The workflow's live session, starting (or resuming) one if needed.
    pub(super) async fn ensure_session(
        self: &Arc<Self>,
        workflow_id: &str,
    ) -> Result<Arc<Session>> {
        let gate = lock(&self.session_locks)
            .entry(workflow_id.to_owned())
            .or_default()
            .clone();
        let _guard = gate.lock().await;
        if let Some(s) = lock(&self.sessions).get(workflow_id) {
            return Ok(s.clone());
        }

        let (wf, slot, repo, model) = {
            let core = self.core();
            let wf = core.store.workflow(workflow_id)?;
            let slot = core.store.slot(&wf.slot_id)?;
            let repo = core.store.repo(&slot.repo_id)?;
            let model = core
                .store
                .setting("model")?
                .filter(|m| !m.trim().is_empty());
            (wf, slot, repo, model)
        };
        if !wf.state.is_open() {
            return Err(anyhow!("this workflow is closed"));
        }
        let checkout = slot
            .checkout_path
            .clone()
            .ok_or_else(|| anyhow!("the slot has no checkout yet"))?;
        let script = self.slot_script(&slot)?;
        let connectors = self.enabled_connectors();

        // Install connector skills into the checkout (§8.2).
        for c in &connectors {
            match c.skill_install() {
                Ok(Some(install)) => {
                    let out = script
                        .exec(
                            Some(&checkout),
                            "/bin/sh",
                            &["-c".to_owned(), install.script.clone()],
                            Some(install.stdin),
                        )
                        .await?;
                    if !out.success() {
                        tracing::warn!(connector = %c.name, stderr = %String::from_utf8_lossy(&out.stderr), "skill install failed");
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(connector = %c.name, error = %e, "skill unreadable"),
            }
        }

        let host = slot.placement != Placement::Inside.as_str();
        let mut env: BTreeMap<String, String> = script.env.vars().into_iter().collect();
        if host {
            let base = std::env::var("PATH").unwrap_or_default();
            env.insert(
                "PATH".into(),
                berth_connectors::path_with_connectors(&connectors, &base),
            );
            env.insert(
                "BERTH_SLOT_SCRIPT".into(),
                script.ty.script().to_string_lossy().into_owned(),
            );
        }
        let slot_exec = host.then(|| script.ty.script().to_string_lossy().into_owned());
        let system_prompt = prompt::session_context(&prompt::SessionContext {
            repo_name: &repo.name(),
            slot_name: &slot.name,
            slot_type: &slot.slot_type,
            placement: &slot.placement,
            branch: &slot.branch,
            base_ref: &slot.base_ref,
            origin: &wf.origin,
            slot_exec: slot_exec
                .as_deref()
                .filter(|_| slot.slot_type != "worktree"),
        });

        let cmd = if host {
            let mut cmd = tokio::process::Command::new(&self.launch.program);
            cmd.args(&self.launch.args)
                .current_dir(&checkout)
                .envs(&env);
            cmd
        } else {
            let (program, args) = self.launch.inside_args();
            script.exec_command(None, &program, &args)
        };
        let (sidecar, events) = Sidecar::spawn(cmd)?;
        let session = Arc::new(Session { sidecar });

        let start = SessionStart {
            workflow_id: wf.id.clone(),
            cwd: checkout,
            system_prompt,
            env: if host { env } else { BTreeMap::new() },
            allowed_tools: vec![],
            connector_skills: connectors
                .iter()
                .map(|c| ConnectorSkill {
                    name: c.name.clone(),
                    description: c.description.clone(),
                })
                .collect(),
            model,
        };
        let msg = match wf.claude_session_id.clone() {
            Some(id) => AppToSidecar::SessionResume(SessionResume {
                claude_session_id: id,
                start,
            }),
            None => AppToSidecar::SessionStart(start),
        };
        self.core().engine.session_started(&wf.id);
        session.send(msg)?;
        lock(&self.sessions).insert(wf.id.clone(), session.clone());
        tokio::spawn(self.clone().pump(wf.id.clone(), events));
        Ok(session)
    }

    pub(super) async fn send_prompt(
        self: &Arc<Self>,
        workflow_id: &str,
        text: &str,
        attachments: &[std::path::PathBuf],
    ) -> Result<()> {
        let session = self.ensure_session(workflow_id).await?;
        let slot = {
            let core = self.core();
            core.store
                .slot(&core.store.workflow(workflow_id)?.slot_id)?
        };
        let mut files = Vec::with_capacity(attachments.len());
        for path in attachments {
            files.push(self.attach(&slot, workflow_id, path).await?);
        }
        let images: Vec<TurnAttachment> = files
            .iter()
            .filter(|f| berth_core::files::is_inline_image(&f.mime))
            .filter_map(|f| {
                Some(TurnAttachment {
                    path: f.slot_path.clone()?,
                    mime: f.mime.clone(),
                })
            })
            .collect();
        let submission = {
            let mut core = self.core();
            let super::Core { store, engine } = &mut *core;
            engine.submit_prompt(store, workflow_id, text, files)?
        };
        session.send(AppToSidecar::SessionTurn(SessionTurn {
            text: submission.turn_text,
            attachments: images,
        }))?;
        self.emit_workflow(workflow_id);
        self.emit_comments(workflow_id);
        Ok(())
    }

    pub(super) async fn answer(
        self: &Arc<Self>,
        workflow_id: &str,
        question_id: &str,
        text: &str,
    ) -> Result<()> {
        let session = self.ensure_session(workflow_id).await?;
        let delivery = {
            let mut core = self.core();
            let super::Core { store, engine } = &mut *core;
            engine.answer(store, workflow_id, question_id, text)?
        };
        match delivery {
            AnswerDelivery::Answer { question_id, text } => {
                session.send(AppToSidecar::QuestionAnswer(QuestionAnswer {
                    question_id,
                    text,
                }))?;
            }
            AnswerDelivery::Turn(text) => {
                session.send(AppToSidecar::SessionTurn(SessionTurn {
                    text,
                    attachments: vec![],
                }))?;
            }
        }
        self.emit_workflow(workflow_id);
        Ok(())
    }

    async fn pump(
        self: Arc<Self>,
        workflow_id: String,
        mut events: UnboundedReceiver<SidecarEvent>,
    ) {
        while let Some(event) = events.recv().await {
            match event {
                SidecarEvent::Message(msg) => {
                    let result = {
                        let mut core = self.core();
                        let super::Core { store, engine } = &mut *core;
                        engine.apply(store, &workflow_id, &msg.body)
                    };
                    match result {
                        Ok(fx) => self.run_effects(&workflow_id, fx),
                        Err(e) => {
                            tracing::error!(error = %e, kind = msg.body.type_name(), "apply failed");
                        }
                    }
                }
                SidecarEvent::ProtocolError { line, error } => {
                    tracing::warn!(%error, line = %line.chars().take(200).collect::<String>(), "sidecar protocol error");
                }
                SidecarEvent::Exited { code, stderr_tail } => {
                    lock(&self.sessions).remove(&workflow_id);
                    let error = (code != Some(0)).then(|| {
                        let tail: String = stderr_tail
                            .lines()
                            .rev()
                            .take(5)
                            .collect::<Vec<_>>()
                            .join(" | ");
                        format!(
                            "Agent session exited ({}). {tail}",
                            code.map_or("signal".into(), |c| c.to_string())
                        )
                    });
                    let fx = {
                        let mut core = self.core();
                        let super::Core { store, engine } = &mut *core;
                        engine.sidecar_exited(store, &workflow_id, error.as_deref())
                    };
                    match fx {
                        Ok(fx) => self.run_effects(&workflow_id, fx),
                        Err(e) => tracing::error!(error = %e, "sidecar exit bookkeeping failed"),
                    }
                    self.emit(UiEvent::Activity {
                        workflow_id: workflow_id.clone(),
                        activity: None,
                    });
                    self.emit_workflow(&workflow_id);
                    break;
                }
            }
        }
    }

    pub(super) fn run_effects(self: &Arc<Self>, workflow_id: &str, fx: Vec<Effect>) {
        let mut refresh_flow = false;
        let mut refresh_slots = false;
        for effect in fx {
            match effect {
                Effect::Snapshot {
                    workflow_id,
                    section_no,
                    kind,
                } => {
                    tokio::spawn(self.clone().take_snapshot(workflow_id, section_no, kind));
                }
                Effect::BranchSnapshot {
                    workflow_id,
                    branch_id,
                    at_collect,
                } => {
                    tokio::spawn(self.clone().take_branch_snapshot(
                        workflow_id,
                        branch_id,
                        at_collect,
                    ));
                }
                Effect::OpenReview { workflow_id } => {
                    self.emit(UiEvent::OpenReview { workflow_id });
                }
                Effect::FetchArtifact {
                    workflow_id,
                    node_id,
                    slot_path,
                    size_bytes,
                } => {
                    tokio::spawn(self.clone().fetch_artifact(
                        workflow_id,
                        node_id,
                        slot_path,
                        size_bytes,
                    ));
                }
                Effect::LinkOutcome {
                    workflow_id,
                    outcome,
                } => {
                    tokio::spawn(self.clone().link_outcome(workflow_id, outcome));
                }
                Effect::ConfirmSlotRequest {
                    workflow_id,
                    request,
                } => {
                    if let Ok(repo_id) = self.repo_of_workflow(&workflow_id) {
                        self.emit(UiEvent::SlotRequest {
                            repo_id,
                            workflow_id,
                            request,
                        });
                    }
                }
                Effect::Error { message, fatal, .. } => {
                    let repo = self.repo_of_workflow(workflow_id).ok();
                    let level = if fatal {
                        ToastLevel::Error
                    } else {
                        ToastLevel::Info
                    };
                    self.toast(repo, level, message);
                }
                Effect::FlowChanged { .. } => refresh_flow = true,
                Effect::Cost { .. } => {
                    refresh_flow = true;
                    refresh_slots = true;
                }
                Effect::StateChanged { workflow_id, state } => {
                    refresh_flow = true;
                    refresh_slots = true;
                    if state != WorkflowState::Running {
                        self.emit(UiEvent::Activity {
                            workflow_id: workflow_id.clone(),
                            activity: None,
                        });
                    }
                    if state == WorkflowState::Closed {
                        self.release_slot(&workflow_id);
                    }
                }
                Effect::Activity {
                    workflow_id,
                    activity,
                } => {
                    self.emit(UiEvent::Activity {
                        workflow_id,
                        activity,
                    });
                }
            }
        }
        if refresh_flow {
            self.emit_workflow(workflow_id);
        }
        if refresh_slots && let Ok(repo) = self.repo_of_workflow(workflow_id) {
            self.emit_slots(&repo);
        }
    }

    /// A closed workflow frees its slot: `Running -> Ready` (§6.3).
    fn release_slot(&self, workflow_id: &str) {
        let core = self.core();
        if let Ok(wf) = core.store.workflow(workflow_id)
            && let Ok(slot) = core.store.slot(&wf.slot_id)
            && slot.state == berth_core::SlotState::Running
        {
            let _ = core
                .store
                .transition_slot(&slot.id, berth_core::SlotState::Ready, None);
        }
    }
}
