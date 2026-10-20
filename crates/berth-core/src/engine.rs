//! The workflow state machine: applies sidecar messages and user actions to the
//! store and returns the side effects the app must carry out.
//!
//! The engine is synchronous and executor-agnostic. It never touches git, the
//! slot, or the sidecar itself; it asks for that through [`Effect`]s.

use std::collections::{HashMap, HashSet};

use berth_protocol::{
    BranchStatus, CollectResults, Decision, Finding, FindingAction, NodePayload, Origin, Outcome,
    Prompt, Question, Reopened, Review, ReviewComment, Rewind, SidecarToApp, SlotRequest,
    SubagentSpawn, Summary, TerminalKind,
};

use crate::activity::RoundActivity;
use crate::error::{CoreError, Result};
use crate::ids::{new_id, now_ms};
use crate::model::{Branch, BranchState, Node, Slot, SnapshotKind, Workflow, WorkflowState};
use crate::prompt::{answer_as_turn, review_as_turn, review_decisions, serialize_prompt};
use crate::store::{NewRawEvent, Store};

/// Work the app performs on the engine's behalf.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Take a tree snapshot, then call [`Engine::record_snapshot`].
    Snapshot {
        workflow_id: String,
        section_no: u32,
        kind: SnapshotKind,
    },
    /// Best-effort branch snapshot at spawn or collect (§5.4).
    BranchSnapshot {
        workflow_id: String,
        branch_id: String,
        at_collect: bool,
    },
    /// Open the review pane (on Summary and Outcome, §11).
    OpenReview {
        workflow_id: String,
    },
    /// Open the review pane on its Findings tab (a review arrived).
    ShowFindings {
        workflow_id: String,
    },
    /// Link the outcome to a GitHub entity (kind = pr).
    LinkOutcome {
        workflow_id: String,
        outcome: Outcome,
    },
    /// Copy an artifact out of the slot into app storage, then call
    /// [`Engine::artifact_fetched`].
    FetchArtifact {
        workflow_id: String,
        node_id: String,
        slot_path: String,
        size_bytes: u64,
    },
    /// Ask the user to approve `request_slot`.
    ConfirmSlotRequest {
        workflow_id: String,
        request: SlotRequest,
    },
    Error {
        workflow_id: String,
        message: String,
        fatal: bool,
    },
    /// The flow (nodes, branches, rounds) changed; refresh views.
    FlowChanged {
        workflow_id: String,
    },
    StateChanged {
        workflow_id: String,
        state: WorkflowState,
    },
    Cost {
        workflow_id: String,
        total_usd: f64,
    },
    /// What the agent is doing right now, for the live status line.
    /// The open round's activity changed; `None` when the round ended.
    Activity {
        workflow_id: String,
        activity: Option<RoundActivity>,
    },
}

/// How a question's answer reaches the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerDelivery {
    /// The `ask_user` call is still blocked in this sidecar: send `question.answer`.
    Answer { question_id: String, text: String },
    /// The `present_review` call is still blocked: send `review.answer`.
    Review { review_id: String, text: String },
    /// The sidecar restarted since the question was asked: send a new turn.
    Turn(String),
}

/// A submitted Prompt and the turn text to send.
#[derive(Debug, Clone, PartialEq)]
pub struct Submission {
    pub node: Node,
    pub turn_text: String,
    /// The user message's uuid in Claude Code's transcript (ADR 0016).
    pub turn_uuid: String,
}

/// What a rewind leaves for the user: the message that was rewound, to edit
/// and send again.
#[derive(Debug, Clone, PartialEq)]
pub struct Rewound {
    pub prompt: Prompt,
    /// The workflow was closed (its Outcome was dropped): its slot hosts it again.
    pub reopened: bool,
}

/// Per-workflow state that lives only as long as one sidecar process.
#[derive(Debug, Default)]
struct Live {
    /// The session has not answered yet: its first assistant message settles
    /// a rewind (ADR 0009).
    fresh: bool,
    latest_raw: Option<i64>,
    /// Last raw event id already covered by a node, per lane (`None` = main).
    lane_cursor: HashMap<Option<String>, i64>,
    /// Questions and reviews whose `ask_user` / `present_review` call is
    /// blocked in the current sidecar.
    blocked: HashSet<String>,
    /// What the open round is doing (the live card, the rail's second line).
    activity: Option<RoundActivity>,
}

#[derive(Debug, Default)]
pub struct Engine {
    live: HashMap<String, Live>,
}

impl Engine {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a workflow in `slot_id` with its Origin node.
    pub fn create_workflow(store: &Store, slot_id: &str, origin: Origin) -> Result<Workflow> {
        if let Some(active) = store.active_workflow(slot_id)? {
            return Err(CoreError::InvalidTransition(format!(
                "slot {slot_id} already hosts workflow {}",
                active.id
            )));
        }
        let wf = Workflow {
            id: new_id(),
            slot_id: slot_id.to_owned(),
            origin: origin.clone(),
            state: WorkflowState::Starting,
            claude_session_id: None,
            section_no: 1,
            total_cost_usd: 0.0,
            last_error: None,
            created_at: now_ms(),
            closed_at: None,
        };
        store.insert_workflow(&wf)?;
        store.insert_node(&wf.id, None, None, NodePayload::Origin(origin), None)?;
        Ok(wf)
    }

