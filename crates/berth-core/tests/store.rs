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
    }
}

#[test]
fn a_slot_can_be_renamed_and_unnamed() {
    let store = Store::open_in_memory().unwrap();
    let repo = store.upsert_repo(Path::new("/tmp/repo")).unwrap();
    let s = slot(&repo.id);
    store.insert_slot(&s).unwrap();
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
