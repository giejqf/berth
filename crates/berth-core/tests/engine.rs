#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use berth_core::{
    AnswerDelivery, Effect, Engine, NewReviewComment, Slot, SlotState, SnapshotKind, Store,
    WorkflowState,
    ids::{new_id, now_ms},
};
use berth_protocol::{
    BranchCollected, BranchSpawned, BranchStatus, Decision, DiffPreset, DiffSide, FindingAction,
    FindingInput, FindingLevel, NodeArtifact, NodeDecision, NodeOutcome, NodePayload, NodeQuestion,
    NodeReview, NodeSummary, Origin, OriginKind, Outcome, OutcomeKind, RawEvent, RoundEnded,
    SessionCost, SessionError, SessionStarted, SidecarToApp, Summary, TerminalKind,
};

fn setup() -> (Store, Engine, String) {
    let store = Store::open_in_memory().unwrap();
    let repo = store.upsert_repo(Path::new("/tmp/repo")).unwrap();
    let mut slot = Slot {
        id: new_id(),
        repo_id: repo.id,
        slot_type: "worktree".into(),
        name: "fix".into(),
        custom_name: None,
        model: None,
        effort: None,
        state: SlotState::Ready,
        state_detail: None,
        checkout_path: Some("/tmp/co".into()),
        placement: "host".into(),
        branch: "berth/fix".into(),
        base_ref: "main".into(),
        created_at: now_ms(),
        number: 0,
    };
    store.insert_slot(&mut slot).unwrap();
    let origin = Origin {
        kind: OriginKind::Thought,
        r#ref: String::new(),
        title: "Fix it".into(),
        body: String::new(),
        custom_prompt: None,
        url: None,
    };
    let wf = Engine::create_workflow(&store, &slot.id, origin).unwrap();
    (store, Engine::new(), wf.id)
}

fn raw(seq: u64, kind: &str, branch: Option<&str>) -> SidecarToApp {
    SidecarToApp::RawEvent(RawEvent {
        seq,
        kind: kind.into(),
        json: serde_json::json!({"type": kind, "message": {"content": [{"type": "tool_use", "name": "Bash", "input": {"command": "cargo test"}}]}}),
        branch_id: branch.map(str::to_owned),
    })
}

fn decision(title: &str, branch: Option<&str>) -> SidecarToApp {
    SidecarToApp::NodeDecision(NodeDecision {
        decision: Decision {
            title: title.into(),
            rationale: "r".into(),
            affects: vec![],
            reversible: true,
        },
        branch_id: branch.map(str::to_owned),
    })
}

fn ended(kind: TerminalKind, last_text: Option<&str>) -> SidecarToApp {
    SidecarToApp::RoundEnded(RoundEnded {
        terminal_kind: kind,
        cost_usd: 0.1,
        duration_ms: 1000,
        nudges: 0,
        last_text: last_text.map(str::to_owned),
    })
}

#[test]
fn second_workflow_in_a_slot_is_rejected_while_one_is_open() {
    let (store, _, wf) = setup();
    let slot = store.workflow(&wf).unwrap().slot_id;
    let origin = store.workflow(&wf).unwrap().origin;
    assert!(Engine::create_workflow(&store, &slot, origin).is_err());
}

#[test]
fn prompt_decision_summary_round() {
    let (store, mut engine, wf) = setup();
    engine.session_started(&wf);
    engine
        .apply(
            &store,
            &wf,
            &SidecarToApp::SessionStarted(SessionStarted {
                claude_session_id: "s1".into(),
            }),
        )
        .unwrap();
    assert_eq!(
        store.workflow(&wf).unwrap().claude_session_id.as_deref(),
        Some("s1")
    );

    let sub = engine
        .submit_prompt(&store, &wf, "Do the thing", vec![], false)
        .unwrap();
    assert_eq!(sub.turn_text, "Do the thing");
    assert_eq!(store.workflow(&wf).unwrap().state, WorkflowState::Running);
    assert!(
        engine
            .submit_prompt(&store, &wf, "/context", vec![], true)
            .is_err(),
        "a running workflow takes no command (a message joins its round: ADR 0018)"
    );

    let fx = engine
        .apply(&store, &wf, &raw(1, "assistant", None))
        .unwrap();
    let activity = fx.iter().find_map(|e| match e {
        Effect::Activity {
            activity: Some(a), ..
        } => Some(a.clone()),
        _ => None,
    });
    let activity = activity.expect("an activity update");
    assert_eq!(activity.tool_calls, 1);
    assert_eq!(activity.headline(), "Running cargo test");
    engine.apply(&store, &wf, &raw(2, "user", None)).unwrap();
    engine
        .apply(&store, &wf, &decision("Use sqlite", None))
        .unwrap();

    let fx = engine
        .apply(
            &store,
            &wf,
            &SidecarToApp::NodeSummary(NodeSummary {
                summary: Summary {
                    done: vec!["x".into()],
                    remaining: vec![],
                    follow_ups: vec![],
                    notes: None,
                },
            }),
        )
        .unwrap();
    assert!(fx.contains(&Effect::Snapshot {
        workflow_id: wf.clone(),
        section_no: 1,
        kind: SnapshotKind::Summary
    }));
    assert!(fx.contains(&Effect::OpenReview {
        workflow_id: wf.clone()
    }));
    assert_eq!(store.workflow(&wf).unwrap().section_no, 2);

    Engine::record_snapshot(&store, &wf, 1, SnapshotKind::Summary, "abc").unwrap();
    let view = store.workflow_view(&wf).unwrap();
    assert_eq!(
        view.snapshot(2, SnapshotKind::Start).unwrap().tree_sha,
        "abc"
    );

    let fx = engine
        .apply(&store, &wf, &ended(TerminalKind::WriteSummary, None))
        .unwrap();
    assert!(fx.contains(&Effect::StateChanged {
        workflow_id: wf.clone(),
        state: WorkflowState::Idle
    }));
    let view = store.workflow_view(&wf).unwrap();
    assert_eq!(view.rounds.len(), 1);
    assert_eq!(
        view.rounds[0].terminal_kind,
        Some(TerminalKind::WriteSummary)
    );
    assert!(view.rounds[0].ended_at.is_some());
    // Origin, Prompt, Decision, Summary
    let kinds: Vec<&str> = view
        .nodes
        .iter()
        .map(|n| n.payload.kind().as_str())
        .collect();
    assert_eq!(kinds, ["origin", "prompt", "decision", "summary"]);
    // The decision's raw span covers the two raw events before it.
    let d = &view.nodes[2];
    let span = store
        .raw_events(&wf, d.raw_first.unwrap(), d.raw_last.unwrap(), Some(None))
        .unwrap();
    assert_eq!(span.iter().map(|r| r.seq).collect::<Vec<_>>(), [1, 2]);
    assert_eq!(
        span[0].round_id.as_deref(),
        Some(view.rounds[0].id.as_str())
    );

    engine
        .apply(
            &store,
            &wf,
            &SidecarToApp::SessionCost(SessionCost { total_usd: 0.42 }),
        )
        .unwrap();
    assert!((store.workflow(&wf).unwrap().total_cost_usd - 0.42).abs() < f64::EPSILON);
}

