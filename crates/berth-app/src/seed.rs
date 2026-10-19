//! Devtools fixture: the design's scenario (`BERTH_DEV_SEED=<scenario>`), so
//! screenshots line up with the artboards. A demo repo with three slots as
//! real git worktrees; the flow is written straight into the store.
//!
//! Scenarios: `question` (default), `working`, `review`, `comments`, `done`,
//! `empty` (a repo without workflows), `welcome` (only recent repositories).

use std::path::{Path, PathBuf};
use std::process::Command as Proc;

use anyhow::{Context as _, Result, anyhow};
use berth_core::{
    Branch, BranchState, NewReviewComment, Slot, SlotState, SnapshotKind, Store, Workflow,
    WorkflowState, ids::new_id, paths,
};
use berth_protocol::{
    BranchStatus, CollectResults, Decision, DiffPreset, DiffSide, FollowUp, NodePayload, Origin,
    OriginKind, Outcome, OutcomeKind, Prompt, Question, ReviewComment, SubagentSpawn, Summary,
};
use chrono::{Local, TimeZone as _};

/// Seed `scenario` into an empty database; returns the repo to open (none
/// for `welcome`).
pub fn seed(scenario: &str) -> Result<Option<PathBuf>> {
    let store = Store::open(&paths::db_path())?;
    let repo_dir = paths::app_support_dir().join("demo");
    if repo_dir.exists() {
        return Ok((scenario != "welcome").then_some(repo_dir));
    }
    make_repo(&repo_dir)?;
    let repo = store.upsert_repo(&repo_dir)?;
    if scenario == "welcome" {
        let other = paths::app_support_dir().join("berth");
        make_repo(&other)?;
        store.upsert_repo(&other)?;
        store.set_setting(
            "recent_repos",
            &serde_json::to_string(&[&repo_dir, &other])?,
        )?;
        return Ok(None);
    }
    if scenario == "empty" {
        return Ok(Some(repo_dir));
    }
    let at = |h: u32, m: u32, s: u32| -> i64 {
        let today = Local::now().date_naive();
        today
            .and_hms_opt(h, m, s)
            .and_then(|d| Local.from_local_datetime(&d).single())
            .map_or(0, |d| d.timestamp_millis())
    };
    let s = Seeder { store: &store };

    // ---- slot A: "Add a greeting function" ----
    let a = s.slot(
        &repo.id,
        &repo_dir,
        "add-a-greeting-function",
        "main",
        at(11, 50, 0),
    )?;
    let base_tree = git_out(&repo_dir, &["rev-parse", "main^{tree}"])?;
    write_greeting(&s.checkout(&a))?;
    let done = scenario == "done";
    let wa = s.workflow(
        &a,
        "Add a greeting function",
        "Add a `greet(name)` function to main.rs and call it from `main`.",
        at(11, 51, 0),
    )?;
    s.store
        .insert_snapshot(&wa.id, 1, SnapshotKind::Start, &base_tree)?;
    for (no, title) in [(1u32, "Plan the change"), (2, "Pick the greeting")] {
        let r = s.round(&wa.id, no, at(12, 4 + no * 3, 0))?;
        s.node(
            &wa.id,
            Some(&r),
            None,
            NodePayload::Summary(Summary {
                done: vec![format!("{title}.")],
                remaining: vec![],
                follow_ups: vec![],
                notes: None,
            }),
            at(12, 5 + no * 3, 0),
        )?;
        s.store
            .insert_snapshot(&wa.id, no, SnapshotKind::Summary, &base_tree)?;
        s.store
            .insert_snapshot(&wa.id, no + 1, SnapshotKind::Start, &base_tree)?;
    }
    let r3 = s.round(&wa.id, 3, at(12, 10, 0))?;
    let sent_comment = ReviewComment {
        preset: DiffPreset::Section,
        path: "main.rs".into(),
        line: 2,
        side: DiffSide::New,
        body: "Also add a one-line doc comment above greet.".into(),
    };
    let p3 = s.node(
        &wa.id,
        Some(&r3),
        None,
        NodePayload::Prompt(Prompt {
            text: "Use \"Berth\" as the name and don't compile anything yet.".into(),
            review_comments: if matches!(scenario, "comments" | "done") {
                vec![sent_comment]
            } else {
                vec![]
            },
            attachments: vec![],
        }),
        at(12, 10, 0),
    )?;
    s.node(&wa.id, Some(&r3), None, NodePayload::Decision(Decision {
        title: "Keep greet private; call it from main".into(),
        rationale: "Nothing else in the crate needs it, and a public fn in a binary crate only widens the surface a later library split would have to keep.".into(),
        affects: vec!["main.rs".into()],
        reversible: true,
    }), at(12, 14, 0))?;
    s.node(&wa.id, Some(&r3), None, NodePayload::Summary(Summary {
        done: vec![
            "You picked \"Berth\", so `main` now calls `greet(\"Berth\")`. The program prints \"Hello, Berth!\".".into(),
            "Nothing has been compiled or run yet, in line with your earlier request. Compile and run it before merging.".into(),
            "The change isn't committed and there's no PR yet.".into(),
        ],
        remaining: vec![],
        follow_ups: vec![],
        notes: None,
    }), at(12, 15, 0))?;
    let tree3 = snapshot_tree(&s.checkout(&a))?;
    s.store
        .insert_snapshot(&wa.id, 3, SnapshotKind::Summary, &tree3)?;
    s.store
        .insert_snapshot(&wa.id, 4, SnapshotKind::Start, &tree3)?;
    s.store.set_section_no(&wa.id, 4)?;
    s.store.set_workflow_state(&wa.id, WorkflowState::Idle)?;
    let mut pending_comment: Option<String> = None;
    if matches!(scenario, "comments" | "done") {
        let sent = s.store.insert_review_comment(&NewReviewComment {
            workflow_id: wa.id.clone(),
            from_snapshot: base_tree.clone(),
            to_snapshot: tree3.clone(),
            preset: DiffPreset::Section,
            path: "main.rs".into(),
            side: DiffSide::New,
            line: 2,
            body: "Also add a one-line doc comment above greet.".into(),
        })?;
        s.store.mark_comments_sent(&[sent.id], &p3)?;
        let pending = s.store.insert_review_comment(&NewReviewComment {
            workflow_id: wa.id.clone(),
            from_snapshot: base_tree.clone(),
            to_snapshot: tree3.clone(),
            preset: DiffPreset::Section,
            path: "main.rs".into(),
            side: DiffSide::New,
            line: 7,
            body: "Prefer a named constant for the greeting text.".into(),
        })?;
        pending_comment = Some(pending.id);
    }
    if done {
        let r4 = s.round(&wa.id, 4, at(12, 16, 0))?;
        let comment = ReviewComment {
            preset: DiffPreset::Section,
            path: "main.rs".into(),
            line: 7,
            side: DiffSide::New,
            body: "Prefer a named constant for the greeting text.".into(),
        };
        let p4 = s.node(&wa.id, Some(&r4), None, NodePayload::Prompt(Prompt {
            text: "Address the review comment, then commit everything on this branch and finish the workflow with record_outcome kind=commit. Include exactly one follow-up suggesting a unit test for greet.".into(),
            review_comments: vec![comment],
            attachments: vec![],
        }), at(12, 16, 0))?;
        if let Some(id) = pending_comment.take() {
            s.store.mark_comments_sent(&[id], &p4)?;
        }
        s.node(&wa.id, Some(&r4), None, NodePayload::Decision(Decision {
            title: "Introduce a `GREETING` constant".into(),
            rationale: "Answers your comment on `main.rs:7`: the greeting text moves to a `const` so the test and `main` share one source of truth.".into(),
            affects: vec!["main.rs".into()],
            reversible: true,
        }), at(12, 19, 0))?;
        s.node(&wa.id, Some(&r4), None, NodePayload::Outcome(Outcome {
            kind: OutcomeKind::Commit,
            r#ref: "ae1d438".into(),
            summary: "Added `greet(name: &str) -> String` to main.rs with a one-line doc comment, as the review asked. `main` now prints `greet(\"Berth\")`. All of this is committed on the branch; it has not been compiled or run, as you asked.".into(),
            follow_ups: vec![FollowUp {
                id: "fu-test".into(),
                title: "Add a unit test for greet".into(),
                description: "Add a `#[cfg(test)]` module that checks greet(\"Berth\") returns \"Hello, Berth!\", so changes to the greeting format get caught.".into(),
            }],
        }), at(12, 22, 0))?;
        s.store.set_workflow_state(&wa.id, WorkflowState::Closed)?;
    }

    // ---- slot B: "Add a unit test for greet" ----
    let b = s.slot(
        &repo.id,
        &repo_dir,
        "add-a-unit-test-for-greet",
        "main",
        at(12, 0, 0),
    )?;
    let origin_body = "Add a `#[cfg(test)]` module to main.rs that checks `greet(\"Berth\")` returns \"Hello, Berth!\", so changes to the greeting format get caught.";
    let wb = if done {
        s.workflow_from(
            &b,
            "Add a unit test for greet",
            origin_body,
            at(12, 1, 0),
            OriginKind::FollowUp,
            "fu-test",
        )?
    } else {
        s.workflow(&b, "Add a unit test for greet", origin_body, at(12, 1, 0))?
    };
    let rb1 = s.round(&wb.id, 1, at(12, 1, 10))?;
    s.node(&wb.id, Some(&rb1), None, NodePayload::Decision(Decision {
        title: "Stack this branch on the greeting-function branch".into(),
        rationale: "`greet` only exists on `berth/add-a-greeting-function`, which hasn't merged into main yet, so the test needs that commit underneath it to compile.".into(),
        affects: vec!["main.rs".into()],
        reversible: true,
    }), at(12, 2, 0))?;
    let rb2 = s.round(&wb.id, 1, at(12, 2, 30))?;
    s.node(&wb.id, Some(&rb2), None, NodePayload::Prompt(Prompt {
        text: "Use the Agent tool to run two subagents in parallel: one reads main.rs and records one decision about where a unit test for greet should live; the other checks whether Cargo.toml exists and records one decision about how tests would be run. When both return, ask me whether the test should be an inline `#[cfg(test)]` module or a tests/ directory.".into(),
        review_comments: vec![],
        attachments: vec![],
    }), at(12, 2, 40))?;
    if scenario == "working" {
        s.store.set_workflow_state(&wb.id, WorkflowState::Running)?;
    } else {
        let lanes = [
            (
                "Decide where the greet test lives",
                "Use an inline `#[cfg(test)]` module",
                "`greet` is private and main.rs is a binary with no library, so only a module inside main.rs can reach it.",
                18_000u64,
                3u32,
                "An inline `#[cfg(test)]` module is the only place that can call the private `greet` without a library crate.",
            ),
            (
                "Decide how tests would run",
                "Run tests with `cargo test`",
                "There is no `Cargo.toml` at the repo root, so the crate uses the default `[package]`; the built-in harness runs any `#[test]`.",
                34_000,
                4,
                "The root holds only `.git` and `main.rs`, which currently has an uncommitted change adding a `#[cfg(test)]` module containing `greet_formats_name`, which asserts greet(\"Berth\") == \"Hello, Berth!\". Built that way, the inline test passes.",
            ),
        ];
        let group = "msg-group-1".to_owned();
        let spawn_at = at(12, 3, 0);
        let mut ids = Vec::new();
        for (desc, _, _, _, _, _) in &lanes {
            let id = new_id();
            s.store.insert_branch(&Branch {
                id: id.clone(),
                workflow_id: wb.id.clone(),
                spawn_round_id: Some(rb2.clone()),
                collect_round_id: None,
                parent_branch_id: None,
                parent_tool_use_id: format!("tu-{id}"),
                agent_name: "general-purpose".into(),
                description: (*desc).into(),
                parallel_group: Some(group.clone()),
                status: BranchState::Running,
                spawned_at: spawn_at,
                collected_at: None,
                cost_usd: None,
                spawn_tree: None,
                collect_tree: None,
                result: None,
            })?;
            s.node(
                &wb.id,
                Some(&rb2),
                None,
                NodePayload::SubagentSpawn(SubagentSpawn {
                    branch_id: id.clone(),
                    agent_name: "general-purpose".into(),
                    description: (*desc).into(),
                    prompt: (*desc).into(),
                    parallel_group: Some(group.clone()),
                }),
                spawn_at,
            )?;
            ids.push(id);
        }
        for ((_, title, why, _, _, _), id) in lanes.iter().zip(&ids) {
            s.node(
                &wb.id,
                Some(&rb2),
                Some(id),
                NodePayload::Decision(Decision {
                    title: (*title).into(),
                    rationale: (*why).into(),
                    affects: vec![],
                    reversible: true,
                }),
                spawn_at + 10_000,
            )?;
        }
        for ((_, _, _, ms, calls, text), id) in lanes.iter().zip(&ids) {
            let result = CollectResults {
                branch_id: id.clone(),
                result_text: (*text).into(),
                duration_ms: *ms,
                cost_usd: None,
                tool_calls: *calls,
                touched_files: vec![],
                status: BranchStatus::Ok,
            };
            let when = spawn_at + i64::try_from(*ms).unwrap_or(0);
            s.store.collect_branch(id, &result, Some(&rb2), when)?;
            s.node(
                &wb.id,
                Some(&rb2),
                None,
                NodePayload::CollectResults(result),
                when,
            )?;
        }
        s.node(&wb.id, Some(&rb2), None, NodePayload::Question(Question {
            question_id: "q-test-location".into(),
            question: "Both checks are done, and each recorded one decision. The first recommends an inline `#[cfg(test)]` module, because `greet` is private and main.rs is a binary with no library, so a tests/ directory can't reach it unless it moves into a library crate. The second found no Cargo.toml and recommends `rustc --test main.rs`; built that way, the inline test passes (1 passed, 0 failed) and nothing is committed yet. **Which should it be?**".into(),
            options: vec![
                "Inline `#[cfg(test)]` module — Keeps the private function reachable; what the first subagent recommends".into(),
                "tests/ directory — Needs a library crate first; the agent would add one".into(),
            ],
            allow_free_text: true,
            answer: None,
            answered_at: None,
        }), berth_core::ids::now_ms() - 40_000)?;
        s.store
            .set_workflow_state(&wb.id, WorkflowState::AwaitingAnswer)?;
    }

    // ---- slot C: "Migrate config to TOML" ----
    let c = s.slot(
        &repo.id,
        &repo_dir,
        "migrate-config-to-toml",
        "main",
        at(12, 5, 0),
    )?;
    let wc = s.workflow(
        &c,
        "Migrate config to TOML",
        "Move the app's settings from `config.json` to `config.toml`.",
        at(12, 5, 0),
    )?;
    s.round(&wc.id, 1, at(12, 5, 10))?;
    s.store.set_workflow_state(&wc.id, WorkflowState::Running)?;
    Ok(Some(repo_dir))
}