    /// Close an open workflow without an outcome, so its slot can host a new one
    /// (reuse, §6.3): its open round ends as interrupted, running subagents are
    /// cancelled. Stop its sidecar first.
    pub fn close_for_reuse(&mut self, store: &Store, workflow_id: &str) -> Result<Vec<Effect>> {
        let mut fx = Vec::new();
        if let Some(round) = store.open_round(workflow_id)? {
            let now = now_ms();
            store.end_round(
                &round.id,
                TerminalKind::Interrupted,
                round.nudges,
                round.cost_usd,
                now - round.started_at,
                now,
            )?;
        }
        self.cancel_open_branches(store, workflow_id, &mut fx)?;
        self.set_state(store, workflow_id, WorkflowState::Closed, &mut fx)?;
        self.live.remove(workflow_id);
        fx.push(Effect::FlowChanged {
            workflow_id: workflow_id.to_owned(),
        });
        Ok(fx)
    }

    /// A new sidecar process started for `workflow_id`; forget per-process state.
    pub fn session_started(&mut self, workflow_id: &str) {
        self.live.insert(
            workflow_id.to_owned(),
            Live {
                fresh: true,
                ..Live::default()
            },
        );
    }

    /// Open workflow `workflow_id` from the repo's history in `slot` (ADR
    /// 0010): it moves there (a closed one opens again), a new section starts
    /// in the slot's checkout (its start snapshot is an [`Effect::Snapshot`]),
    /// and a Reopened node says where the conversation is now. Close the
    /// slot's open workflow first.
    pub fn reopen(&mut self, store: &Store, workflow_id: &str, slot: &Slot) -> Result<Vec<Effect>> {
        if let Some(open) = store.active_workflow(&slot.id)?
            && open.id != workflow_id
        {
            return Err(CoreError::InvalidTransition(format!(
                "slot {} already hosts workflow {}",
                slot.id, open.id
            )));
        }
        let wf = store.workflow(workflow_id)?;
        if wf.state.is_busy() {
            return Err(CoreError::InvalidTransition(
                "the agent is working on it".to_owned(),
            ));
        }
        let before = store.slot(&wf.slot_id)?;
        let moved = before.id != slot.id;
        store.move_workflow(workflow_id, &slot.id, now_ms())?;
        let mut fx = Vec::new();
        if wf.state == WorkflowState::Closed {
            store.reopen_workflow(workflow_id)?;
            fx.push(Effect::StateChanged {
                workflow_id: workflow_id.to_owned(),
                state: WorkflowState::Idle,
            });
        }
        // Diffs from here compare against this checkout, not the old one.
        let used = store
            .rounds(workflow_id)?
            .iter()
            .any(|r| r.section_no == wf.section_no && r.is_work());
        let section = if used {
            wf.section_no + 1
        } else {
            wf.section_no
        };
        store.set_section_no(workflow_id, section)?;
        store.delete_snapshots(workflow_id, section, SnapshotKind::Start)?;
        fx.push(Effect::Snapshot {
            workflow_id: workflow_id.to_owned(),
            section_no: section,
            kind: SnapshotKind::Start,
        });
        store.insert_node(
            workflow_id,
            None,
            None,
            NodePayload::Reopened(reopened_in(
                slot,
                moved.then(|| before.checkout_path.clone()).flatten(),
                moved.then(|| before.branch.clone()),
            )),
            None,
        )?;
        self.live.remove(workflow_id);
        fx.push(Effect::FlowChanged {
            workflow_id: workflow_id.to_owned(),
        });
        Ok(fx)
    }

    /// Take a Claude Code session from the repo's history into `slot` (ADR
    /// 0010): a workflow with `origin` that resumes it on its first prompt.
    /// `from` is where it ran, and on which branch.
    pub fn adopt_session(
        &mut self,
        store: &Store,
        slot: &Slot,
        origin: Origin,
        session_id: &str,
        from: (Option<String>, Option<String>),
    ) -> Result<(Workflow, Vec<Effect>)> {
        let wf = Self::create_workflow(store, &slot.id, origin)?;
        store.set_claude_session(&wf.id, session_id)?;
        store.set_workflow_state(&wf.id, WorkflowState::Idle)?;
        let (cwd, branch) = from;
        let elsewhere = cwd.filter(|c| Some(c) != slot.checkout_path.as_ref());
        let branch = branch.filter(|_| elsewhere.is_some());
        store.insert_node(
            &wf.id,
            None,
            None,
            NodePayload::Reopened(reopened_in(slot, elsewhere, branch)),
            None,
        )?;
        let wf = store.workflow(&wf.id)?;
        let fx = vec![
            Effect::Snapshot {
                workflow_id: wf.id.clone(),
                section_no: 1,
                kind: SnapshotKind::Start,
            },
            Effect::FlowChanged {
                workflow_id: wf.id.clone(),
            },
        ];
        Ok((wf, fx))
    }