#[test]
fn questions_answer_live_or_as_turn_after_restart() {
    let (store, mut engine, wf) = setup();
    engine.session_started(&wf);
    engine
        .submit_prompt(&store, &wf, "go", vec![], false)
        .unwrap();
    let q = |id: &str| {
        SidecarToApp::NodeQuestion(NodeQuestion {
            question_id: id.into(),
            question: "Which DB?".into(),
            options: vec!["sqlite".into()],
            allow_free_text: true,
        })
    };
    engine.apply(&store, &wf, &q("q1")).unwrap();
    assert_eq!(
        store.workflow(&wf).unwrap().state,
        WorkflowState::AwaitingAnswer
    );
    assert_eq!(
        store
            .workflow_view(&wf)
            .unwrap()
            .pending_question()
            .unwrap()
            .1
            .question_id,
        "q1"
    );

    let d = engine.answer(&store, &wf, "q1", "sqlite").unwrap();
    assert_eq!(
        d,
        AnswerDelivery::Answer {
            question_id: "q1".into(),
            text: "sqlite".into()
        }
    );
    assert_eq!(store.workflow(&wf).unwrap().state, WorkflowState::Running);
    assert!(engine.answer(&store, &wf, "q1", "again").is_err());
    let view = store.workflow_view(&wf).unwrap();
    let NodePayload::Question(answered) = &view.nodes.last().unwrap().payload else {
        panic!()
    };
    assert_eq!(answered.answer.as_deref(), Some("sqlite"));
    assert!(view.pending_question().is_none());

    // A second question, then the sidecar dies: the answer becomes a turn.
    engine.apply(&store, &wf, &q("q2")).unwrap();
    engine.sidecar_exited(&store, &wf, Some("crashed")).unwrap();
    assert_eq!(
        store.workflow(&wf).unwrap().state,
        WorkflowState::AwaitingAnswer
    );
    assert_eq!(
        store.workflow(&wf).unwrap().last_error.as_deref(),
        Some("crashed")
    );
    engine.session_started(&wf);
    let d = engine.answer(&store, &wf, "q2", "postgres").unwrap();
    let AnswerDelivery::Turn(text) = d else {
        panic!("{d:?}")
    };
    assert!(text.contains("Which DB?") && text.contains("postgres"));
    assert!(store.open_round(&wf).unwrap().is_some());
}

#[test]
fn reviews_wait_for_a_decision_on_every_finding() {
    let (store, mut engine, wf) = setup();
    engine.session_started(&wf);
    engine
        .submit_prompt(&store, &wf, "review it", vec![], false)
        .unwrap();
    let finding = |title: &str, suggested| FindingInput {
        level: FindingLevel::High,
        title: title.into(),
        detail: "why".into(),
        path: Some("src/a.rs".into()),
        line: Some(3),
        suggested,
    };
    let review = |id: &str| {
        SidecarToApp::NodeReview(NodeReview {
            review_id: id.into(),
            summary: Some("Two findings.".into()),
            findings: vec![
                finding("Unchecked unwrap", FindingAction::Fix),
                finding("Naming", FindingAction::Decide),
            ],
        })
    };
    let fx = engine.apply(&store, &wf, &review("r1")).unwrap();
    assert!(fx.iter().any(|e| matches!(e, Effect::ShowFindings { .. })));
    assert_eq!(
        store.workflow(&wf).unwrap().state,
        WorkflowState::AwaitingAnswer
    );
    // The app checks before it starts an agent: no prompt while it waits, and
    // checking the decisions records nothing.
    assert!(Engine::check_prompt(&store, &wf).is_err());
    let both = [
        (FindingAction::Fix, None),
        (FindingAction::Decide, Some("keep it".to_owned())),
    ];
    let (_, decided) = Engine::decided_review(&store, &wf, "r1", &both).unwrap();
    assert_eq!(decided.findings[1].note.as_deref(), Some("keep it"));
    assert!(Engine::decided_review(&store, &wf, "r1", &both[..1]).is_err());
    let NodePayload::Review(stored) = store.review_node(&wf, "r1").unwrap().unwrap().payload else {
        panic!("not a review")
    };
    assert!(stored.answered_at.is_none() && stored.findings[0].choice.is_none());
    let facts = store.workflow_facts(&wf).unwrap();
    assert_eq!(facts.review.map(|(_, n)| n), Some(2));
    assert!(facts.question_at.is_none());

    // Every finding needs a decision, and a decide needs words.
    let fix = (FindingAction::Fix, None);
    assert!(
        engine
            .answer_review(&store, &wf, "r1", std::slice::from_ref(&fix))
            .is_err()
    );
    assert!(
        engine
            .answer_review(
                &store,
                &wf,
                "r1",
                &[fix.clone(), (FindingAction::Decide, Some("  ".into()))]
            )
            .is_err()
    );
    let d = engine
        .answer_review(
            &store,
            &wf,
            "r1",
            &[
                (FindingAction::Dismiss, Some("ignored".into())),
                (FindingAction::Decide, Some("rename it later".into())),
            ],
        )
        .unwrap();
    let AnswerDelivery::Review { review_id, text } = d else {
        panic!("{d:?}")
    };
    assert_eq!(review_id, "r1");
    assert_eq!(
        text,
        "1. [high] Unchecked unwrap (src/a.rs:3): dismissed, leave it as it is\n\
         2. [high] Naming (src/a.rs:3): my call: rename it later"
    );
    assert_eq!(store.workflow(&wf).unwrap().state, WorkflowState::Running);
    let view = store.workflow_view(&wf).unwrap();
    assert!(view.pending_review().is_none());
    let NodePayload::Review(r) = &view.nodes.last().unwrap().payload else {
        panic!()
    };
    assert_eq!(r.findings[0].choice, Some(FindingAction::Dismiss));
    // Only a decide keeps its note.
    assert_eq!(r.findings[0].note, None);
    assert_eq!(r.findings[1].note.as_deref(), Some("rename it later"));
    assert!(
        engine
            .answer_review(&store, &wf, "r1", &[fix.clone(), fix.clone()])
            .is_err()
    );
    assert!(store.workflow_facts(&wf).unwrap().review.is_none());

    // The sidecar dies while a review waits: the decisions go as a new turn.
    engine.apply(&store, &wf, &review("r2")).unwrap();
    engine.sidecar_exited(&store, &wf, None).unwrap();
    engine.session_started(&wf);
    let d = engine
        .answer_review(&store, &wf, "r2", &[fix.clone(), fix])
        .unwrap();
    let AnswerDelivery::Turn(text) = d else {
        panic!("{d:?}")
    };
    assert!(text.starts_with("Earlier you presented code review findings."));
    assert!(store.open_round(&wf).unwrap().is_some());

    // A question the round ended on stays unanswered, but the workflow waits
    // on the review that came after it, not on that question.
    engine
        .apply(
            &store,
            &wf,
            &SidecarToApp::NodeQuestion(NodeQuestion {
                question_id: "q-old".into(),
                question: "?".into(),
                options: vec![],
                allow_free_text: true,
            }),
        )
        .unwrap();
    engine
        .apply(&store, &wf, &ended(TerminalKind::Interrupted, None))
        .unwrap();
    engine
        .submit_prompt(&store, &wf, "review again", vec![], false)
        .unwrap();
    engine.apply(&store, &wf, &review("r3")).unwrap();
    let view = store.workflow_view(&wf).unwrap();
    assert!(view.pending_question().is_none());
    assert_eq!(view.pending_review().unwrap().1.review_id, "r3");
}

#[test]
fn unterminated_round_gets_a_stub_summary() {
    let (store, mut engine, wf) = setup();
    engine.session_started(&wf);
    engine
        .submit_prompt(&store, &wf, "go", vec![], false)
        .unwrap();
    let fx = engine
        .apply(
            &store,
            &wf,
            &ended(TerminalKind::Unterminated, Some("I looked around.")),
        )
        .unwrap();
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::Snapshot { section_no: 1, .. }))
    );
    let view = store.workflow_view(&wf).unwrap();
    let NodePayload::Summary(s) = &view.nodes.last().unwrap().payload else {
        panic!()
    };
    assert_eq!(s.done, ["I looked around."]);
    assert!(s.notes.is_some());
    assert_eq!(view.workflow.state, WorkflowState::Idle);
    assert_eq!(view.workflow.section_no, 2);
}