struct Seeder<'a> {
    store: &'a Store,
}

impl Seeder<'_> {
    fn checkout(&self, slot: &Slot) -> PathBuf {
        PathBuf::from(slot.checkout_path.clone().unwrap_or_default())
    }

    fn slot(
        &self,
        repo_id: &str,
        repo: &Path,
        name: &str,
        base: &str,
        created: i64,
    ) -> Result<Slot> {
        let id = new_id();
        let checkout = paths::slot_state_dir(&id).join("checkout");
        std::fs::create_dir_all(checkout.parent().context("slot dir")?)?;
        let branch = format!("berth/{name}");
        git(
            repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "--no-track",
                "-b",
                &branch,
                &checkout.to_string_lossy(),
                base,
            ],
        )?;
        let slot = Slot {
            id,
            repo_id: repo_id.to_owned(),
            slot_type: "worktree".into(),
            name: name.to_owned(),
            custom_name: None,
            model: None,
            effort: None,
            state: SlotState::Running,
            state_detail: None,
            checkout_path: Some(checkout.to_string_lossy().into_owned()),
            placement: "host".into(),
            branch,
            base_ref: base.to_owned(),
            created_at: created,
        };
        self.store.insert_slot(&slot)?;
        Ok(slot)
    }

    fn workflow(&self, slot: &Slot, title: &str, body: &str, at: i64) -> Result<Workflow> {
        self.workflow_from(slot, title, body, at, OriginKind::Thought, "")
    }

    fn workflow_from(
        &self,
        slot: &Slot,
        title: &str,
        body: &str,
        at: i64,
        kind: OriginKind,
        r: &str,
    ) -> Result<Workflow> {
        let wf = Workflow {
            id: new_id(),
            slot_id: slot.id.clone(),
            origin: Origin {
                kind,
                r#ref: r.to_owned(),
                title: title.to_owned(),
                body: body.to_owned(),
                custom_prompt: None,
                url: None,
            },
            state: WorkflowState::Idle,
            claude_session_id: None,
            section_no: 1,
            total_cost_usd: 0.0,
            last_error: None,
            created_at: at,
            closed_at: None,
        };
        self.store.insert_workflow(&wf)?;
        self.node(
            &wf.id,
            None,
            None,
            NodePayload::Origin(wf.origin.clone()),
            at,
        )?;
        Ok(wf)
    }

    fn round(&self, wf: &str, section: u32, at: i64) -> Result<String> {
        Ok(self.store.insert_round(wf, section, at)?.id)
    }

    fn node(
        &self,
        wf: &str,
        round: Option<&str>,
        branch: Option<&str>,
        payload: NodePayload,
        at: i64,
    ) -> Result<String> {
        // A transcript line behind every node, so "Details" has something to show.
        let raw = self.store.insert_raw_event(&berth_core::NewRawEvent {
            workflow_id: wf,
            round_id: round,
            branch_id: branch,
            seq: 0,
            kind: "assistant",
            json: r#"{"type":"assistant","message":{"content":[{"type":"text","text":"(seeded for the design scenario)"}]}}"#,
        })?;
        let n = self
            .store
            .insert_node(wf, round, branch, payload, Some((raw, raw)))?;
        self.store.set_node_created_at(&n.id, at)?;
        Ok(n.id)
    }
}