    /// Rewind the conversation to Prompt `node_id` (ADR 0009): that prompt
    /// and everything after it leave the flow, a Rewind node marks the spot,
    /// and the next session resumes the agent's transcript from just before
    /// it. Files are not rewound. Not while the agent works; stop its sidecar
    /// first.
    pub fn rewind(
        &mut self,
        store: &Store,
        workflow_id: &str,
        node_id: &str,
    ) -> Result<(Rewound, Vec<Effect>)> {
        let wf = store.workflow(workflow_id)?;
        if wf.state.is_busy() {
            return Err(CoreError::InvalidTransition(
                "the agent is working; interrupt it before rewinding".to_owned(),
            ));
        }
        let node = store.node(node_id)?;
        let (NodePayload::Prompt(prompt), Some(round_id)) = (&node.payload, &node.round_id) else {
            return Err(CoreError::InvalidTransition(
                "a conversation rewinds only to one of your messages".to_owned(),
            ));
        };
        if node.workflow_id != workflow_id {
            return Err(CoreError::NotFound {
                kind: "prompt",
                id: node_id.to_owned(),
            });
        }
        let round = store.round(round_id)?;
        let at = store.resume_point(workflow_id, &round.id)?.ok_or_else(|| {
            CoreError::InvalidTransition(
                "the agent's transcript before this message is missing".to_owned(),
            )
        })?;
        // A move into this slot (opened from history) among what goes is kept:
        // the conversation still continues in this checkout.
        let moved = store
            .nodes(workflow_id)?
            .into_iter()
            .rev()
            .find_map(|n| match n.payload {
                NodePayload::Reopened(r) if n.seq > node.seq => Some(r),
                _ => None,
            });
        // Read before the round goes.
        let turn = store.round_turn(&round.id)?;
        let dropped = store.drop_from(workflow_id, node.seq, &round.id, round.section_no)?;
        if let Some(r) = &moved {
            store.insert_node(
                workflow_id,
                None,
                None,
                NodePayload::Reopened(r.clone()),
                None,
            )?;
        }
        store.insert_node(
            workflow_id,
            None,
            None,
            NodePayload::Rewind(Rewind {
                dropped_rounds: dropped.rounds,
                from: dropped.from,
                to: dropped.to,
            }),
            None,
        )?;
        store.set_section_no(workflow_id, round.section_no)?;
        store.set_workflow_error(workflow_id, None)?;
        store.set_workflow_resume_at(workflow_id, Some(&at))?;
        // Dropping exactly that one clean turn (no nudges, which are turns of
        // their own): the next session declares it, so Claude Code refuses a
        // resume that would drop anything else, such as a queued notification.
        let one_turn = (dropped.rounds == 1 && round.nudges == 0)
            .then_some(turn)
            .flatten();
        store.set_workflow_resume_drops(workflow_id, one_turn.as_deref())?;
        let reopened = wf.state == WorkflowState::Closed;
        let mut fx = vec![Effect::Activity {
            workflow_id: workflow_id.to_owned(),
            activity: None,
        }];
        // The section's start snapshot is the old checkout's: diffs from here
        // compare against this one.
        if moved.is_some() {
            store.delete_snapshots(workflow_id, round.section_no, SnapshotKind::Start)?;
            fx.push(Effect::Snapshot {
                workflow_id: workflow_id.to_owned(),
                section_no: round.section_no,
                kind: SnapshotKind::Start,
            });
        }
        if reopened {
            store.reopen_workflow(workflow_id)?;
            fx.push(Effect::StateChanged {
                workflow_id: workflow_id.to_owned(),
                state: WorkflowState::Idle,
            });
        } else {
            self.set_state(store, workflow_id, WorkflowState::Idle, &mut fx)?;
        }
        self.live.remove(workflow_id);
        fx.push(Effect::FlowChanged {
            workflow_id: workflow_id.to_owned(),
        });
        Ok((
            Rewound {
                prompt: prompt.clone(),
                reopened,
            },
            fx,
        ))
    }

    fn live(&mut self, workflow_id: &str) -> &mut Live {
        self.live.entry(workflow_id.to_owned()).or_default()
    }

    /// Record the user's Prompt (with any pending review comments) and open a round.
    /// Whether the workflow takes a prompt now; [`Self::submit_prompt`]'s
    /// check, so the app can ask before it starts an agent for one.
    pub fn check_prompt(store: &Store, workflow_id: &str) -> Result<()> {
        let wf = store.workflow(workflow_id)?;
        if wf.state.accepts_prompt() {
            Ok(())
        } else {
            Err(CoreError::InvalidTransition(format!(
                "workflow {workflow_id} is {}; it does not accept a prompt now",
                wf.state
            )))
        }
    }

    /// The question, if it is still waiting for its answer ([`Self::answer`]'s check).
    pub fn open_question(
        store: &Store,
        workflow_id: &str,
        question_id: &str,
    ) -> Result<(Node, Question)> {
        let node = store
            .question_node(workflow_id, question_id)?
            .ok_or_else(|| CoreError::NotFound {
                kind: "question",
                id: question_id.to_owned(),
            })?;
        let NodePayload::Question(q) = node.payload.clone() else {
            return Err(CoreError::Corrupt {
                what: "question node",
                value: node.id,
            });
        };
        if q.answer.is_some() {
            return Err(CoreError::InvalidTransition(format!(
                "question {question_id} already answered"
            )));
        }
        Ok((node, q))
    }

    /// The review with `choices` applied, if it is still waiting and they are a
    /// decision on every finding ([`Self::answer_review`]'s check). Writes nothing.
    pub fn decided_review(
        store: &Store,
        workflow_id: &str,
        review_id: &str,
        choices: &[(FindingAction, Option<String>)],
    ) -> Result<(Node, Review)> {
        let node =
            store
                .review_node(workflow_id, review_id)?
                .ok_or_else(|| CoreError::NotFound {
                    kind: "review",
                    id: review_id.to_owned(),
                })?;
        let NodePayload::Review(mut r) = node.payload.clone() else {
            return Err(CoreError::Corrupt {
                what: "review node",
                value: node.id,
            });
        };
        if r.answered_at.is_some() {
            return Err(CoreError::InvalidTransition(format!(
                "review {review_id} already answered"
            )));
        }
        if choices.len() != r.findings.len() {
            return Err(CoreError::InvalidTransition(format!(
                "review {review_id} has {} findings, got {} decisions",
                r.findings.len(),
                choices.len()
            )));
        }
        for (i, (f, (choice, note))) in r.findings.iter_mut().zip(choices).enumerate() {
            let note = note.as_deref().map(str::trim).filter(|n| !n.is_empty());
            if *choice == FindingAction::Decide && note.is_none() {
                return Err(CoreError::InvalidTransition(format!(
                    "finding {} is yours to decide: say what to do",
                    i + 1
                )));
            }
            f.choice = Some(*choice);
            f.note = note
                .filter(|_| *choice == FindingAction::Decide)
                .map(str::to_owned);
        }
        Ok((node, r))
    }