#[test]
fn branches_spawn_collect_and_cancel_at_outcome() {
    let (store, mut engine, wf) = setup();
    engine.session_started(&wf);
    engine
        .submit_prompt(&store, &wf, "go", vec![], false)
        .unwrap();
    let spawned = |id: &str| {
        SidecarToApp::BranchSpawned(BranchSpawned {
            branch_id: id.into(),
            parent_tool_use_id: format!("tu-{id}"),
            parent_branch_id: None,
            agent_name: "explore".into(),
            description: "Explore".into(),
            prompt: "look".into(),
            parallel_group: Some("m1".into()),
        })
    };
    engine
        .apply(&store, &wf, &raw(1, "assistant", None))
        .unwrap();
    let fx = engine.apply(&store, &wf, &spawned("a")).unwrap();
    assert!(fx.contains(&Effect::BranchSnapshot {
        workflow_id: wf.clone(),
        branch_id: "a".into(),
        at_collect: false
    }));
    engine.apply(&store, &wf, &spawned("b")).unwrap();
    engine
        .apply(&store, &wf, &raw(2, "assistant", Some("a")))
        .unwrap();
    engine
        .apply(&store, &wf, &decision("in branch", Some("a")))
        .unwrap();
    engine
        .apply(&store, &wf, &raw(3, "user", Some("a")))
        .unwrap();
    engine
        .apply(
            &store,
            &wf,
            &SidecarToApp::BranchCollected(BranchCollected {
                branch_id: "a".into(),
                result_text: "found".into(),
                status: BranchStatus::Ok,
                duration_ms: 41_000,
                cost_usd: None,
                tool_calls: 4,
                touched_files: vec!["src/a.rs".into()],
            }),
        )
        .unwrap();

    let view = store.workflow_view(&wf).unwrap();
    let a = view.branch("a").unwrap();
    assert_eq!(a.status, berth_core::BranchState::Ok);
    assert_eq!(a.result.as_ref().unwrap().touched_files, ["src/a.rs"]);
    let collect = view
        .nodes
        .iter()
        .find(|n| matches!(n.payload, NodePayload::CollectResults(_)))
        .unwrap();
    assert!(
        collect.branch_id.is_none(),
        "collect sits in the parent lane"
    );
    let branch_events = store
        .raw_events(
            &wf,
            collect.raw_first.unwrap(),
            collect.raw_last.unwrap(),
            Some(Some("a")),
        )
        .unwrap();
    assert_eq!(
        branch_events.iter().map(|r| r.seq).collect::<Vec<_>>(),
        [2, 3]
    );
    let in_branch = view
        .nodes
        .iter()
        .find(|n| n.branch_id.as_deref() == Some("a"))
        .unwrap();
    assert!(matches!(in_branch.payload, NodePayload::Decision(_)));

    // Outcome closes the still-running branch b as cancelled and the workflow.
    let fx = engine
        .apply(
            &store,
            &wf,
            &SidecarToApp::NodeOutcome(NodeOutcome {
                outcome: Outcome {
                    kind: OutcomeKind::Pr,
                    r#ref: "https://github.com/o/r/pull/7".into(),
                    summary: "done".into(),
                    follow_ups: vec![],
                    title: None,
                },
            }),
        )
        .unwrap();
    assert!(fx.iter().any(|e| matches!(e, Effect::LinkOutcome { .. })));
    let view = store.workflow_view(&wf).unwrap();
    assert_eq!(
        view.branch("b").unwrap().status,
        berth_core::BranchState::Cancelled
    );
    assert_eq!(view.workflow.state, WorkflowState::Closed);
    assert!(view.workflow.closed_at.is_some());
    engine
        .apply(&store, &wf, &ended(TerminalKind::RecordOutcome, None))
        .unwrap();
    assert_eq!(store.workflow(&wf).unwrap().state, WorkflowState::Closed);
}

#[test]
fn self_started_turn_opens_an_implicit_round() {
    let (store, mut engine, wf) = setup();
    engine.session_started(&wf);
    engine
        .submit_prompt(&store, &wf, "go", vec![], false)
        .unwrap();
    engine
        .apply(&store, &wf, &ended(TerminalKind::WriteSummary, None))
        .unwrap();
    assert!(store.open_round(&wf).unwrap().is_none());
    engine
        .apply(&store, &wf, &raw(9, "assistant", None))
        .unwrap();
    assert!(store.open_round(&wf).unwrap().is_some());
    assert_eq!(store.workflow(&wf).unwrap().state, WorkflowState::Running);
}

#[test]
fn fatal_errors_and_recovery_settle_workflows() {
    let (store, mut engine, wf) = setup();
    engine.session_started(&wf);
    engine
        .submit_prompt(&store, &wf, "go", vec![], false)
        .unwrap();
    let fx = engine
        .apply(
            &store,
            &wf,
            &SidecarToApp::SessionError(SessionError {
                message: "boom".into(),
                fatal: true,
            }),
        )
        .unwrap();
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::Error { fatal: true, .. }))
    );
    assert_eq!(store.workflow(&wf).unwrap().state, WorkflowState::Idle);
    assert_eq!(
        store.rounds(&wf).unwrap()[0].terminal_kind,
        Some(TerminalKind::Error)
    );

    engine
        .submit_prompt(&store, &wf, "again", vec![], false)
        .unwrap();
    let mut fresh = Engine::new();
    fresh.recover(&store).unwrap();
    assert_eq!(store.workflow(&wf).unwrap().state, WorkflowState::Idle);
    assert!(store.open_round(&wf).unwrap().is_none());
}

#[test]
fn review_comments_ride_along_with_the_next_prompt() {
    let (store, mut engine, wf) = setup();
    engine.session_started(&wf);
    let c = store
        .insert_review_comment(&NewReviewComment {
            workflow_id: wf.clone(),
            from_snapshot: "a".into(),
            to_snapshot: "b".into(),
            preset: DiffPreset::Section,
            path: "src/x.rs".into(),
            side: DiffSide::New,
            line: 3,
            body: "rename".into(),
        })
        .unwrap();
    let sub = engine
        .submit_prompt(&store, &wf, "Address these", vec![], false)
        .unwrap();
    assert_eq!(
        sub.turn_text,
        "Address these\n\nReview comments on section diff:\n- src/x.rs:3 (new): rename"
    );
    let NodePayload::Prompt(p) = &sub.node.payload else {
        panic!()
    };
    assert_eq!(p.review_comments.len(), 1);
    assert!(store.pending_review_comments(&wf).unwrap().is_empty());
    let all = store.review_comments(&wf).unwrap();
    assert_eq!(all[0].id, c.id);
    assert_eq!(all[0].sent_in_node.as_deref(), Some(sub.node.id.as_str()));
}

#[test]
fn slot_lifecycle_rejects_invalid_edges() {
    let (store, _, wf) = setup();
    let slot = store.workflow(&wf).unwrap().slot_id;
    store
        .transition_slot(&slot, SlotState::Running, None)
        .unwrap();
    assert!(
        store
            .transition_slot(&slot, SlotState::Creating, None)
            .is_err()
    );
    store
        .transition_slot(&slot, SlotState::Ready, None)
        .unwrap();
    store
        .transition_slot(&slot, SlotState::Destroying, None)
        .unwrap();
    store
        .transition_slot(&slot, SlotState::Destroyed, None)
        .unwrap();
    let repo = store.slot(&slot).unwrap().repo_id;
    assert!(store.live_slots(&repo).unwrap().is_empty());
}

