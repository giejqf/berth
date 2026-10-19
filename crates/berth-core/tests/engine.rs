#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use berth_core::{
    AnswerDelivery, Effect, Engine, NewReviewComment, Slot, SlotState, SnapshotKind, Store,
    WorkflowState,
    ids::{new_id, now_ms},
};
use berth_protocol::{
    BranchCollected, BranchSpawned, BranchStatus, Decision, DiffPreset, DiffSide, NodeArtifact,
    NodeDecision, NodeOutcome, NodePayload, NodeQuestion, NodeSummary, Origin, OriginKind, Outcome,
    OutcomeKind, RawEvent, RoundEnded, SessionCost, SessionError, SessionStarted, SidecarToApp,
    Summary, TerminalKind,
};

fn setup() -> (Store, Engine, String) {
    let store = Store::open_in_memory().unwrap();
    let repo = store.upsert_repo(Path::new("/tmp/repo")).unwrap();
    let slot = Slot {
        id: new_id(),
        repo_id: repo.id,
        slot_type: "worktree".into(),
        name: "fix".into(),
        custom_name: None,
        state: SlotState::Ready,
        state_detail: None,
        checkout_path: Some("/tmp/co".into()),
        placement: "host".into(),
        branch: "berth/fix".into(),
        base_ref: "main".into(),
        created_at: now_ms(),
    };
    store.insert_slot(&slot).unwrap();
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
        .submit_prompt(&store, &wf, "Do the thing", vec![])
        .unwrap();
    assert_eq!(sub.turn_text, "Do the thing");
    assert_eq!(store.workflow(&wf).unwrap().state, WorkflowState::Running);
    assert!(
        engine.submit_prompt(&store, &wf, "again", vec![]).is_err(),
        "running workflows reject prompts"
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
    engine.submit_prompt(&store, &wf, "go", vec![]).unwrap();
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
fn unterminated_round_gets_a_stub_summary() {
    let (store, mut engine, wf) = setup();
    engine.session_started(&wf);
    engine.submit_prompt(&store, &wf, "go", vec![]).unwrap();
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
    engine.submit_prompt(&store, &wf, "go", vec![]).unwrap();
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
    engine.submit_prompt(&store, &wf, "go", vec![]).unwrap();
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
    engine.submit_prompt(&store, &wf, "go", vec![]).unwrap();
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

    engine.submit_prompt(&store, &wf, "again", vec![]).unwrap();
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
        .submit_prompt(&store, &wf, "Address these", vec![])
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
        .submit_prompt(&store, &wf, "Take a screenshot", vec![])
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
        .submit_prompt(&store, &wf, "Implement this", vec![file.clone()])
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
