#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use berth_core::{
    Slot, SlotState, Store,
    ids::{new_id, now_ms},
};

fn slot(repo_id: &str) -> Slot {
    Slot {
        id: new_id(),
        repo_id: repo_id.to_owned(),
        slot_type: "worktree".into(),
        name: "add-a-test".into(),
        custom_name: None,
        model: None,
        effort: None,
        state: SlotState::Ready,
        state_detail: None,
        checkout_path: Some("/tmp/co".into()),
        placement: "host".into(),
        branch: "berth/add-a-test".into(),
        base_ref: "main".into(),
        created_at: now_ms(),
        number: 0,
    }
}

#[test]
fn a_slot_can_be_renamed_and_unnamed() {
    let store = Store::open_in_memory().unwrap();
    let repo = store.upsert_repo(Path::new("/tmp/repo")).unwrap();
    let mut s = slot(&repo.id);
    store.insert_slot(&mut s).unwrap();
    assert_eq!(store.slot(&s.id).unwrap().custom_name, None);

    let renamed = store.set_slot_custom_name(&s.id, Some("tests")).unwrap();
    assert_eq!(renamed.custom_name.as_deref(), Some("tests"));
    assert_eq!(renamed.name, "add-a-test");
    assert_eq!(
        store.live_slots(&repo.id).unwrap()[0]
            .custom_name
            .as_deref(),
        Some("tests")
    );

    let cleared = store.set_slot_custom_name(&s.id, None).unwrap();
    assert_eq!(cleared.custom_name, None);
}

#[test]
fn renaming_a_missing_slot_fails() {
    let store = Store::open_in_memory().unwrap();
    assert!(store.set_slot_custom_name("nope", Some("x")).is_err());
}

#[test]
fn a_database_from_0_1_0_gains_custom_names() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("berth.db");
    {
        // The schema as 0.1.0 left it.
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(include_str!("../migrations/0001_init.sql"))
            .unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute(
            "INSERT INTO repos (id, path, created_at) VALUES ('r', '/tmp/repo', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO slots (id, repo_id, type, name, state, placement, branch, base_ref, created_at)
             VALUES ('s', 'r', 'worktree', 'old', 'ready', 'host', 'berth/old', 'main', 0)",
            [],
        )
        .unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(store.slot("s").unwrap().custom_name, None);
    let renamed = store.set_slot_custom_name("s", Some("kept")).unwrap();
    assert_eq!(renamed.custom_name.as_deref(), Some("kept"));
}

#[test]
fn which_slots_can_take_a_new_workflow() {
    use berth_core::{ReuseBlocked, Workflow, WorkflowState};
    let store = Store::open_in_memory().unwrap();
    let repo = store.upsert_repo(Path::new("/tmp/repo")).unwrap();
    let wf = |state| Workflow {
        id: new_id(),
        slot_id: String::new(),
        origin: berth_protocol::Origin {
            kind: berth_protocol::OriginKind::Thought,
            r#ref: String::new(),
            title: "x".into(),
            body: String::new(),
            custom_prompt: None,
            url: None,
        },
        state,
        claude_session_id: None,
        section_no: 1,
        total_cost_usd: 0.0,
        last_error: None,
        created_at: now_ms(),
        closed_at: None,
    };
    let at = |state| Slot {
        state,
        ..slot(&repo.id)
    };
    use SlotState as S;
    use WorkflowState as W;
    assert_eq!(at(S::Ready).reuse(None), Ok(false));
    assert_eq!(at(S::Ready).reuse(Some(&wf(W::Closed))), Ok(false));
    assert_eq!(at(S::Running).reuse(Some(&wf(W::Idle))), Ok(true));
    assert_eq!(at(S::Running).reuse(Some(&wf(W::AwaitingAnswer))), Ok(true));
    assert_eq!(
        at(S::Running).reuse(Some(&wf(W::Running))),
        Err(ReuseBlocked::Working)
    );
    assert_eq!(
        at(S::Running).reuse(Some(&wf(W::Starting))),
        Err(ReuseBlocked::Working)
    );
    assert_eq!(at(S::Creating).reuse(None), Err(ReuseBlocked::Creating));
    assert_eq!(at(S::Failed).reuse(None), Err(ReuseBlocked::Failed));
    assert_eq!(at(S::Destroying).reuse(None), Err(ReuseBlocked::Destroying));
}