#[test]
fn gh_entities_upsert_and_link() {
    let (store, _, wf) = setup();
    let repo = store.repo_by_path(Path::new("/tmp/repo")).unwrap().unwrap();
    let e1 = store
        .upsert_gh_entity(&repo.id, "pr", "7", r#"{"v":1}"#)
        .unwrap();
    let e2 = store
        .upsert_gh_entity(&repo.id, "pr", "7", r#"{"v":2}"#)
        .unwrap();
    assert_eq!(e1.id, e2.id);
    assert_eq!(e2.json, r#"{"v":2}"#);
    store.link_workflow(&wf, &e1.id, "outcome").unwrap();
    store.link_workflow(&wf, &e1.id, "outcome").unwrap();
    assert_eq!(store.workflow_links(&wf).unwrap().len(), 1);
    store.set_setting("k", "v").unwrap();
    store.set_setting("k", "w").unwrap();
    assert_eq!(store.setting("k").unwrap().as_deref(), Some("w"));
}

#[test]
fn reopening_a_file_database_keeps_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/berth.db");
    {
        let store = Store::open(&path).unwrap();
        store.upsert_repo(Path::new("/r")).unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert!(store.repo_by_path(Path::new("/r")).unwrap().is_some());
}

#[test]
fn artifacts_are_fetched_then_shown() {
    let (store, mut engine, wf) = setup();
    engine.session_started(&wf);
    engine
        .submit_prompt(&store, &wf, "Take a screenshot", vec![], false)
        .unwrap();
    engine
        .apply(&store, &wf, &raw(1, "assistant", None))
        .unwrap();
    let fx = engine
        .apply(
            &store,
            &wf,
            &SidecarToApp::NodeArtifact(NodeArtifact {
                artifact_id: "a1".into(),
                path: "/co/out/Login Page.png".into(),
                title: "Login page".into(),
                description: Some("after the fix".into()),
                size_bytes: 4096,
            }),
        )
        .unwrap();
    let Some(Effect::FetchArtifact {
        node_id,
        slot_path,
        size_bytes,
        ..
    }) = fx
        .iter()
        .find(|e| matches!(e, Effect::FetchArtifact { .. }))
        .cloned()
    else {
        panic!("{fx:?}")
    };
    assert_eq!(
        (slot_path.as_str(), size_bytes),
        ("/co/out/Login Page.png", 4096)
    );
    let NodePayload::Artifact(a) = store.node(&node_id).unwrap().payload else {
        panic!()
    };
    assert_eq!(a.file.name, "Login Page.png");
    assert_eq!(a.file.mime, "image/png");
    assert!(a.file.stored.is_none());

    Engine::artifact_fetched(
        &store,
        &node_id,
        Ok(("/app/files/a1-Login_Page.png".into(), None)),
    )
    .unwrap();
    let NodePayload::Artifact(a) = store.node(&node_id).unwrap().payload else {
        panic!()
    };
    assert_eq!(
        a.file.stored.as_deref(),
        Some("/app/files/a1-Login_Page.png")
    );
    Engine::artifact_fetched(&store, &node_id, Err("too large".into())).unwrap();
    let NodePayload::Artifact(a) = store.node(&node_id).unwrap().payload else {
        panic!()
    };
    assert_eq!(a.error.as_deref(), Some("too large"));
}

#[test]
fn prompt_attachments_are_listed_in_the_turn() {
    let (store, mut engine, wf) = setup();
    let file = berth_protocol::FileRef {
        id: "f1".into(),
        name: "spec.pdf".into(),
        mime: "application/pdf".into(),
        size_bytes: 10_240,
        slot_path: Some("/co/.git/berth-attachments/f1/spec.pdf".into()),
        stored: Some("/app/files/f1-spec.pdf".into()),
        thumbnail: None,
    };
    let sub = engine
        .submit_prompt(&store, &wf, "Implement this", vec![file.clone()], false)
        .unwrap();
    assert!(
        sub.turn_text.ends_with(
            "- spec.pdf (application/pdf, 10 KB): /co/.git/berth-attachments/f1/spec.pdf"
        )
    );
    let NodePayload::Prompt(p) = &sub.node.payload else {
        panic!()
    };
    assert_eq!(p.attachments, vec![file]);
}

fn origin(title: &str) -> Origin {
    Origin {
        kind: OriginKind::Thought,
        r#ref: String::new(),
        title: title.into(),
        body: String::new(),
        custom_prompt: None,
        url: None,
    }
}

#[test]
fn an_open_workflow_is_closed_so_its_slot_can_be_reused() {
    let (store, mut engine, wf) = setup();
    let slot_id = store.workflow(&wf).unwrap().slot_id;
    // The origin round is suspended on a question: the workflow is open.
    engine.begin_origin_round(&store, &wf).unwrap();
    store
        .set_workflow_state(&wf, WorkflowState::AwaitingAnswer)
        .unwrap();
    let latest = store.workflows_for_slot(&slot_id).unwrap().pop();
    assert_eq!(
        store.slot(&slot_id).unwrap().reuse(latest.as_ref()),
        Ok(true)
    );
    assert!(Engine::create_workflow(&store, &slot_id, origin("Next")).is_err());

    let fx = engine.close_for_reuse(&store, &wf).unwrap();
    assert!(fx.iter().any(|e| matches!(
        e,
        Effect::StateChanged {
            state: WorkflowState::Closed,
            ..
        }
    )));
    let closed = store.workflow(&wf).unwrap();
    assert_eq!(closed.state, WorkflowState::Closed);
    assert!(closed.closed_at.is_some());
    // Its suspended round ended as interrupted.
    assert!(store.open_round(&wf).unwrap().is_none());

    let next = Engine::create_workflow(&store, &slot_id, origin("Next")).unwrap();
    assert_eq!(next.slot_id, slot_id);
    assert_eq!(store.workflows_for_slot(&slot_id).unwrap().len(), 2);
    assert_eq!(
        store.active_workflow(&slot_id).unwrap().unwrap().id,
        next.id
    );
}

/// A main-lane transcript entry with the `uuid` Claude Code gives it.
fn entry(seq: u64, kind: &str, uuid: &str) -> SidecarToApp {
    SidecarToApp::RawEvent(RawEvent {
        seq,
        kind: kind.into(),
        json: serde_json::json!({"type": kind, "uuid": uuid, "message": {"content": []}}),
        branch_id: None,
    })
}

fn summary(done: &str) -> SidecarToApp {
    SidecarToApp::NodeSummary(NodeSummary {
        summary: Summary {
            done: vec![done.into()],
            remaining: vec![],
            follow_ups: vec![],
            notes: None,
        },
    })
}

#[test]
fn rewinding_drops_the_flow_from_a_prompt_and_resumes_before_it() {
    let (store, mut engine, wf) = setup();
    engine.session_started(&wf);
    // Round 1, section 1.
    engine
        .submit_prompt(&store, &wf, "one", vec![], false)
        .unwrap();
    engine
        .apply(&store, &wf, &entry(1, "assistant", "u1a"))
        .unwrap();
    engine.apply(&store, &wf, &entry(2, "user", "u1b")).unwrap();
    engine.apply(&store, &wf, &summary("first")).unwrap();
    Engine::record_snapshot(&store, &wf, 1, SnapshotKind::Summary, "t1").unwrap();
    engine
        .apply(&store, &wf, &ended(TerminalKind::WriteSummary, None))
        .unwrap();
    // Round 2, section 2: a subagent, a summary.
    let two = engine
        .submit_prompt(&store, &wf, "two", vec![], false)
        .unwrap();
    engine
        .apply(&store, &wf, &entry(3, "assistant", "u2a"))
        .unwrap();
    engine
        .apply(
            &store,
            &wf,
            &SidecarToApp::BranchSpawned(BranchSpawned {
                branch_id: "b1".into(),
                parent_tool_use_id: "t".into(),
                parent_branch_id: None,
                agent_name: "a".into(),
                description: "d".into(),
                prompt: "p".into(),
                parallel_group: None,
            }),
        )
        .unwrap();
    engine
        .apply(&store, &wf, &decision("in lane", Some("b1")))
        .unwrap();
    engine.apply(&store, &wf, &summary("second")).unwrap();
    Engine::record_snapshot(&store, &wf, 2, SnapshotKind::Summary, "t2").unwrap();
    engine
        .apply(&store, &wf, &ended(TerminalKind::WriteSummary, None))
        .unwrap();
    // Round 3 carries a review comment and ends on a question.
    let snap = store.snapshots(&wf).unwrap();
    store
        .insert_review_comment(&NewReviewComment {
            workflow_id: wf.clone(),
            from_snapshot: "t1".into(),
            to_snapshot: "t2".into(),
            preset: DiffPreset::Section,
            path: "a.rs".into(),
            side: DiffSide::New,
            line: 1,
            body: "rename".into(),
        })
        .unwrap();
    engine
        .submit_prompt(&store, &wf, "three", vec![], false)
        .unwrap();
    assert!(store.pending_review_comments(&wf).unwrap().is_empty());
    engine
        .apply(&store, &wf, &entry(4, "assistant", "u3a"))
        .unwrap();
    engine
        .apply(
            &store,
            &wf,
            &SidecarToApp::NodeQuestion(NodeQuestion {
                question_id: "q".into(),
                question: "?".into(),
                options: vec![],
                allow_free_text: true,
            }),
        )
        .unwrap();
    assert_eq!(snap.len(), 4);

    // Not while the agent works.
    let three = store.workflow_view(&wf).unwrap().nodes[5].id.clone();
    store
        .set_workflow_state(&wf, WorkflowState::Running)
        .unwrap();
    assert!(engine.rewind(&store, &wf, &three).is_err());
    store
        .set_workflow_state(&wf, WorkflowState::AwaitingAnswer)
        .unwrap();
    // Only to a prompt.
    let origin = store.workflow_view(&wf).unwrap().nodes[0].id.clone();
    assert!(engine.rewind(&store, &wf, &origin).is_err());

    // Back to before "two".
    let (rewound, fx) = engine.rewind(&store, &wf, &two.node.id).unwrap();
    assert_eq!(rewound.prompt.text, "two");
    assert!(!rewound.reopened);
    assert!(fx.contains(&Effect::StateChanged {
        workflow_id: wf.clone(),
        state: WorkflowState::Idle
    }));
    let view = store.workflow_view(&wf).unwrap();
    let kinds: Vec<&str> = view
        .nodes
        .iter()
        .map(|n| n.payload.kind().as_str())
        .collect();
    assert_eq!(kinds, ["origin", "prompt", "summary", "rewind"]);
    let berth_protocol::NodePayload::Rewind(r) = &view.nodes[3].payload else {
        panic!()
    };
    assert_eq!(r.dropped_rounds, 2);
    assert_eq!(view.rounds.len(), 1);
    assert!(view.branches.is_empty());
    assert_eq!(view.workflow.state, WorkflowState::Idle);
    assert_eq!(view.workflow.section_no, 2);
    let snaps: Vec<(u32, SnapshotKind)> = view
        .snapshots
        .iter()
        .map(|s| (s.section_no, s.kind))
        .collect();
    assert_eq!(
        snaps,
        [(1, SnapshotKind::Summary), (2, SnapshotKind::Start)]
    );
    // The comment that went with "three" waits for the next prompt again.
    assert_eq!(store.pending_review_comments(&wf).unwrap().len(), 1);
    // The agent goes on from the last entry before "two".
    assert_eq!(
        store.workflow_resume_at(&wf).unwrap().as_deref(),
        Some("u1b")
    );

    // The next prompt says the files were not rewound.
    let sub = engine
        .submit_prompt(&store, &wf, "two, better", vec![], false)
        .unwrap();
    assert!(sub.turn_text.starts_with(berth_core::prompt::REWIND_NOTE));
    assert!(sub.turn_text.contains("two, better"));
    // A fresh session that answers settles the rewind.
    engine.session_started(&wf);
    engine
        .apply(&store, &wf, &entry(1, "assistant", "u4a"))
        .unwrap();
    assert_eq!(store.workflow_resume_at(&wf).unwrap(), None);
    engine.apply(&store, &wf, &summary("again")).unwrap();
    engine
        .apply(&store, &wf, &ended(TerminalKind::WriteSummary, None))
        .unwrap();
    let sub = engine
        .submit_prompt(&store, &wf, "four", vec![], false)
        .unwrap();
    assert!(!sub.turn_text.contains("rewound"));

    // A second rewind resumes on the new branch, past the dropped rounds' entries.
    let after = store.workflow_view(&wf).unwrap();
    let four = after.nodes.last().unwrap().id.clone();
    engine
        .apply(&store, &wf, &ended(TerminalKind::Interrupted, None))
        .unwrap();
    engine.rewind(&store, &wf, &four).unwrap();
    assert_eq!(
        store.workflow_resume_at(&wf).unwrap().as_deref(),
        Some("u4a")
    );
}

#[test]
fn rewinding_past_an_outcome_reopens_the_workflow() {
    let (store, mut engine, wf) = setup();
    engine.session_started(&wf);
    engine
        .submit_prompt(&store, &wf, "one", vec![], false)
        .unwrap();
    engine
        .apply(&store, &wf, &entry(1, "assistant", "u1"))
        .unwrap();
    engine
        .apply(&store, &wf, &ended(TerminalKind::WriteSummary, None))
        .unwrap();
    let two = engine
        .submit_prompt(&store, &wf, "ship it", vec![], false)
        .unwrap();
    engine
        .apply(&store, &wf, &entry(2, "assistant", "u2"))
        .unwrap();
    engine
        .apply(
            &store,
            &wf,
            &SidecarToApp::NodeOutcome(NodeOutcome {
                outcome: Outcome {
                    kind: OutcomeKind::Pr,
                    r#ref: "https://github.com/o/r/pull/1".into(),
                    summary: "s".into(),
                    follow_ups: vec![],
                    title: None,
                },
            }),
        )
        .unwrap();
    assert_eq!(store.workflow(&wf).unwrap().state, WorkflowState::Closed);
    let (rewound, _) = engine.rewind(&store, &wf, &two.node.id).unwrap();
    assert!(rewound.reopened);
    let w = store.workflow(&wf).unwrap();
    assert_eq!(w.state, WorkflowState::Idle);
    assert_eq!(w.closed_at, None);
}

#[test]
fn writing_to_a_closed_workflow_continues_it_in_its_slot() {
    let (store, mut engine, wf) = setup();
    let home = store.workflow(&wf).unwrap().slot_id;
    engine.session_started(&wf);
    engine
        .submit_prompt(&store, &wf, "one", vec![], false)
        .unwrap();
    engine
        .apply(
            &store,
            &wf,
            &SidecarToApp::NodeOutcome(NodeOutcome {
                outcome: Outcome {
                    kind: OutcomeKind::Pr,
                    r#ref: "https://github.com/o/r/pull/1".into(),
                    summary: "s".into(),
                    follow_ups: vec![],
                    title: None,
                },
            }),
        )
        .unwrap();
    engine
        .apply(&store, &wf, &ended(TerminalKind::RecordOutcome, None))
        .unwrap();
    assert_eq!(store.workflow(&wf).unwrap().state, WorkflowState::Closed);
    let nodes = store.workflow_view(&wf).unwrap().nodes.len();

    let fx = engine.continue_closed(&store, &wf).unwrap();
    let w = store.workflow(&wf).unwrap();
    assert_eq!(w.state, WorkflowState::Idle);
    assert_eq!(w.closed_at, None);
    // Section 1 had the round: section 2 starts from the checkout as it is.
    assert_eq!(w.section_no, 2);
    assert!(fx.contains(&Effect::Snapshot {
        workflow_id: wf.clone(),
        section_no: 2,
        kind: SnapshotKind::Start
    }));
    // Same slot, same conversation: no marker, and the prompt goes as typed.
    assert_eq!(store.workflow_view(&wf).unwrap().nodes.len(), nodes);
    let sub = engine
        .submit_prompt(&store, &wf, "also the changelog", vec![], false)
        .unwrap();
    assert_eq!(sub.turn_text, "also the changelog");
    // An open workflow is left as it is.
    assert!(engine.continue_closed(&store, &wf).unwrap().is_empty());

    // Once its slot moved on to another workflow, it opens from the history.
    store
        .set_workflow_state(&wf, WorkflowState::Closed)
        .unwrap();
    Engine::create_workflow(&store, &home, origin("Next")).unwrap();
    assert!(engine.continue_closed(&store, &wf).is_err());
}

#[test]
fn a_message_sent_while_the_agent_works_joins_its_round() {
    let (store, mut engine, wf) = setup();
    engine.session_started(&wf);
    let first = engine
        .submit_prompt(&store, &wf, "add a greeting", vec![], false)
        .unwrap();
    engine
        .apply(&store, &wf, &entry(1, "assistant", "u1"))
        .unwrap();
    assert_eq!(store.workflow(&wf).unwrap().state, WorkflowState::Running);

    // A command would be a round of its own: it waits.
    assert!(
        engine
            .submit_prompt(&store, &wf, "/compact", vec![], true)
            .is_err()
    );
    let queued = engine
        .submit_prompt(&store, &wf, "also say goodbye", vec![], false)
        .unwrap();
    assert_eq!(queued.turn_text, "also say goodbye");
    assert_eq!(queued.node.round_id, first.node.round_id);
    let rounds = store.rounds(&wf).unwrap();
    assert_eq!(rounds.len(), 1);
    let view = store.workflow_view(&wf).unwrap();
    assert!(!view.sent_while_working(&first.node));
    assert!(view.sent_while_working(&queued.node));

    engine.apply(&store, &wf, &summary("both")).unwrap();
    engine
        .apply(&store, &wf, &ended(TerminalKind::WriteSummary, None))
        .unwrap();
    assert_eq!(store.workflow(&wf).unwrap().state, WorkflowState::Idle);
    // Mid-round, it is no rewind point; the round's own message is.
    let refused = engine.rewind(&store, &wf, &queued.node.id).err();
    assert!(refused.is_some_and(|e| e.to_string().contains("while the agent worked")));
}

#[test]
fn a_reply_answers_in_the_flow_without_ending_the_section() {
    let (store, mut engine, wf) = setup();
    engine.session_started(&wf);
    engine
        .submit_prompt(&store, &wf, "one", vec![], false)
        .unwrap();
    engine.apply(&store, &wf, &summary("first")).unwrap();
    engine
        .apply(&store, &wf, &ended(TerminalKind::WriteSummary, None))
        .unwrap();
    let section = store.workflow(&wf).unwrap().section_no;
    assert_eq!(section, 2);

    engine
        .submit_prompt(&store, &wf, "why TOML?", vec![], false)
        .unwrap();
    engine
        .apply(
            &store,
            &wf,
            &SidecarToApp::NodeReply(berth_protocol::NodeReply {
                reply: berth_protocol::Reply {
                    message: "Comments survive a round trip.".into(),
                },
            }),
        )
        .unwrap();
    engine
        .apply(&store, &wf, &ended(TerminalKind::Reply, None))
        .unwrap();
    let w = store.workflow(&wf).unwrap();
    assert_eq!(w.state, WorkflowState::Idle);
    assert_eq!(w.section_no, section, "a reply ends no section");
    let view = store.workflow_view(&wf).unwrap();
    assert!(matches!(
        view.nodes.last().unwrap().payload,
        NodePayload::Reply(_)
    ));
    // Nothing changed to review: the round is no section work.
    assert!(!view.rounds.last().unwrap().is_work());
    // The slot waits on the user's next message, not on a review.
    let facts = store.workflow_facts(&wf).unwrap();
    assert!(facts.reply_at.is_some());
}

fn second_slot(store: &Store, of: &str) -> Slot {
    let first = store.slot(of).unwrap();
    let mut slot = Slot {
        id: new_id(),
        name: "other".into(),
        checkout_path: Some("/tmp/other".into()),
        branch: "berth/other".into(),
        created_at: now_ms(),
        number: 0,
        ..first
    };
    store.insert_slot(&mut slot).unwrap();
    slot
}

#[test]
fn a_closed_workflow_opens_in_another_slot_and_says_where() {
    let (store, mut engine, wf) = setup();
    let home = store.workflow(&wf).unwrap().slot_id;
    engine.session_started(&wf);
    engine
        .submit_prompt(&store, &wf, "one", vec![], false)
        .unwrap();
    engine
        .apply(&store, &wf, &entry(1, "assistant", "u1"))
        .unwrap();
    engine.apply(&store, &wf, &summary("first")).unwrap();
    Engine::record_snapshot(&store, &wf, 1, SnapshotKind::Summary, "t1").unwrap();
    engine
        .apply(&store, &wf, &ended(TerminalKind::WriteSummary, None))
        .unwrap();
    engine
        .submit_prompt(&store, &wf, "two", vec![], false)
        .unwrap();
    engine
        .apply(
            &store,
            &wf,
            &SidecarToApp::NodeOutcome(NodeOutcome {
                outcome: Outcome {
                    kind: OutcomeKind::Commit,
                    r#ref: "abc".into(),
                    summary: "s".into(),
                    follow_ups: vec![],
                    title: None,
                },
            }),
        )
        .unwrap();
    // The home slot moves on to another workflow; the closed one is history.
    let next = Engine::create_workflow(&store, &home, origin("Next")).unwrap();
    let other = second_slot(&store, &home);

    // A slot hosting an open workflow takes no other.
    let busy = Engine::create_workflow(&store, &other.id, origin("Busy")).unwrap();
    assert!(engine.reopen(&store, &wf, &other).is_err());
    store
        .set_workflow_state(&busy.id, WorkflowState::Closed)
        .unwrap();

    let fx = engine.reopen(&store, &wf, &other).unwrap();
    let w = store.workflow(&wf).unwrap();
    assert_eq!(w.slot_id, other.id);
    assert_eq!(w.state, WorkflowState::Idle);
    // Its section 2 had a round: section 3 starts here, in this checkout.
    assert_eq!(w.section_no, 3);
    assert!(fx.contains(&Effect::Snapshot {
        workflow_id: wf.clone(),
        section_no: 3,
        kind: SnapshotKind::Start
    }));
    // Each slot shows what it hosted last.
    assert_eq!(
        store
            .workflows_for_slot(&other.id)
            .unwrap()
            .last()
            .unwrap()
            .id,
        wf
    );
    assert_eq!(
        store.workflows_for_slot(&home).unwrap().last().unwrap().id,
        next.id
    );
    let view = store.workflow_view(&wf).unwrap();
    let berth_protocol::NodePayload::Reopened(r) = &view.nodes.last().unwrap().payload else {
        panic!()
    };
    assert_eq!(r.checkout, "/tmp/other");
    assert_eq!(r.from_checkout.as_deref(), Some("/tmp/co"));
    assert_eq!(r.from_branch.as_deref(), Some("berth/fix"));
    // The next prompt says so.
    let sub = engine
        .submit_prompt(&store, &wf, "go on", vec![], false)
        .unwrap();
    assert!(
        sub.turn_text
            .starts_with("(This conversation was reopened in another checkout")
    );
    assert!(sub.turn_text.contains("`/tmp/co`"));
    assert!(sub.turn_text.ends_with("go on"));
}

#[test]
fn a_claude_code_session_becomes_a_workflow_that_resumes_it() {
    let (store, mut engine, wf) = setup();
    let home = store.workflow(&wf).unwrap().slot_id;
    store
        .set_workflow_state(&wf, WorkflowState::Closed)
        .unwrap();
    let slot = store.slot(&home).unwrap();
    let origin = Origin {
        kind: OriginKind::Session,
        r#ref: "sess-9".into(),
        title: "Fix the login bug".into(),
        body: "the login fails".into(),
        custom_prompt: None,
        url: None,
    };
    let (adopted, fx) = engine
        .adopt_session(
            &store,
            &slot,
            origin,
            "sess-9",
            (Some("/repo".into()), Some("main".into())),
        )
        .unwrap();
    assert_eq!(adopted.claude_session_id.as_deref(), Some("sess-9"));
    assert_eq!(adopted.state, WorkflowState::Idle);
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::Snapshot { section_no: 1, .. }))
    );
    let view = store.workflow_view(&adopted.id).unwrap();
    let kinds: Vec<&str> = view
        .nodes
        .iter()
        .map(|n| n.payload.kind().as_str())
        .collect();
    assert_eq!(kinds, ["origin", "reopened"]);
    // Its first prompt is a plain turn on the resumed session, with the note.
    let sub = engine
        .submit_prompt(&store, &adopted.id, "where were we?", vec![], false)
        .unwrap();
    assert!(
        sub.turn_text
            .contains("Earlier messages ran in `/repo` (branch `main`)")
    );
}