    pub fn submit_prompt(
        &mut self,
        store: &Store,
        workflow_id: &str,
        text: &str,
        attachments: Vec<berth_protocol::FileRef>,
        command: bool,
    ) -> Result<Submission> {
        Self::check_prompt(store, workflow_id)?;
        let wf = store.workflow(workflow_id)?;
        // A slash command (the caller knows the session's: see
        // `Commands::is_command`) goes to Claude Code as typed (ADR 0015):
        // review comments wait for the next prompt, and so do the notes below.
        let pending = if command {
            Vec::new()
        } else {
            store.pending_review_comments(workflow_id)?
        };
        let review_comments: Vec<ReviewComment> = pending
            .iter()
            .map(crate::model::ReviewCommentRow::to_protocol)
            .collect();
        let prompt = Prompt {
            text: text.to_owned(),
            review_comments,
            attachments,
        };
        let mut turn_text = serialize_prompt(&prompt);
        // The first prompt after a rewind or a reopen tells the agent where
        // its files stand: every rewind and reopen since the last prompt
        // (one that ran a command doesn't count), in order.
        let nodes = store.nodes(workflow_id)?;
        let commands: std::collections::HashSet<String> = store
            .rounds(workflow_id)?
            .into_iter()
            .filter(|r| !r.is_work())
            .map(|r| r.id)
            .collect();
        let since = nodes
            .iter()
            .rposition(|n| {
                matches!(n.payload, NodePayload::Prompt(_))
                    && !n.round_id.as_ref().is_some_and(|r| commands.contains(r))
            })
            .map_or(0, |i| i + 1);
        let notes: Vec<String> = nodes[since..]
            .iter()
            .filter_map(|n| match &n.payload {
                NodePayload::Rewind(_) => Some(crate::prompt::REWIND_NOTE.to_owned()),
                NodePayload::Reopened(r) => Some(crate::prompt::reopen_note(r)),
                _ => None,
            })
            .collect();
        if !notes.is_empty() && !command {
            turn_text = format!("{}\n\n{turn_text}", notes.join("\n\n"));
        }
        let round = store.insert_round(workflow_id, wf.section_no, now_ms())?;
        let turn_uuid = uuid::Uuid::new_v4().to_string();
        store.set_round_turn(&round.id, &turn_uuid)?;
        let node = store.insert_node(
            workflow_id,
            Some(&round.id),
            None,
            NodePayload::Prompt(prompt),
            None,
        )?;
        let ids: Vec<String> = pending.into_iter().map(|c| c.id).collect();
        store.mark_comments_sent(&ids, &node.id)?;
        store.set_workflow_state(workflow_id, WorkflowState::Running)?;
        store.set_workflow_error(workflow_id, None)?;
        let live = self.live(workflow_id);
        if let Some(latest) = live.latest_raw {
            live.lane_cursor.insert(None, latest);
        }
        Ok(Submission {
            node,
            turn_text,
            turn_uuid,
        })
    }

    /// Open the workflow's first round, driven by its Origin (no Prompt node:
    /// the Origin node already shows what was asked). Returns the turn text.
    pub fn begin_origin_round(&mut self, store: &Store, workflow_id: &str) -> Result<String> {
        let wf = store.workflow(workflow_id)?;
        if wf.state != WorkflowState::Starting {
            return Err(CoreError::InvalidTransition(format!(
                "workflow {workflow_id} already started"
            )));
        }
        store.insert_round(workflow_id, wf.section_no, now_ms())?;
        store.set_workflow_state(workflow_id, WorkflowState::Running)?;
        store.set_workflow_error(workflow_id, None)?;
        Ok(crate::prompt::origin_turn(&wf.origin))
    }

    /// Record the user's answer and decide how to deliver it.
    pub fn answer(
        &mut self,
        store: &Store,
        workflow_id: &str,
        question_id: &str,
        text: &str,
    ) -> Result<AnswerDelivery> {
        let (node, mut q) = Self::open_question(store, workflow_id, question_id)?;
        q.answer = Some(text.to_owned());
        q.answered_at = Some(now_ms());
        store.update_node_payload(&node.id, &NodePayload::Question(q.clone()))?;
        let live = self.live(workflow_id);
        let delivery = if live.blocked.remove(question_id) {
            AnswerDelivery::Answer {
                question_id: question_id.to_owned(),
                text: text.to_owned(),
            }
        } else {
            // The blocked tool call died with its sidecar; the answer starts a new round.
            if store.open_round(workflow_id)?.is_none() {
                let wf = store.workflow(workflow_id)?;
                store.insert_round(workflow_id, wf.section_no, now_ms())?;
            }
            AnswerDelivery::Turn(answer_as_turn(&q.question, text))
        };
        store.set_workflow_state(workflow_id, WorkflowState::Running)?;
        Ok(delivery)
    }

    /// Record the user's decision on each finding of a review (`(choice,
    /// note)`, in the findings' order) and decide how to deliver them. A
    /// `decide` needs the user's words.
    pub fn answer_review(
        &mut self,
        store: &Store,
        workflow_id: &str,
        review_id: &str,
        choices: &[(FindingAction, Option<String>)],
    ) -> Result<AnswerDelivery> {
        let (node, mut r) = Self::decided_review(store, workflow_id, review_id, choices)?;
        r.answered_at = Some(now_ms());
        store.update_node_payload(&node.id, &NodePayload::Review(r.clone()))?;
        let delivery = if self.live(workflow_id).blocked.remove(review_id) {
            AnswerDelivery::Review {
                review_id: review_id.to_owned(),
                text: review_decisions(&r),
            }
        } else {
            if store.open_round(workflow_id)?.is_none() {
                let wf = store.workflow(workflow_id)?;
                store.insert_round(workflow_id, wf.section_no, now_ms())?;
            }
            AnswerDelivery::Turn(review_as_turn(&r))
        };
        store.set_workflow_state(workflow_id, WorkflowState::Running)?;
        Ok(delivery)
    }