#[test]
fn slots_are_grouped_into_windows() {
    let store = Store::open_in_memory().unwrap();
    let repo = store.upsert_repo(Path::new("/tmp/repo")).unwrap();
    let slot = |name: &str, at: i64| {
        let mut s = Slot {
            id: new_id(),
            repo_id: repo.id.clone(),
            slot_type: "worktree".into(),
            name: name.into(),
            custom_name: None,
            model: None,
            effort: None,
            state: SlotState::Ready,
            state_detail: None,
            checkout_path: Some(format!("/tmp/{name}")),
            placement: "host".into(),
            branch: format!("berth/{name}"),
            base_ref: "main".into(),
            created_at: at,
            number: 0,
        };
        store.insert_slot(&mut s).unwrap();
        s
    };
    // Slots from before windows join the first window.
    let a = slot("a", 1);
    let b = slot("b", 2);
    assert_eq!(store.repo_windows(&repo.id, "w1").unwrap(), ["w1"]);
    assert_eq!(store.slot_window(&a.id).unwrap().as_deref(), Some("w1"));
    assert_eq!(store.slot_window(&b.id).unwrap().as_deref(), Some("w1"));
    // A slot dragged out makes a second window; windows keep their order.
    store.set_slot_window(&b.id, "w2").unwrap();
    assert_eq!(
        store.repo_windows(&repo.id, "unused").unwrap(),
        ["w1", "w2"]
    );
    // Merging moves every slot of one window into the other.
    store.merge_windows(&repo.id, "w2", "w1").unwrap();
    assert_eq!(store.repo_windows(&repo.id, "unused").unwrap(), ["w1"]);
    // A repo without slots still opens one window.
    let other = store.upsert_repo(Path::new("/tmp/other")).unwrap();
    assert_eq!(store.repo_windows(&other.id, "fresh").unwrap(), ["fresh"]);
}

#[test]
fn slots_take_the_lowest_free_number_in_their_repo() {
    let store = Store::open_in_memory().unwrap();
    let repo = store.upsert_repo(Path::new("/tmp/repo")).unwrap();
    let other = store.upsert_repo(Path::new("/tmp/other")).unwrap();
    let add = |repo_id: &str| {
        let mut s = slot(repo_id);
        store.insert_slot(&mut s).unwrap();
        s
    };
    let (a, b, c) = (add(&repo.id), add(&repo.id), add(&repo.id));
    assert_eq!([a.number, b.number, c.number], [1, 2, 3]);
    assert_eq!(store.slot(&b.id).unwrap().number, 2);
    // Each repo counts on its own.
    assert_eq!(add(&other.id).number, 1);
    // A destroyed slot's number is free again; a failed one keeps its own.
    store
        .transition_slot(&b.id, SlotState::Destroying, None)
        .unwrap();
    store
        .transition_slot(&b.id, SlotState::Destroyed, None)
        .unwrap();
    assert_eq!(add(&repo.id).number, 2);
    assert_eq!(add(&repo.id).number, 4);
}

#[test]
fn slots_from_before_numbers_are_numbered_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("berth.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(include_str!("../migrations/0001_init.sql"))
            .unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute(
            "INSERT INTO repos (id, path, created_at) VALUES ('r', '/tmp/repo', 0)",
            [],
        )
        .unwrap();
        for (id, state, at) in [("x", "ready", 3), ("y", "destroyed", 1), ("z", "failed", 2)] {
            conn.execute(
                "INSERT INTO slots (id, repo_id, type, name, state, placement, branch, base_ref, created_at)
                 VALUES (?1, 'r', 'worktree', ?1, ?2, 'host', 'b', 'main', ?3)",
                rusqlite::params![id, state, at],
            )
            .unwrap();
        }
    }
    let store = Store::open(&path).unwrap();
    let n = |id| store.slot(id).unwrap().number;
    assert_eq!([n("z"), n("x"), n("y")], [1, 2, 0]);
}

#[test]
fn agents_work_while_a_round_runs_or_a_slot_is_made_or_torn_down() {
    use berth_core::{Workflow, WorkflowState};
    let store = Store::open_in_memory().unwrap();
    let repo = store.upsert_repo(Path::new("/tmp/repo")).unwrap();
    let mut s = Slot {
        state: SlotState::Creating,
        ..slot(&repo.id)
    };
    store.insert_slot(&mut s).unwrap();
    assert!(store.agents_working().unwrap(), "creating");
    store
        .transition_slot(&s.id, SlotState::Ready, None)
        .unwrap();
    assert!(!store.agents_working().unwrap(), "ready");

    let wf = Workflow {
        id: new_id(),
        slot_id: s.id.clone(),
        origin: berth_protocol::Origin {
            kind: berth_protocol::OriginKind::Thought,
            r#ref: String::new(),
            title: "x".into(),
            body: String::new(),
            custom_prompt: None,
            url: None,
        },
        state: WorkflowState::Running,
        claude_session_id: None,
        section_no: 1,
        total_cost_usd: 0.0,
        last_error: None,
        created_at: now_ms(),
        closed_at: None,
    };
    store.insert_workflow(&wf).unwrap();
    assert!(store.agents_working().unwrap(), "a round runs");
    for idle in [WorkflowState::AwaitingAnswer, WorkflowState::Idle] {
        store.set_workflow_state(&wf.id, idle).unwrap();
        assert!(!store.agents_working().unwrap(), "{idle:?}");
    }

    store
        .transition_slot(&s.id, SlotState::Destroying, None)
        .unwrap();
    assert!(store.agents_working().unwrap(), "destroying");
}