#[test]
fn rewinding_past_a_reopen_keeps_the_new_checkout() {
    let (store, mut engine, wf) = setup();
    let home = store.workflow(&wf).unwrap().slot_id;
    engine.session_started(&wf);
    engine
        .submit_prompt(&store, &wf, "zero", vec![], false)
        .unwrap();
    engine
        .apply(&store, &wf, &entry(1, "assistant", "u0"))
        .unwrap();
    engine
        .apply(&store, &wf, &ended(TerminalKind::WriteSummary, None))
        .unwrap();
    let one = engine
        .submit_prompt(&store, &wf, "one", vec![], false)
        .unwrap();
    engine
        .apply(&store, &wf, &entry(1, "assistant", "u1"))
        .unwrap();
    engine
        .apply(&store, &wf, &ended(TerminalKind::WriteSummary, None))
        .unwrap();
    // Opened in another slot from the history, then on with a prompt.
    let other = second_slot(&store, &home);
    store
        .set_workflow_state(&wf, WorkflowState::Closed)
        .unwrap();
    engine.reopen(&store, &wf, &other).unwrap();
    engine
        .submit_prompt(&store, &wf, "two", vec![], false)
        .unwrap();
    engine
        .apply(&store, &wf, &entry(2, "assistant", "u2"))
        .unwrap();
    engine
        .apply(&store, &wf, &ended(TerminalKind::WriteSummary, None))
        .unwrap();

    // Back to before "one": the move stays, and this checkout gets a new start.
    let (_, fx) = engine.rewind(&store, &wf, &one.node.id).unwrap();
    assert!(fx.iter().any(|e| matches!(
        e,
        Effect::Snapshot {
            kind: SnapshotKind::Start,
            ..
        }
    )));
    let kinds: Vec<&str> = store
        .workflow_view(&wf)
        .unwrap()
        .nodes
        .iter()
        .map(|n| n.payload.kind().as_str())
        .collect();
    assert_eq!(kinds, ["origin", "prompt", "reopened", "rewind"]);
    // The next prompt tells the agent both.
    let sub = engine
        .submit_prompt(&store, &wf, "one again", vec![], false)
        .unwrap();
    assert!(sub.turn_text.contains("reopened in another checkout"));
    assert!(sub.turn_text.contains("rewound"));
    assert!(sub.turn_text.ends_with("one again"));
}