    /// Record the result of an [`Effect::FetchArtifact`]: the app-local copy (and
    /// a preview image for non-images), or why the copy failed.
    pub fn artifact_fetched(
        store: &Store,
        node_id: &str,
        result: std::result::Result<(String, Option<String>), String>,
    ) -> Result<()> {
        let node = store.node(node_id)?;
        let NodePayload::Artifact(mut a) = node.payload else {
            return Err(CoreError::Corrupt {
                what: "artifact node",
                value: node.id,
            });
        };
        match result {
            Ok((stored, thumbnail)) => {
                a.file.stored = Some(stored);
                a.file.thumbnail = thumbnail;
                a.error = None;
            }
            Err(e) => a.error = Some(e),
        }
        store.update_node_payload(node_id, &NodePayload::Artifact(a))
    }

    /// Persist a snapshot the app took for an [`Effect::Snapshot`]. A summary
    /// snapshot is also the start snapshot of the next section.
    pub fn record_snapshot(
        store: &Store,
        workflow_id: &str,
        section_no: u32,
        kind: SnapshotKind,
        tree_sha: &str,
    ) -> Result<()> {
        store.insert_snapshot(workflow_id, section_no, kind, tree_sha)?;
        if kind == SnapshotKind::Summary {
            store.insert_snapshot(workflow_id, section_no + 1, SnapshotKind::Start, tree_sha)?;
        }
        Ok(())
    }