#[test]
fn a_status_set_by_hand_holds_until_anything_newer_happens() {
    use berth_core::{ManualStatus, Workflow, WorkflowState};
    let store = Store::open_in_memory().unwrap();
    let repo = store.upsert_repo(Path::new("/tmp/repo")).unwrap();
    let mut s = slot(&repo.id);
    store.insert_slot(&mut s).unwrap();
    let wf = Workflow {
        id: new_id(),
        slot_id: s.id.clone(),
        origin: berth_protocol::Origin {
            kind: berth_protocol::OriginKind::Thought,
            r#ref: String::new(),
            title: "x".into(),
            body: String::new(),
            custom_prompt: None,
            url: None,
        },
        state: WorkflowState::Idle,
        claude_session_id: None,
        section_no: 1,
        total_cost_usd: 0.0,
        last_error: None,
        created_at: now_ms(),
        closed_at: None,
    };
    store.insert_workflow(&wf).unwrap();
    let round = store.insert_round(&wf.id, 1, now_ms() - 60_000).unwrap();
    store
        .end_round(
            &round.id,
            berth_protocol::TerminalKind::Reply,
            0,
            0.0,
            1,
            now_ms() - 50_000,
        )
        .unwrap();
    let status = |store: &Store| store.workflow_facts(&wf.id).unwrap().status;
    assert_eq!(status(&store), None);

    store
        .set_workflow_status(&wf.id, Some(ManualStatus::Todo))
        .unwrap();
    assert_eq!(status(&store), Some(ManualStatus::Todo));
    store
        .set_workflow_status(&wf.id, Some(ManualStatus::Done))
        .unwrap();
    assert_eq!(status(&store), Some(ManualStatus::Done));
    store
        .set_workflow_status(&wf.id, Some(ManualStatus::Archived))
        .unwrap();
    assert_eq!(status(&store), Some(ManualStatus::Archived));

    // The agent works again: what its state says counts once more.
    store.insert_round(&wf.id, 1, now_ms() + 1_000).unwrap();
    assert_eq!(status(&store), None);

    store.set_workflow_status(&wf.id, None).unwrap();
    assert_eq!(status(&store), None);
}

#[test]
fn a_busy_or_failed_slot_takes_no_status_by_hand() {
    use berth_core::{ManualStatus, WorkflowState};
    let locked = |slot, wf| ManualStatus::locked(slot, wf).is_some();
    assert!(locked(SlotState::Running, WorkflowState::Running));
    assert!(locked(SlotState::Running, WorkflowState::Starting));
    assert!(locked(SlotState::Creating, WorkflowState::Starting));
    assert!(locked(SlotState::Failed, WorkflowState::Idle));
    assert!(!locked(SlotState::Running, WorkflowState::Idle));
    assert!(!locked(SlotState::Running, WorkflowState::AwaitingAnswer));
    assert!(!locked(SlotState::Ready, WorkflowState::Closed));
}

#[test]
fn a_file_is_viewed_by_the_change_read_until_unmarked() {
    use berth_core::{ViewedFile, Workflow, WorkflowState};
    let store = Store::open_in_memory().unwrap();
    let repo = store.upsert_repo(Path::new("/tmp/repo")).unwrap();
    let mut s = slot(&repo.id);
    store.insert_slot(&mut s).unwrap();
    let wf = Workflow {
        id: new_id(),
        slot_id: s.id.clone(),
        origin: berth_protocol::Origin {
            kind: berth_protocol::OriginKind::Thought,
            r#ref: String::new(),
            title: "x".into(),
            body: String::new(),
            custom_prompt: None,
            url: None,
        },
        state: WorkflowState::Idle,
        claude_session_id: None,
        section_no: 1,
        total_cost_usd: 0.0,
        last_error: None,
        created_at: now_ms(),
        closed_at: None,
    };
    store.insert_workflow(&wf).unwrap();
    let viewed = |store: &Store| store.viewed_files(&wf.id).unwrap();
    assert!(viewed(&store).is_empty());

    store.set_viewed(&wf.id, "src/a.rs", "1..2", true).unwrap();
    // Marking it again keeps one row.
    store.set_viewed(&wf.id, "src/a.rs", "1..2", true).unwrap();
    // The agent changed it after: the new change is viewed too, separately.
    store.set_viewed(&wf.id, "src/a.rs", "1..3", true).unwrap();
    assert_eq!(
        viewed(&store),
        [
            ViewedFile {
                path: "src/a.rs".into(),
                change: "1..2".into()
            },
            ViewedFile {
                path: "src/a.rs".into(),
                change: "1..3".into()
            }
        ]
    );
    store.set_viewed(&wf.id, "src/a.rs", "1..2", false).unwrap();
    assert_eq!(viewed(&store).len(), 1);
    assert_eq!(viewed(&store)[0].change, "1..3");
}