#[test]
fn context_reports_are_kept_on_the_workflow() {
    let (store, mut engine, wf) = setup();
    assert!(store.workflow_view(&wf).unwrap().context.is_none());
    let report = SidecarToApp::SessionContext(berth_protocol::SessionContext {
        categories: vec![
            berth_protocol::ContextCategory {
                name: "Messages".into(),
                tokens: 30_000,
                kind: berth_protocol::ContextKind::Used,
            },
            berth_protocol::ContextCategory {
                name: "Free space".into(),
                tokens: 170_000,
                kind: berth_protocol::ContextKind::Free,
            },
        ],
        max_tokens: None,
    });
    let fx = engine.apply(&store, &wf, &report).unwrap();
    assert!(fx.iter().any(|e| matches!(e, Effect::FlowChanged { .. })));
    let context = store.workflow_view(&wf).unwrap().context.unwrap();
    assert_eq!((context.used(), context.window()), (30_000, 200_000));
    // The window Claude Code reports is kept with it (ADR 0025).
    let small = SidecarToApp::SessionContext(berth_protocol::SessionContext {
        categories: vec![],
        max_tokens: Some(48_000),
    });
    engine.apply(&store, &wf, &small).unwrap();
    let context = store.workflow_view(&wf).unwrap().context.unwrap();
    assert_eq!(context.window(), 48_000);
}