    /// Apply one sidecar message.
    pub fn apply(
        &mut self,
        store: &Store,
        workflow_id: &str,
        msg: &SidecarToApp,
    ) -> Result<Vec<Effect>> {
        let wf_id = workflow_id.to_owned();
        let mut fx = Vec::new();
        match msg {
            SidecarToApp::SessionStarted(s) => {
                store.set_claude_session(workflow_id, &s.claude_session_id)?;
            }
            SidecarToApp::RawEvent(e) => {
                let mut round = store.open_round(workflow_id)?;
                if round.is_none() && e.kind == "assistant" && e.branch_id.is_none() {
                    // The CLI started a turn on its own (e.g. a background agent finished).
                    let wf = store.workflow(workflow_id)?;
                    if wf.state.is_open() {
                        round = Some(store.insert_round(workflow_id, wf.section_no, now_ms())?);
                        self.set_state(store, workflow_id, WorkflowState::Running, &mut fx)?;
                    }
                }
                let json = serde_json::to_string(&e.json)?;
                let id = store.insert_raw_event(&NewRawEvent {
                    workflow_id,
                    round_id: round.as_ref().map(|r| r.id.as_str()),
                    branch_id: e.branch_id.as_deref(),
                    seq: i64::try_from(e.seq).unwrap_or(i64::MAX),
                    kind: &e.kind,
                    json: &json,
                })?;
                let live = self.live(workflow_id);
                live.latest_raw = Some(id);
                live.lane_cursor.entry(None).or_insert(id - 1);
                // The agent answered on the rewound branch: later sessions
                // resume its latest message again.
                if live.fresh && e.kind == "assistant" && e.branch_id.is_none() {
                    live.fresh = false;
                    store.set_workflow_resume_at(workflow_id, None)?;
                }
                if let Some(round) = &round {
                    let activity = live
                        .activity
                        .get_or_insert_with(|| RoundActivity::new(round.started_at));
                    if activity.observe(&e.kind, &e.json, e.branch_id.as_deref(), now_ms()) {
                        fx.push(Effect::Activity {
                            workflow_id: wf_id,
                            activity: Some(activity.clone()),
                        });
                    }
                }
            }
            SidecarToApp::NodeDecision(d) => {
                let payload = NodePayload::Decision(d.decision.clone());
                self.insert_lane_node(store, workflow_id, d.branch_id.as_deref(), payload)?;
                fx.push(Effect::FlowChanged { workflow_id: wf_id });
            }
            SidecarToApp::NodeQuestion(q) => {
                let payload = NodePayload::Question(Question {
                    question_id: q.question_id.clone(),
                    question: q.question.clone(),
                    options: q.options.clone(),
                    allow_free_text: q.allow_free_text,
                    answer: None,
                    answered_at: None,
                });
                self.insert_lane_node(store, workflow_id, None, payload)?;
                self.live(workflow_id).blocked.insert(q.question_id.clone());
                self.set_state(store, workflow_id, WorkflowState::AwaitingAnswer, &mut fx)?;
                fx.push(Effect::FlowChanged { workflow_id: wf_id });
            }
            SidecarToApp::NodeReview(r) => {
                let findings = r
                    .findings
                    .iter()
                    .map(|f| Finding {
                        level: f.level,
                        title: f.title.clone(),
                        detail: f.detail.clone(),
                        path: f.path.clone(),
                        line: f.line,
                        suggested: f.suggested,
                        choice: None,
                        note: None,
                    })
                    .collect();
                let payload = NodePayload::Review(Review {
                    review_id: r.review_id.clone(),
                    summary: r.summary.clone(),
                    findings,
                    answered_at: None,
                });
                self.insert_lane_node(store, workflow_id, None, payload)?;
                self.live(workflow_id).blocked.insert(r.review_id.clone());
                self.set_state(store, workflow_id, WorkflowState::AwaitingAnswer, &mut fx)?;
                fx.push(Effect::ShowFindings {
                    workflow_id: wf_id.clone(),
                });
                fx.push(Effect::FlowChanged { workflow_id: wf_id });
            }
            SidecarToApp::NodeSummary(s) => {
                self.insert_lane_node(
                    store,
                    workflow_id,
                    None,
                    NodePayload::Summary(s.summary.clone()),
                )?;
                self.end_section(store, workflow_id, &mut fx)?;
            }
            SidecarToApp::NodeArtifact(a) => {
                let name = a.path.rsplit('/').next().unwrap_or(&a.path).to_owned();
                let payload = NodePayload::Artifact(berth_protocol::Artifact {
                    artifact_id: a.artifact_id.clone(),
                    title: a.title.clone(),
                    description: a.description.clone(),
                    file: berth_protocol::FileRef {
                        id: a.artifact_id.clone(),
                        mime: crate::files::mime_for(&name).to_owned(),
                        name,
                        size_bytes: a.size_bytes,
                        slot_path: Some(a.path.clone()),
                        stored: None,
                        thumbnail: None,
                    },
                    error: None,
                });
                let node = self.insert_lane_node(store, workflow_id, None, payload)?;
                fx.push(Effect::FetchArtifact {
                    workflow_id: wf_id.clone(),
                    node_id: node.id,
                    slot_path: a.path.clone(),
                    size_bytes: a.size_bytes,
                });
                fx.push(Effect::FlowChanged { workflow_id: wf_id });
            }
            SidecarToApp::NodeOutcome(o) => {
                self.insert_lane_node(
                    store,
                    workflow_id,
                    None,
                    NodePayload::Outcome(o.outcome.clone()),
                )?;
                self.cancel_open_branches(store, workflow_id, &mut fx)?;
                let wf = store.workflow(workflow_id)?;
                fx.push(Effect::Snapshot {
                    workflow_id: wf_id.clone(),
                    section_no: wf.section_no,
                    kind: SnapshotKind::Summary,
                });
                self.set_state(store, workflow_id, WorkflowState::Closed, &mut fx)?;
                fx.push(Effect::OpenReview {
                    workflow_id: wf_id.clone(),
                });
                fx.push(Effect::LinkOutcome {
                    workflow_id: wf_id.clone(),
                    outcome: o.outcome.clone(),
                });
                fx.push(Effect::FlowChanged { workflow_id: wf_id });
            }
            SidecarToApp::BranchSpawned(b) => {
                let round = store.open_round(workflow_id)?;
                let now = now_ms();
                store.insert_branch(&Branch {
                    id: b.branch_id.clone(),
                    workflow_id: wf_id.clone(),
                    spawn_round_id: round.as_ref().map(|r| r.id.clone()),
                    collect_round_id: None,
                    parent_branch_id: b.parent_branch_id.clone(),
                    parent_tool_use_id: b.parent_tool_use_id.clone(),
                    agent_name: b.agent_name.clone(),
                    description: b.description.clone(),
                    parallel_group: b.parallel_group.clone(),
                    status: BranchState::Running,
                    spawned_at: now,
                    collected_at: None,
                    cost_usd: None,
                    spawn_tree: None,
                    collect_tree: None,
                    result: None,
                })?;
                let payload = NodePayload::SubagentSpawn(SubagentSpawn {
                    branch_id: b.branch_id.clone(),
                    agent_name: b.agent_name.clone(),
                    description: b.description.clone(),
                    prompt: b.prompt.clone(),
                    parallel_group: b.parallel_group.clone(),
                });
                self.insert_lane_node(store, workflow_id, b.parent_branch_id.as_deref(), payload)?;
                // The branch's own lane starts after the spawn event.
                if let Some(latest) = self.live(workflow_id).latest_raw {
                    self.live(workflow_id)
                        .lane_cursor
                        .insert(Some(b.branch_id.clone()), latest);
                }
                fx.push(Effect::BranchSnapshot {
                    workflow_id: wf_id.clone(),
                    branch_id: b.branch_id.clone(),
                    at_collect: false,
                });
                fx.push(Effect::FlowChanged { workflow_id: wf_id });
            }
            SidecarToApp::BranchCollected(c) => {
                let result = CollectResults {
                    branch_id: c.branch_id.clone(),
                    result_text: c.result_text.clone(),
                    duration_ms: c.duration_ms,
                    cost_usd: c.cost_usd,
                    tool_calls: c.tool_calls,
                    touched_files: c.touched_files.clone(),
                    status: c.status,
                };
                if self.collect(store, workflow_id, result)? {
                    fx.push(Effect::BranchSnapshot {
                        workflow_id: wf_id.clone(),
                        branch_id: c.branch_id.clone(),
                        at_collect: true,
                    });
                    fx.push(Effect::FlowChanged { workflow_id: wf_id });
                }
            }
            SidecarToApp::SlotRequest(r) => {
                fx.push(Effect::ConfirmSlotRequest {
                    workflow_id: wf_id,
                    request: r.clone(),
                });
            }
            SidecarToApp::RoundEnded(r) => {
                self.live(workflow_id).activity = None;
                fx.push(Effect::Activity {
                    workflow_id: wf_id.clone(),
                    activity: None,
                });
                let wf = store.workflow(workflow_id)?;
                let round = match store.open_round(workflow_id)? {
                    Some(round) => round,
                    None => store.insert_round(workflow_id, wf.section_no, now_ms())?,
                };
                // While the round is open, so the node joins it with its span.
                if r.terminal_kind == TerminalKind::Command {
                    self.command_ran(store, workflow_id, &round.id, r.last_text.as_deref())?;
                }
                let duration = i64::try_from(r.duration_ms).unwrap_or(i64::MAX);
                store.end_round(
                    &round.id,
                    r.terminal_kind,
                    r.nudges,
                    r.cost_usd,
                    duration,
                    now_ms(),
                )?;
                if r.terminal_kind == TerminalKind::Unterminated {
                    // Stub Summary so the flow never dangles (§5.3).
                    let stub = Summary {
                        done: r
                            .last_text
                            .iter()
                            .map(|t| t.trim().to_owned())
                            .filter(|t| !t.is_empty())
                            .collect(),
                        remaining: vec![],
                        follow_ups: vec![],
                        notes: Some("The agent ended the round without a summary.".to_owned()),
                    };
                    store.insert_node(
                        workflow_id,
                        Some(&round.id),
                        None,
                        NodePayload::Summary(stub),
                        None,
                    )?;
                    self.end_section(store, workflow_id, &mut fx)?;
                }
                // Questions and reviews still pending when the round ended are moot.
                self.live(workflow_id).blocked.clear();
                let wf = store.workflow(workflow_id)?;
                if wf.state.is_open() {
                    self.set_state(store, workflow_id, WorkflowState::Idle, &mut fx)?;
                }
                fx.push(Effect::FlowChanged { workflow_id: wf_id });
            }
            SidecarToApp::SessionCost(c) => {
                store.set_workflow_cost(workflow_id, c.total_usd)?;
                fx.push(Effect::Cost {
                    workflow_id: wf_id,
                    total_usd: c.total_usd,
                });
            }
            SidecarToApp::SessionError(e) => {
                store.set_workflow_error(workflow_id, Some(&e.message))?;
                if e.fatal {
                    fx.extend(self.sidecar_exited(store, workflow_id, Some(&e.message))?);
                }
                fx.push(Effect::Error {
                    workflow_id: wf_id,
                    message: e.message.clone(),
                    fatal: e.fatal,
                });
            }
            SidecarToApp::SessionContext(c) => {
                let context = crate::ContextUsage {
                    categories: c.categories.clone(),
                    at: now_ms(),
                };
                store.set_workflow_context(workflow_id, &context)?;
                fx.push(Effect::FlowChanged { workflow_id: wf_id });
            }
            // The models on offer and the plan's limits are the app's, not the
            // workflow's: the backend keeps them. The history comes from a
            // listing sidecar.
            SidecarToApp::NodeCompacted(c) => {
                // Compaction writes to the transcript: after a rewind, later
                // sessions resume from it, not from the rewind point (ADR 0015).
                let live = self.live(workflow_id);
                if live.fresh {
                    live.fresh = false;
                    store.set_workflow_resume_at(workflow_id, None)?;
                }
                let payload = NodePayload::Compacted(c.compacted.clone());
                self.insert_lane_node(store, workflow_id, None, payload)?;
                fx.push(Effect::FlowChanged { workflow_id: wf_id });
            }
            SidecarToApp::BranchProgress(p) => {
                let activity = self.live(workflow_id).activity.as_mut();
                if let Some(activity) = activity {
                    activity
                        .branch_summaries
                        .insert(p.branch_id.clone(), p.summary.clone());
                    fx.push(Effect::Activity {
                        workflow_id: wf_id,
                        activity: Some(activity.clone()),
                    });
                }
            }
            SidecarToApp::NodeRefusal(r) => {
                let payload = NodePayload::Refusal(r.refusal.clone());
                self.insert_lane_node(store, workflow_id, None, payload)?;
                fx.push(Effect::FlowChanged { workflow_id: wf_id });
            }
            // The backend keeps what is the app's or the moment's: models,
            // plan limits, commands, the account, a suggestion, background
            // tasks; renames and the history answer short-lived sidecars.
            SidecarToApp::SessionModels(_)
            | SidecarToApp::SessionQuota(_)
            | SidecarToApp::SessionCommands(_)
            | SidecarToApp::SessionSuggestion(_)
            | SidecarToApp::SessionTasks(_)
            | SidecarToApp::SessionAccount(_)
            | SidecarToApp::SessionsRenamed(_)
            | SidecarToApp::SessionsListed(_) => {}
        }
        Ok(fx)
    }