fn make_repo(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    git(dir, &["init", "--quiet", "-b", "main"])?;
    std::fs::write(
        dir.join("main.rs"),
        "fn main() {\n    println!(\"hello\");\n}\n",
    )?;
    std::fs::write(
        dir.join("README.md"),
        "# demo\n\nA tiny program Berth's agent works on.\n",
    )?;
    git(dir, &["add", "-A"])?;
    git(
        dir,
        &[
            "-c",
            "user.name=Berth",
            "-c",
            "user.email=berth@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "init",
        ],
    )?;
    Ok(())
}

fn write_greeting(checkout: &Path) -> Result<()> {
    std::fs::write(
        checkout.join("main.rs"),
        "/// Returns a greeting of the form \"Hello, <name>!\".\nfn greet(name: &str) -> String {\n    format!(\"Hello, {name}!\")\n}\n\nfn main() {\n    println!(\"{}\", greet(\"Berth\"));\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn greet_formats_name() {\n        assert_eq!(greet(\"Berth\"), \"Hello, Berth!\");\n    }\n}\n",
    )?;
    std::fs::write(
        checkout.join("README.md"),
        "# demo\n\nA tiny program Berth's agent works on.\n\nRun it to see the greeting:\n`rustc main.rs && ./main`\n",
    )?;
    Ok(())
}

fn snapshot_tree(checkout: &Path) -> Result<String> {
    let index = checkout
        .parent()
        .map_or_else(|| checkout.join("../seed-index"), |d| d.join("seed-index"));
    let idx = index.to_string_lossy().into_owned();
    let run = |args: &[&str]| -> Result<String> {
        let out = Proc::new("git")
            .args(args)
            .current_dir(checkout)
            .env("GIT_INDEX_FILE", &idx)
            .output()?;
        if !out.status.success() {
            return Err(anyhow!(
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    run(&["read-tree", "HEAD"])?;
    run(&["add", "-A"])?;
    let tree = run(&["write-tree"])?;
    let _ = std::fs::remove_file(&index);
    Ok(tree)
}

fn git(dir: &Path, args: &[&str]) -> Result<()> {
    git_out(dir, args).map(|_| ())
}

fn git_out(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Proc::new("git").args(args).current_dir(dir).output()?;
    if !out.status.success() {
        return Err(anyhow!(
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}