#[test]
fn slash_commands_go_as_typed_and_end_without_a_summary() {
    let (store, mut engine, wf) = setup();
    engine.session_started(&wf);
    store
        .insert_review_comment(&NewReviewComment {
            workflow_id: wf.clone(),
            from_snapshot: "a".into(),
            to_snapshot: "b".into(),
            preset: DiffPreset::Section,
            path: "src/x.rs".into(),
            side: DiffSide::New,
            line: 3,
            body: "rename".into(),
        })
        .unwrap();
    store
        .insert_node(
            &wf,
            None,
            None,
            NodePayload::Rewind(berth_protocol::Rewind {
                dropped_rounds: 1,
                from: 0,
                to: 1,
            }),
            None,
        )
        .unwrap();

    // The command goes verbatim: the comment and the rewind note wait.
    let sub = engine
        .submit_prompt(&store, &wf, "/context", vec![], true)
        .unwrap();
    assert_eq!(sub.turn_text, "/context");
    assert_eq!(store.pending_review_comments(&wf).unwrap().len(), 1);
    let fx = engine
        .apply(
            &store,
            &wf,
            &ended(
                TerminalKind::Command,
                Some("## Context Usage\n\n13.7k / 1m"),
            ),
        )
        .unwrap();
    assert!(fx.iter().any(|e| matches!(e, Effect::FlowChanged { .. })));
    let view = store.workflow_view(&wf).unwrap();
    assert_eq!(view.workflow.state, WorkflowState::Idle);
    assert_eq!(view.workflow.section_no, 1, "a command ends no section");
    let Some(NodePayload::Command(c)) = view.nodes.last().map(|n| &n.payload) else {
        panic!("no command node");
    };
    assert_eq!(c.command, "/context");
    assert!(c.output.starts_with("## Context Usage"));
    let last = view.nodes.last().unwrap();
    assert_eq!(last.round_id, view.nodes[view.nodes.len() - 2].round_id);
    assert!(last.round_id.is_some());

    // /compact shows its Compacted node, not a Command node.
    engine
        .submit_prompt(&store, &wf, "/compact", vec![], true)
        .unwrap();
    engine
        .apply(
            &store,
            &wf,
            &SidecarToApp::NodeCompacted(berth_protocol::NodeCompacted {
                compacted: berth_protocol::Compacted {
                    trigger: berth_protocol::CompactTrigger::Manual,
                    pre_tokens: 17_895,
                    post_tokens: Some(1_961),
                },
            }),
        )
        .unwrap();
    engine
        .apply(
            &store,
            &wf,
            &ended(TerminalKind::Command, Some("Compacted")),
        )
        .unwrap();
    let kinds: Vec<&str> = store
        .workflow_view(&wf)
        .unwrap()
        .nodes
        .iter()
        .map(|n| n.payload.kind().as_str())
        .collect();
    assert_eq!(
        kinds,
        [
            "origin",
            "rewind",
            "prompt",
            "command",
            "prompt",
            "compacted"
        ]
    );

    // The next real prompt carries what waited.
    let sub = engine
        .submit_prompt(&store, &wf, "go on", vec![], false)
        .unwrap();
    assert!(sub.turn_text.contains("rewound"));
    assert!(sub.turn_text.contains("src/x.rs:3 (new): rename"));
}

#[test]
fn a_compaction_after_a_rewind_settles_where_sessions_resume() {
    let (store, mut engine, wf) = setup();
    store.set_workflow_resume_at(&wf, Some("u1")).unwrap();
    engine.session_started(&wf);
    engine
        .submit_prompt(&store, &wf, "/compact", vec![], true)
        .unwrap();
    engine
        .apply(
            &store,
            &wf,
            &SidecarToApp::NodeCompacted(berth_protocol::NodeCompacted {
                compacted: berth_protocol::Compacted {
                    trigger: berth_protocol::CompactTrigger::Manual,
                    pre_tokens: 10_000,
                    post_tokens: None,
                },
            }),
        )
        .unwrap();
    assert_eq!(store.workflow_resume_at(&wf).unwrap(), None);
}

#[test]
fn a_command_round_without_an_open_round_still_shows() {
    let (store, mut engine, wf) = setup();
    engine.session_started(&wf);
    // The round's opening was lost (the app restarted between): it ends anyway.
    engine
        .apply(&store, &wf, &ended(TerminalKind::Command, Some("Done")))
        .unwrap();
    let view = store.workflow_view(&wf).unwrap();
    let Some(NodePayload::Command(c)) = view.nodes.last().map(|n| &n.payload) else {
        panic!("no command node");
    };
    assert_eq!((c.command.as_str(), c.output.as_str()), ("", "Done"));
    assert!(view.rounds.iter().all(|r| !r.is_work()));
}

#[test]
fn prose_that_starts_with_a_slash_carries_the_notes() {
    let (store, mut engine, wf) = setup();
    engine.session_started(&wf);
    store
        .insert_node(
            &wf,
            None,
            None,
            NodePayload::Rewind(berth_protocol::Rewind {
                dropped_rounds: 1,
                from: 0,
                to: 1,
            }),
            None,
        )
        .unwrap();
    // The session's list knows no `/tmp`: the backend sends it as prose.
    let sub = engine
        .submit_prompt(&store, &wf, "/tmp is full, clean it", vec![], false)
        .unwrap();
    assert!(sub.turn_text.starts_with(berth_core::prompt::REWIND_NOTE));
}