    /// The sidecar process for `workflow_id` is gone. Ends the in-flight round
    /// and returns the workflow to `Idle` (an unanswered question stays visible
    /// and is delivered as a turn once a new session starts).
    pub fn sidecar_exited(
        &mut self,
        store: &Store,
        workflow_id: &str,
        error: Option<&str>,
    ) -> Result<Vec<Effect>> {
        let mut fx = vec![Effect::Activity {
            workflow_id: workflow_id.to_owned(),
            activity: None,
        }];
        self.live.remove(workflow_id);
        if let Some(round) = store.open_round(workflow_id)? {
            let now = now_ms();
            store.end_round(
                &round.id,
                TerminalKind::Error,
                round.nudges,
                0.0,
                now - round.started_at,
                now,
            )?;
            fx.push(Effect::FlowChanged {
                workflow_id: workflow_id.to_owned(),
            });
        }
        if let Some(e) = error {
            store.set_workflow_error(workflow_id, Some(e))?;
        }
        let wf = store.workflow(workflow_id)?;
        if matches!(wf.state, WorkflowState::Running | WorkflowState::Starting) {
            self.set_state(store, workflow_id, WorkflowState::Idle, &mut fx)?;
        }
        Ok(fx)
    }

    /// After an app restart no sidecar is alive: settle every open workflow.
    pub fn recover(&mut self, store: &Store) -> Result<()> {
        for wf in store.open_workflows()? {
            self.sidecar_exited(store, &wf.id, None)?;
        }
        Ok(())
    }

