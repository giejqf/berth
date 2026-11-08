//! Messages between slots' agents (`message_slot`, ADR 0021): the backend
//! finds the slot, starts its agent when the message wakes it, and tells the
//! sender where the message went. The rules are `berth_core::messages`.

use std::sync::Arc;

use anyhow::Result;
use berth_core::messages::{self, Delivery};
use berth_core::{Engine, MessagePolicy, Recipient, Sender, Slot, Workflow, WorkflowState};
use berth_protocol::{AppToSidecar, SessionTurn, SlotMessageRequest, SlotMessageSent};

use super::{Shared, lock};

impl Shared {
    /// Route workflow `from`'s `message_slot` call and answer it.
    pub(super) async fn route_slot_message(
        self: Arc<Self>,
        from: String,
        request: SlotMessageRequest,
    ) {
        let (ok, text) = match self.post_slot_message(&from, &request).await {
            Ok(Ok(text)) => (true, text),
            Ok(Err(why)) => (false, why),
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "slot message failed");
                (false, format!("The message was not sent: {e:#}"))
            }
        };
        if let Some(s) = lock(&self.sessions).get(&from).cloned() {
            let _ = s.send(AppToSidecar::SlotMessageSent(SlotMessageSent {
                request_id: request.request_id,
                ok,
                text,
            }));
        }
    }

    /// Post the message; `Ok(Err(why))` when it goes nowhere (no such slot),
    /// in words for the agent.
    async fn post_slot_message(
        self: &Arc<Self>,
        from: &str,
        request: &SlotMessageRequest,
    ) -> Result<Result<String, String>> {
        // A session that started before Settings turned messages off still
        // has the tool (ADR 0032).
        let policy = self.agent_limits().messages;
        if !policy.allows() {
            return Ok(Err(berth_core::policy::MESSAGES_OFF.to_owned()));
        }
        let (sender, from_slot, slots) = {
            let core = self.core();
            let wf = core.store.workflow(from)?;
            let slot = core.store.slot(&wf.slot_id)?;
            let slots: Vec<(Slot, Option<Workflow>)> = core
                .store
                .live_slots(&slot.repo_id)?
                .into_iter()
                .map(|s| {
                    let w = core.store.workflows_for_slot(&s.id)?.pop();
                    Ok((s, w))
                })
                .collect::<Result<_>>()?;
            (wf, slot, slots)
        };
        let (to_slot, to) = match messages::find_slot(&slots, &sender.slot_id, &request.to) {
            Ok((s, w)) => (s.clone(), w.clone()),
            Err(why) => return Ok(Err(why)),
        };
        let to_name = messages::slot_name(&to_slot, Some(&to));
        let from_name = messages::slot_name(&from_slot, Some(&sender));
        let hop = Engine::message_hop(&self.core().store, from)?;
        let live = lock(&self.sessions).contains_key(&to.id);
        let mut delivery = messages::plan(to.state, to_slot.state, hop, live, policy);
        // Waking an idle agent may need its session started first; if it
        // can't start, the message waits for the user.
        if delivery == Delivery::Wake
            && let Err(e) = self.ensure_session(&to.id).await
        {
            delivery = Delivery::Hold(format!("its agent could not start: {e:#}"));
        }
        let posted = {
            let mut core = self.core();
            let super::Core { store, engine } = &mut *core;
            engine.post_message(
                store,
                &Sender {
                    workflow: &sender,
                    name: &from_name,
                    branch: &from_slot.branch,
                },
                &Recipient {
                    workflow: &to,
                    name: &to_name,
                },
                &request.message,
                hop,
                &delivery,
            )?
        };
        if let Some((text, uuid)) = posted.turn {
            self.send_turn(&to.id, text, uuid)?;
        }
        self.emit_workflow(from);
        self.emit_workflow(&to.id);
        self.emit_slots(&to_slot.repo_id);
        let waits = to.state == WorkflowState::AwaitingAnswer;
        Ok(Ok(messages::sent_text(&to_name, &delivery, waits)))
    }

    /// The user sends a held message on to its agent: into its round, or one
    /// of its own (a closed workflow goes on, as a prompt would take it).
    pub(super) async fn release_message(
        self: &Arc<Self>,
        workflow_id: &str,
        message_id: &str,
    ) -> Result<()> {
        self.continue_closed(workflow_id).await?;
        let (wf, slot) = {
            let core = self.core();
            let wf = core.store.workflow(workflow_id)?;
            let slot = core.store.slot(&wf.slot_id)?;
            (wf, slot)
        };
        let live = lock(&self.sessions).contains_key(workflow_id);
        // The user sends it: no guard to apply, whatever Settings says.
        let delivery = messages::plan(wf.state, slot.state, 0, live, MessagePolicy::Always);
        if delivery == Delivery::Wake {
            self.ensure_session(workflow_id).await?;
        }
        let (text, uuid) = {
            let mut core = self.core();
            let super::Core { store, engine } = &mut *core;
            engine.release_message(store, workflow_id, message_id, &delivery)?
        };
        self.send_turn(workflow_id, text, uuid)?;
        self.refresh_message(message_id, workflow_id);
        Ok(())
    }

    /// The user dismisses a held message.
    pub(super) fn dismiss_message(&self, workflow_id: &str, message_id: &str) -> Result<()> {
        Engine::dismiss_message(&self.core().store, workflow_id, message_id)?;
        self.refresh_message(message_id, workflow_id);
        Ok(())
    }

    fn send_turn(&self, workflow_id: &str, text: String, uuid: String) -> Result<()> {
        let session = lock(&self.sessions)
            .get(workflow_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("the agent is not running"))?;
        session.send(AppToSidecar::SessionTurn(SessionTurn {
            text,
            attachments: vec![],
            uuid: Some(uuid),
        }))
    }

    /// Both flows that show message `message_id`, and the rail.
    fn refresh_message(&self, message_id: &str, workflow_id: &str) {
        let flows: Vec<String> = self
            .core()
            .store
            .message_nodes(message_id)
            .map(|nodes| nodes.into_iter().map(|n| n.workflow_id).collect())
            .unwrap_or_default();
        for wf in &flows {
            self.emit_workflow(wf);
        }
        if let Ok(repo) = self.repo_of_workflow(workflow_id) {
            self.emit_slots(&repo);
        }
    }
}