#[test]
fn a_rewind_declares_the_one_turn_it_drops() {
    let (store, mut engine, wf) = setup();
    engine.session_started(&wf);
    let one = engine
        .submit_prompt(&store, &wf, "one", vec![], false)
        .unwrap();
    engine
        .apply(&store, &wf, &entry(1, "assistant", "u1"))
        .unwrap();
    engine
        .apply(&store, &wf, &ended(TerminalKind::WriteSummary, None))
        .unwrap();
    let two = engine
        .submit_prompt(&store, &wf, "two", vec![], false)
        .unwrap();
    assert_ne!(one.turn_uuid, two.turn_uuid);
    engine
        .apply(&store, &wf, &entry(2, "assistant", "u2"))
        .unwrap();
    engine
        .apply(&store, &wf, &ended(TerminalKind::WriteSummary, None))
        .unwrap();

    // Back to "two": exactly its turn goes, and the resume says so.
    engine.rewind(&store, &wf, &two.node.id).unwrap();
    assert_eq!(
        store.workflow_resume_at(&wf).unwrap().as_deref(),
        Some("u1")
    );
    assert_eq!(
        store.workflow_resume_drops(&wf).unwrap().as_deref(),
        Some(two.turn_uuid.as_str())
    );
    // Settling the resume point settles it too.
    store.set_workflow_resume_at(&wf, None).unwrap();
    assert_eq!(store.workflow_resume_drops(&wf).unwrap(), None);
}

#[test]
fn a_rewind_over_several_turns_declares_none() {
    let (store, mut engine, wf) = setup();
    engine.session_started(&wf);
    engine
        .submit_prompt(&store, &wf, "zero", vec![], false)
        .unwrap();
    engine
        .apply(&store, &wf, &entry(1, "assistant", "u0"))
        .unwrap();
    engine
        .apply(&store, &wf, &ended(TerminalKind::WriteSummary, None))
        .unwrap();
    let one = engine
        .submit_prompt(&store, &wf, "one", vec![], false)
        .unwrap();
    engine
        .apply(&store, &wf, &entry(2, "assistant", "u1"))
        .unwrap();
    engine
        .apply(&store, &wf, &ended(TerminalKind::WriteSummary, None))
        .unwrap();
    engine
        .submit_prompt(&store, &wf, "two", vec![], false)
        .unwrap();
    engine
        .apply(&store, &wf, &entry(3, "assistant", "u2"))
        .unwrap();
    engine
        .apply(&store, &wf, &ended(TerminalKind::WriteSummary, None))
        .unwrap();
    engine.rewind(&store, &wf, &one.node.id).unwrap();
    assert!(store.workflow_resume_at(&wf).unwrap().is_some());
    assert_eq!(store.workflow_resume_drops(&wf).unwrap(), None);
}

/// A second slot in the same repo, with an idle workflow.
fn api_workflow(store: &Store, beside: &str) -> String {
    let slot = second_slot(store, &store.workflow(beside).unwrap().slot_id);
    let origin = Origin {
        kind: OriginKind::Thought,
        r#ref: String::new(),
        title: "Build the API".into(),
        body: String::new(),
        custom_prompt: None,
        url: None,
    };
    let wf = Engine::create_workflow(store, &slot.id, origin).unwrap();
    store
        .set_workflow_state(&wf.id, WorkflowState::Idle)
        .unwrap();
    wf.id
}

fn messages(store: &Store, wf: &str) -> Vec<berth_protocol::SlotMessage> {
    store
        .nodes(wf)
        .unwrap()
        .into_iter()
        .filter_map(|n| match n.payload {
            NodePayload::Message(m) => Some(m),
            _ => None,
        })
        .collect()
}

#[test]
fn messages_between_slots_wake_join_and_stop_at_the_guard() {
    use berth_core::messages::{Delivery, MAX_HOPS, plan};
    use berth_core::{Recipient, Sender};
    use berth_protocol::{MessageDirection, MessageStatus};

    let (store, mut engine, front) = setup();
    let api = api_workflow(&store, &front);
    engine
        .submit_prompt(&store, &front, "Build the page", vec![], false)
        .unwrap();

    // The user's round sends the first message: it wakes the idle agent.
    let hop = Engine::message_hop(&store, &front).unwrap();
    assert_eq!(hop, 1);
    let to = store.workflow(&api).unwrap();
    let slot_state = store.slot(&to.slot_id).unwrap().state;
    let delivery = plan(to.state, slot_state, hop, false);
    assert_eq!(delivery, Delivery::Wake);
    let from = store.workflow(&front).unwrap();
    let sender = Sender {
        workflow: &from,
        name: "Fix it",
        branch: "berth/fix",
    };
    let posted = engine
        .post_message(
            &store,
            &sender,
            &Recipient {
                workflow: &to,
                name: "Build the API",
            },
            "GET /users 500s",
            hop,
            &delivery,
        )
        .unwrap();
    let (turn, _) = posted.turn.expect("delivered now");
    assert!(turn.starts_with("Message from slot \"Fix it\""), "{turn}");
    assert_eq!(store.workflow(&api).unwrap().state, WorkflowState::Running);
    let sent = messages(&store, &front);
    let got = messages(&store, &api);
    assert_eq!((sent.len(), got.len()), (1, 1));
    assert_eq!(sent[0].direction, MessageDirection::Sent);
    assert_eq!(got[0].direction, MessageDirection::Received);
    assert_eq!(got[0].status, MessageStatus::Woke);
    assert_eq!(got[0].peer_workflow_id, front);
    assert_eq!(sent[0].message_id, got[0].message_id);

    // Its answer, from the round the message woke, joins the sender's round.
    let hop = Engine::message_hop(&store, &api).unwrap();
    assert_eq!(hop, 2);
    let to = store.workflow(&front).unwrap();
    assert_eq!(plan(to.state, SlotState::Ready, hop, true), Delivery::Join);
    let from = store.workflow(&api).unwrap();
    let sender = Sender {
        workflow: &from,
        name: "Build the API",
        branch: "berth/api",
    };
    engine
        .post_message(
            &store,
            &sender,
            &Recipient {
                workflow: &to,
                name: "Fix it",
            },
            "Fixed in 3f2a",
            hop,
            &Delivery::Join,
        )
        .unwrap();
    let rounds = store.rounds(&front).unwrap();
    assert_eq!(rounds.len(), 1, "it joined the running round");

    // A reply to the reply is past the guard: held, and the user decides.
    let hop = Engine::message_hop(&store, &front).unwrap();
    assert_eq!(hop, MAX_HOPS + 1);
    let to = store.workflow(&api).unwrap();
    let delivery = plan(to.state, SlotState::Ready, hop, true);
    assert!(matches!(delivery, Delivery::Hold(_)));
    let from = store.workflow(&front).unwrap();
    let sender = Sender {
        workflow: &from,
        name: "Fix it",
        branch: "berth/fix",
    };
    let held = engine
        .post_message(
            &store,
            &sender,
            &Recipient {
                workflow: &to,
                name: "Build the API",
            },
            "Thanks!",
            hop,
            &delivery,
        )
        .unwrap();
    assert!(held.turn.is_none());
    assert_eq!(
        store
            .workflow_facts(&api)
            .unwrap()
            .held_message
            .map(|h| h.1),
        Some("Fix it".to_owned())
    );

    // Sent on by the user once the agent is idle again: a round of its own,
    // counting from zero; both sides say so.
    store.set_workflow_state(&api, WorkflowState::Idle).unwrap();
    let open = store.open_round(&api).unwrap().unwrap();
    store
        .end_round(&open.id, TerminalKind::WriteSummary, 0, 0.0, 1, now_ms())
        .unwrap();
    let (turn, _) = engine
        .release_message(&store, &api, &held.message_id, &Delivery::Wake)
        .unwrap();
    assert!(turn.contains("Thanks!"), "{turn}");
    let round = store.open_round(&api).unwrap().unwrap();
    assert_eq!(store.round_hop(&round.id).unwrap(), 0);
    for side in [&front, &api] {
        let m = messages(&store, side).into_iter().last().unwrap();
        assert_eq!(m.status, MessageStatus::Released, "{side}");
    }
    assert!(store.workflow_facts(&api).unwrap().held_message.is_none());
    assert!(
        Engine::dismiss_message(&store, &api, &held.message_id).is_err(),
        "settled once"
    );
}