    fn set_state(
        &self,
        store: &Store,
        workflow_id: &str,
        state: WorkflowState,
        fx: &mut Vec<Effect>,
    ) -> Result<()> {
        if store.workflow(workflow_id)?.state != state {
            store.set_workflow_state(workflow_id, state)?;
            fx.push(Effect::StateChanged {
                workflow_id: workflow_id.to_owned(),
                state,
            });
        }
        Ok(())
    }

    /// A round Claude Code spent on a slash command without the model: show
    /// what the command printed. A `/compact` says it with its Compacted node.
    fn command_ran(
        &mut self,
        store: &Store,
        workflow_id: &str,
        round_id: &str,
        output: Option<&str>,
    ) -> Result<()> {
        let nodes = store.nodes(workflow_id)?;
        let in_round = || {
            nodes
                .iter()
                .filter(|n| n.round_id.as_deref() == Some(round_id))
        };
        if in_round().any(|n| matches!(n.payload, NodePayload::Compacted(_))) {
            return Ok(());
        }
        let command = in_round()
            .find_map(|n| match &n.payload {
                NodePayload::Prompt(p) => Some(p.text.trim().to_owned()),
                _ => None,
            })
            .unwrap_or_default();
        let payload = NodePayload::Command(berth_protocol::Command {
            command,
            output: output.map(str::trim).unwrap_or_default().to_owned(),
        });
        // Its raw span is what Claude Code streamed for it ("show details").
        self.insert_lane_node(store, workflow_id, None, payload)?;
        Ok(())
    }

    /// Insert a node in `lane`, giving it the raw span since the lane's previous node.
    fn insert_lane_node(
        &mut self,
        store: &Store,
        workflow_id: &str,
        lane: Option<&str>,
        payload: NodePayload,
    ) -> Result<Node> {
        let round = store.open_round(workflow_id)?;
        let live = self.live(workflow_id);
        let key = lane.map(str::to_owned);
        let span = live.latest_raw.map(|latest| {
            let first = live
                .lane_cursor
                .get(&key)
                .map_or(latest, |c| (c + 1).min(latest));
            (first, latest)
        });
        if let Some((_, last)) = span {
            live.lane_cursor.insert(key, last);
        }
        store.insert_node(
            workflow_id,
            round.as_ref().map(|r| r.id.as_str()),
            lane,
            payload,
            span,
        )
    }

    /// Close a section: snapshot, bump the section number, open the review pane.
    fn end_section(&self, store: &Store, workflow_id: &str, fx: &mut Vec<Effect>) -> Result<()> {
        let wf = store.workflow(workflow_id)?;
        fx.push(Effect::Snapshot {
            workflow_id: workflow_id.to_owned(),
            section_no: wf.section_no,
            kind: SnapshotKind::Summary,
        });
        store.set_section_no(workflow_id, wf.section_no + 1)?;
        fx.push(Effect::OpenReview {
            workflow_id: workflow_id.to_owned(),
        });
        fx.push(Effect::FlowChanged {
            workflow_id: workflow_id.to_owned(),
        });
        Ok(())
    }

    /// Close a branch with a CollectResults node. Returns false if already closed.
    fn collect(
        &mut self,
        store: &Store,
        workflow_id: &str,
        result: CollectResults,
    ) -> Result<bool> {
        let Some(branch) = store.branch(&result.branch_id)? else {
            return Ok(false);
        };
        if branch.status != BranchState::Running {
            return Ok(false);
        }
        let round = store.open_round(workflow_id)?;
        store.collect_branch(
            &branch.id,
            &result,
            round.as_ref().map(|r| r.id.as_str()),
            now_ms(),
        )?;
        // The CollectResults span covers the whole branch transcript.
        let live = self.live(workflow_id);
        let span = live.latest_raw.map(|latest| {
            let first = live
                .lane_cursor
                .remove(&Some(branch.id.clone()))
                .map_or(latest, |c| c.min(latest));
            (first, latest)
        });
        store.insert_node(
            workflow_id,
            round.as_ref().map(|r| r.id.as_str()),
            branch.parent_branch_id.as_deref(),
            NodePayload::CollectResults(result),
            span,
        )?;
        Ok(true)
    }

    /// An unresolved branch at `record_outcome` is closed as cancelled (§5.4).
    fn cancel_open_branches(
        &mut self,
        store: &Store,
        workflow_id: &str,
        fx: &mut Vec<Effect>,
    ) -> Result<()> {
        let now = now_ms();
        for b in store.branches(workflow_id)? {
            if b.status != BranchState::Running {
                continue;
            }
            let result = CollectResults {
                branch_id: b.id.clone(),
                result_text: String::new(),
                duration_ms: u64::try_from(now - b.spawned_at).unwrap_or(0),
                cost_usd: None,
                tool_calls: 0,
                touched_files: vec![],
                status: BranchStatus::Cancelled,
            };
            if self.collect(store, workflow_id, result)? {
                fx.push(Effect::FlowChanged {
                    workflow_id: workflow_id.to_owned(),
                });
            }
        }
        Ok(())
    }
}

fn reopened_in(slot: &Slot, from: Option<String>, from_branch: Option<String>) -> Reopened {
    Reopened {
        slot: slot
            .custom_name
            .clone()
            .unwrap_or_else(|| slot.name.clone()),
        checkout: slot.checkout_path.clone().unwrap_or_default(),
        branch: slot.branch.clone(),
        from_checkout: from,
        from_branch,
    }
}

/// Convenience for views: the Decision nodes in a branch's lane.
#[must_use]
pub fn branch_decisions<'a>(nodes: &'a [Node], branch_id: &str) -> Vec<&'a Decision> {
    nodes
        .iter()
        .filter(|n| n.branch_id.as_deref() == Some(branch_id))
        .filter_map(|n| match &n.payload {
            NodePayload::Decision(d) => Some(d),
            _ => None,
        })
        .collect()
}