#[test]
fn a_workflow_knows_where_it_came_from_and_whom_it_messaged() {
    use berth_core::{MessagePeer, Workflow, WorkflowState};
    use berth_protocol::{
        FollowUp, MessageDirection, MessageStatus, NodePayload, Origin, OriginKind, Outcome,
        OutcomeKind, SlotMessage,
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("berth.db");
    let workflow = |slot_id: &str, kind, r#ref: &str| Workflow {
        id: new_id(),
        slot_id: slot_id.to_owned(),
        origin: Origin {
            kind,
            r#ref: r#ref.to_owned(),
            title: "x".into(),
            body: String::new(),
            custom_prompt: None,
            url: None,
        },
        state: WorkflowState::Idle,
        claude_session_id: None,
        section_no: 1,
        total_cost_usd: 0.0,
        last_error: None,
        created_at: now_ms(),
        closed_at: None,
    };
    let message = |direction, peer: &Workflow| {
        NodePayload::Message(SlotMessage {
            message_id: new_id(),
            direction,
            peer_slot_id: peer.slot_id.clone(),
            peer_workflow_id: peer.id.clone(),
            peer_name: "x".into(),
            text: "hi".into(),
            hop: 1,
            status: MessageStatus::Woke,
            held_because: None,
            settled_at: None,
        })
    };
    let (a, b, c) = {
        let store = Store::open(&path).unwrap();
        let repo = store.upsert_repo(Path::new("/tmp/repo")).unwrap();
        let mut ws = Vec::new();
        for (kind, r#ref) in [
            (OriginKind::Thought, ""),
            (OriginKind::FollowUp, "fu-1"),
            (OriginKind::Thought, ""),
        ] {
            let mut s = slot(&repo.id);
            store.insert_slot(&mut s).unwrap();
            let w = workflow(&s.id, kind, r#ref);
            store.insert_workflow(&w).unwrap();
            ws.push(w);
        }
        let [a, b, c] = <[Workflow; 3]>::try_from(ws).unwrap();
        store
            .insert_node(
                &a.id,
                None,
                None,
                NodePayload::Outcome(Outcome {
                    kind: OutcomeKind::Commit,
                    r#ref: "abc".into(),
                    summary: String::new(),
                    title: None,
                    follow_ups: vec![FollowUp {
                        id: "fu-1".into(),
                        title: "x".into(),
                        description: String::new(),
                    }],
                }),
                None,
            )
            .unwrap();
        for (from, to) in [(&b, &c), (&c, &b), (&b, &c)] {
            store
                .insert_node(
                    &from.id,
                    None,
                    None,
                    message(MessageDirection::Sent, to),
                    None,
                )
                .unwrap();
            store
                .insert_node(
                    &to.id,
                    None,
                    None,
                    message(MessageDirection::Received, from),
                    None,
                )
                .unwrap();
        }
        (a, b, c)
    };
    // As a database from before 0.3.9 has it: the follow-up was started, but
    // where from wasn't kept.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("ALTER TABLE workflows DROP COLUMN from_workflow_id")
            .unwrap();
        conn.pragma_update(None, "user_version", 12).unwrap();
    }
    let store = Store::open(&path).unwrap();
    let facts = |w: &Workflow| store.workflow_facts(&w.id).unwrap();
    assert_eq!(facts(&b).from_workflow.as_deref(), Some(a.id.as_str()));
    assert_eq!(facts(&a).from_workflow, None);
    assert_eq!(facts(&c).from_workflow, None);

    store.set_workflow_from(&c.id, &a.id).unwrap();
    assert_eq!(facts(&c).from_workflow.as_deref(), Some(a.id.as_str()));
    assert_eq!(
        facts(&b).peers,
        [MessagePeer {
            workflow_id: c.id.clone(),
            sent: 2,
            received: 1
        }]
    );
    assert_eq!(facts(&c).peers[0].workflow_id, b.id);
    assert!(facts(&a).peers.is_empty());
}
