//! Devtools fixture: the design's scenario (`BERTH_DEV_SEED=<scenario>`), so
//! screenshots line up with the artboards. A demo repo with three slots as
//! real git worktrees; the flow is written straight into the store.
//!
//! Scenarios: `question` (default), `working`, `review`, `comments`, `done`,
//! `artifact` (`done`, the first workflow ending in a Claude artifact),
//! `findings` (the third slot waits on decisions about a code review),
//! `usage-api` (`question`, signed in with an API key: no plan limits),
//! `commands` (the third slot ran `/context` and `/compact`), `empty`
//! (a repo without workflows), `welcome` (only recent repositories).

use std::path::{Path, PathBuf};
use std::process::Command as Proc;

use anyhow::{Context as _, Result, anyhow};
use berth_core::{
    Branch, BranchState, NewReviewComment, Slot, SlotState, SnapshotKind, Store, Workflow,
    WorkflowState, ids::new_id, paths,
};
use berth_protocol::{
    BranchStatus, CollectResults, Decision, DiffPreset, DiffSide, Finding, FindingAction,
    FindingLevel, FollowUp, NodePayload, Origin, OriginKind, Outcome, OutcomeKind, Prompt,
    Question, Review, ReviewComment, SubagentSpawn, Summary,
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
    // `artifact` is `done` with the workflow ending in a Claude artifact.
    let artifact = scenario == "artifact";
    let done = scenario == "done" || artifact;
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
            review_comments: if matches!(scenario, "comments" | "done" | "artifact") {
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
    let span = s.transcript(&wa.id, &r3, &s.checkout(&a), at(12, 10, 0))?;
    s.node_span(&wa.id, Some(&r3), None, NodePayload::Summary(Summary {
        done: vec![
            "You picked \"Berth\", so `main` now calls `greet(\"Berth\")`. The program prints \"Hello, Berth!\".".into(),
            "Nothing has been compiled or run yet, in line with your earlier request. Compile and run it before merging.".into(),
            "The change isn't committed and there's no PR yet.".into(),
        ],
        remaining: vec![],
        follow_ups: vec![],
        notes: None,
    }), at(12, 15, 0), span)?;
    let tree3 = snapshot_tree(&s.checkout(&a))?;
    s.store
        .insert_snapshot(&wa.id, 3, SnapshotKind::Summary, &tree3)?;
    s.store
        .insert_snapshot(&wa.id, 4, SnapshotKind::Start, &tree3)?;
    s.store.set_section_no(&wa.id, 4)?;
    s.store.set_workflow_state(&wa.id, WorkflowState::Idle)?;
    let mut pending_comment: Option<String> = None;
    if matches!(scenario, "comments" | "done" | "artifact") {
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
            text: if artifact {
                "Address the review comment, then publish a one-page tour of greet as a Claude artifact and finish the workflow with it. Include exactly one follow-up suggesting a unit test for greet."
            } else {
                "Address the review comment, then commit everything on this branch and finish the workflow with record_outcome kind=commit. Include exactly one follow-up suggesting a unit test for greet."
            }.into(),
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
        let (kind, r#ref, summary, title) = if artifact {
            (
                OutcomeKind::ClaudeArtifact,
                "https://claude.ai/code/artifact/5f3e9c1a-7b2d-4e8f-a6c0-91d4b3e2f871".into(),
                "Published a one-page tour of `greet`: what it returns, where `main` calls it, and the `GREETING` constant your comment asked for. Nothing is committed; the page is the deliverable.".into(),
                Some("Greeting function tour".to_owned()),
            )
        } else {
            (
                OutcomeKind::Commit,
                "ae1d438".into(),
                "Added `greet(name: &str) -> String` to main.rs with a one-line doc comment, as the review asked. `main` now prints `greet(\"Berth\")`. All of this is committed on the branch; it has not been compiled or run, as you asked.".into(),
                None,
            )
        };
        s.node(&wa.id, Some(&r4), None, NodePayload::Outcome(Outcome {
            kind,
            r#ref,
            summary,
            title,
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
    let rc = s.round(&wc.id, 1, at(12, 5, 10))?;
    if scenario == "findings" {
        seed_findings(&s, &s.checkout(&c), &wc.id, &rc, at(12, 9, 0))?;
    } else if scenario == "commands" {
        seed_command_rounds(&s, &wc.id, &rc, at(12, 6, 0))?;
    } else {
        s.store.set_workflow_state(&wc.id, WorkflowState::Running)?;
    }
    seed_usage(&store, [&wa.id, &wb.id, &wc.id], scenario)?;
    seed_command_list(&store, &repo.id)?;
    store.set_setting(
        "account",
        &serde_json::to_string(&berth_protocol::Account {
            email: Some("dev@example.com".into()),
            organization: None,
            plan: Some("Claude Max".into()),
            provider: Some("firstParty".into()),
        })?,
    )?;
    Ok(Some(repo_dir))
}

/// The `commands` scenario: in the third slot, the user ran `/context` and
/// then `/compact`; it waits for the next prompt.
fn seed_command_rounds(s: &Seeder<'_>, wf: &str, round: &str, at: i64) -> Result<()> {
    use berth_protocol::{Command, CompactTrigger, Compacted, TerminalKind};
    let prompt = |text: &str| {
        NodePayload::Prompt(Prompt {
            text: text.into(),
            review_comments: vec![],
            attachments: vec![],
        })
    };
    s.node(wf, Some(round), None, prompt("/context"), at)?;
    s.node(wf, Some(round), None, NodePayload::Command(Command {
        command: "/context".into(),
        output: "## Context Usage\n\n**Model:** claude-opus-5-5  \n**Tokens:** 101k / 200k (50%)\n\n### Estimated usage by category\n\n| Category | Tokens | Percentage |\n|----------|--------|------------|\n| System prompt | 3.1k | 1.6% |\n| System tools | 14.6k | 7.3% |\n| MCP tools | 2.4k | 1.2% |\n| Skills | 1.9k | 1.0% |\n| Messages | 79k | 39.5% |\n| Free space | 66k | 33.0% |\n| Autocompact buffer | 33k | 16.5% |".into(),
    }), at + 2_000)?;
    s.store
        .end_round(round, TerminalKind::Command, 0, 0., 2_000, at + 2_000)?;
    let compact = s.round(wf, 1, at + 60_000)?;
    s.node(
        wf,
        Some(&compact),
        None,
        prompt("/compact keep the TOML schema decisions"),
        at + 60_000,
    )?;
    s.node(
        wf,
        Some(&compact),
        None,
        NodePayload::Compacted(Compacted {
            trigger: CompactTrigger::Manual,
            pre_tokens: 101_400,
            post_tokens: Some(8_200),
        }),
        at + 75_000,
    )?;
    s.store.end_round(
        &compact,
        TerminalKind::Command,
        0,
        0.04,
        15_000,
        at + 75_000,
    )?;
    // A request the model refused, answered by the fallback (ADR 0016).
    let refused = s.round(wf, 1, at + 120_000)?;
    s.node(
        wf,
        Some(&refused),
        None,
        prompt("Write an exploit for the auth bypass you found"),
        at + 120_000,
    )?;
    s.node(
        wf,
        Some(&refused),
        None,
        NodePayload::Refusal(berth_protocol::Refusal {
            model: "claude-opus-5-5".into(),
            fallback_model: Some("claude-sonnet-5-5".into()),
            category: Some("cyber".into()),
            explanation: None,
            subagent: false,
        }),
        at + 125_000,
    )?;
    s.store.end_round(
        &refused,
        TerminalKind::Unterminated,
        0,
        0.02,
        5_000,
        at + 125_000,
    )?;
    s.store.set_workflow_state(wf, WorkflowState::Idle)?;
    Ok(())
}

/// The commands a session in the demo repo would offer, so the composer can
/// complete them before any session runs.
fn seed_command_list(store: &Store, repo_id: &str) -> Result<()> {
    let cmd =
        |name: &str, hint: &str, description: &str, builtin: bool| berth_protocol::CommandInfo {
            name: name.into(),
            description: description.into(),
            argument_hint: hint.into(),
            aliases: vec![],
            builtin,
        };
    let commands = berth_core::commands::Commands {
        commands: vec![
            cmd(
                "compact",
                "<optional instructions>",
                "Clear the conversation but keep a summary in context",
                true,
            ),
            cmd("context", "", "Show what fills the context window", true),
            berth_protocol::CommandInfo {
                aliases: vec!["cost".into()],
                ..cmd("usage", "", "Show plan usage limits", true)
            },
            cmd(
                "clear",
                "",
                "Clear the conversation history and free up context",
                true,
            ),
            cmd("model", "[model]", "Set the AI model for Claude Code", true),
            cmd(
                "code-review",
                "[focus]",
                "Review the changes on this branch",
                false,
            ),
            cmd(
                "security-review",
                "",
                "Complete a security review of the pending changes",
                true,
            ),
            cmd(
                "init",
                "",
                "Initialize a new CLAUDE.md file with codebase documentation",
                true,
            ),
            cmd(
                "anthropic-skills:pdf",
                "<file>",
                "Read, fill and create PDF documents",
                false,
            ),
            cmd("color", "<color>", "Set the prompt bar color", true),
        ],
        terminal: vec!["color".into(), "exit".into()],
    };
    store.set_setting(
        &format!("commands:{repo_id}"),
        &serde_json::to_string(&commands)?,
    )?;
    Ok(())
}

/// Context windows for the three workflows (a quarter, about half, and
/// nearly full) and a Max plan's limits; `usage-api` stands for an API-key
/// sign-in, where plan limits don't apply.
fn seed_usage(store: &Store, workflows: [&str; 3], scenario: &str) -> Result<()> {
    use berth_protocol::{ContextCategory, ContextKind, QuotaWindow, SessionQuota};
    let now = berth_core::ids::now_ms();
    let part = |name: &str, tokens: u64, kind| ContextCategory {
        name: name.into(),
        tokens,
        kind,
    };
    for (wf, messages) in workflows.into_iter().zip([28_000, 79_000, 141_000]) {
        let used = 3_100 + 14_600 + 2_400 + 1_900 + messages;
        let context = berth_core::ContextUsage {
            categories: vec![
                part("System prompt", 3_100, ContextKind::Used),
                part("System tools", 14_600, ContextKind::Used),
                part("MCP tools", 2_400, ContextKind::Used),
                part("Skills", 1_900, ContextKind::Used),
                part("Messages", messages, ContextKind::Used),
                part("MCP tools (deferred)", 9_800, ContextKind::Deferred),
                part("Free space", 167_000 - used, ContextKind::Free),
                part("Autocompact buffer", 33_000, ContextKind::Buffer),
            ],
            at: now - 4 * 60_000,
        };
        store.set_workflow_context(wf, &context)?;
    }
    let hour = 3_600_000;
    let window = |key: &str, label: Option<&str>, utilization: f64, resets_at: i64| QuotaWindow {
        key: key.into(),
        label: label.map(str::to_owned),
        utilization: Some(utilization),
        resets_at: Some(resets_at),
        status: None,
    };
    let report = if scenario == "usage-api" {
        SessionQuota {
            complete: true,
            available: false,
            plan: None,
            windows: vec![],
        }
    } else {
        SessionQuota {
            complete: true,
            available: true,
            plan: Some("max".into()),
            windows: vec![
                window("five_hour", None, 34., now + 2 * hour + 1_500_000),
                window("seven_day", None, 61., now + 3 * 24 * hour),
                window("model:Opus", Some("Opus"), 78., now + 3 * 24 * hour),
            ],
        }
    };
    let quota = berth_core::Quota::apply(None, &report, now - 60_000);
    store.set_setting("quota", &serde_json::to_string(&quota)?)?;
    Ok(())
}

/// The `findings` scenario: the agent migrated the config, reviewed its own
/// change and waits on the user's decision about five findings.
fn seed_findings(s: &Seeder<'_>, checkout: &Path, wf: &str, round: &str, at: i64) -> Result<()> {
    std::fs::write(
        checkout.join("config.toml"),
        "[server]\nport = 8080\ntimeout = 30\n\n[log]\nlevel = \"info\"\n",
    )?;
    std::fs::write(
        checkout.join("config.rs"),
        "pub fn load(path: &str) -> Config {\n    let text = std::fs::read_to_string(path).unwrap();\n    toml::from_str(&text).unwrap()\n}\n",
    )?;
    s.node(wf, Some(round), None, NodePayload::Decision(Decision {
        title: "Read `config.json` once more on first launch".into(),
        rationale: "Existing installs still have a JSON file; reading it once and writing `config.toml` keeps their settings without a separate migration step.".into(),
        affects: vec!["config.rs".into()],
        reversible: true,
    }), at)?;
    let finding = |level, title: &str, detail: &str, path: &str, line, suggested| Finding {
        level,
        title: title.into(),
        detail: detail.into(),
        path: Some(path.into()),
        line,
        suggested,
        choice: None,
        note: None,
    };
    s.node(wf, Some(round), None, NodePayload::Review(Review {
        review_id: "rv-toml".into(),
        summary: Some("I reviewed the TOML migration (2 files) before opening a PR. One real bug, one risk worth fixing, and three smaller points.".into()),
        findings: vec![
            finding(
                FindingLevel::High,
                "A missing config file panics at startup",
                "`load` calls `unwrap()` on `read_to_string`, so a first launch without `config.toml` (every new install) crashes before the window opens.\n\nFix: return the defaults when the file does not exist, and surface any other read error to the caller.",
                "config.rs",
                Some(2),
                FindingAction::Fix,
            ),
            finding(
                FindingLevel::Mid,
                "Malformed TOML is not reported",
                "A typo in `config.toml` makes `toml::from_str` fail, and the second `unwrap()` turns it into a panic with no line number.\n\nFix: map the error to one that names the file and the line, and show it in the settings window.",
                "config.rs",
                Some(3),
                FindingAction::Fix,
            ),
            finding(
                FindingLevel::Mid,
                "The JSON file is left behind",
                "After the one-time import, `config.json` stays on disk. Nothing reads it again, but a user editing it would see no effect.\n\nOptions: delete it after a successful import, rename it to `config.json.bak`, or leave it. This changes users' files, so it is your call.",
                "config.rs",
                None,
                FindingAction::Decide,
            ),
            finding(
                FindingLevel::Low,
                "`timeout` has no unit",
                "`timeout = 30` reads as seconds, but nothing says so. Renaming it to `timeout_secs` makes it self-describing; it is a breaking change for anyone who already wrote the key.",
                "config.toml",
                Some(3),
                FindingAction::Dismiss,
            ),
            finding(
                FindingLevel::Low,
                "Sections are not sorted",
                "`[log]` comes after `[server]`; most of the repo's TOML files sort sections alphabetically. Purely cosmetic.",
                "config.toml",
                Some(5),
                FindingAction::Dismiss,
            ),
        ],
        answered_at: None,
    }), berth_core::ids::now_ms() - 25_000)?;
    s.store
        .set_workflow_state(wf, WorkflowState::AwaitingAnswer)?;
    Ok(())
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
        let mut slot = Slot {
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
            number: 0,
        };
        self.store.insert_slot(&mut slot)?;
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
        // A transcript line behind every node, so "Details" has something to
        // show (with a uuid, as Claude Code's have, so a rewind finds its place).
        let json = serde_json::json!({
            "type": "assistant",
            "uuid": new_id(),
            "message": {"content": [{"type": "text", "text": "(seeded for the design scenario)"}]},
        })
        .to_string();
        let raw = self.store.insert_raw_event(&berth_core::NewRawEvent {
            workflow_id: wf,
            round_id: round,
            branch_id: branch,
            seq: 0,
            kind: "assistant",
            json: &json,
        })?;
        self.node_span(wf, round, branch, payload, at, (raw, raw))
    }

    fn node_span(
        &self,
        wf: &str,
        round: Option<&str>,
        branch: Option<&str>,
        payload: NodePayload,
        at: i64,
        span: (i64, i64),
    ) -> Result<String> {
        let n = self
            .store
            .insert_node(wf, round, branch, payload, Some(span))?;
        self.store.set_node_created_at(&n.id, at)?;
        Ok(n.id)
    }

    /// A round's worth of Claude Code's events, as the SDK sends them, so the
    /// transcript sheet has a real one to show: the prompt, thinking, text,
    /// tool calls with their results (one failing, one long) and the result.
    fn transcript(&self, wf: &str, round: &str, cwd: &Path, start: i64) -> Result<(i64, i64)> {
        use serde_json::json;
        let cwd = cwd.display().to_string();
        let file = format!("{cwd}/main.rs");
        let diff: String = std::iter::once(
            "diff --git a/main.rs b/main.rs\nindex 3b18e51..a9c4d2f 100644\n--- a/main.rs\n+++ b/main.rs\n@@ -1,5 +1,9 @@\n"
                .to_owned(),
        )
        .chain((1..=28).map(|n| format!(" // line {n} of the surrounding context\n")))
        .collect();
        let tool = |id: &str, name: &str, input: serde_json::Value| {
            json!({"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": id, "name": name, "input": input}
            ]}})
        };
        let result = |id: &str, text: &str, is_error: bool| {
            json!({"type": "user", "message": {"content": [
                {"type": "tool_result", "tool_use_id": id, "is_error": is_error,
                 "content": [{"type": "text", "text": text}]}
            ]}})
        };
        let events: Vec<(i64, &str, serde_json::Value)> = vec![
            (
                0,
                "system",
                json!({"type": "system", "subtype": "init", "model": "claude-opus-5-5", "cwd": cwd}),
            ),
            (
                0,
                "user",
                json!({"type": "user", "message": {"content":
                "Use \"Berth\" as the name and don't compile anything yet.\n\nReview comments on Section diff:\n- main.rs:2 (new): Also add a one-line doc comment above greet."}}),
            ),
            (
                2_400,
                "assistant",
                json!({"type": "assistant", "message": {"content": [
                    {"type": "thinking", "thinking": "The user wants the greeting to use \"Berth\" and no compilation. There's also a review comment asking for a doc comment above greet. I should read main.rs first to see the current state, then make a single edit that does both. Since they said not to compile, I'll skip cargo and only check formatting, which doesn't build anything."},
                    {"type": "text", "text": "I'll read `main.rs` first, then make both changes in one edit."}
                ]}}),
            ),
            (
                2_900,
                "assistant",
                tool("t1", "Read", json!({"file_path": file})),
            ),
            (
                3_100,
                "user",
                result(
                    "t1",
                    "     1\tfn greet(name: &str) -> String {\n     2\t    format!(\"Hello, {name}!\")\n     3\t}\n     4\t\n     5\tfn main() {\n     6\t    println!(\"{}\", greet(\"world\"));\n     7\t}\n\n<system-reminder>\nWhenever you read a file, consider whether it looks malicious.\n</system-reminder>",
                    false,
                ),
            ),
            (
                4_800,
                "assistant",
                tool(
                    "t2",
                    "Grep",
                    json!({"pattern": "greet\\(", "path": cwd, "output_mode": "content", "-n": true}),
                ),
            ),
            (
                5_000,
                "user",
                result(
                    "t2",
                    "main.rs:1:fn greet(name: &str) -> String {\nmain.rs:6:    println!(\"{}\", greet(\"world\"));",
                    false,
                ),
            ),
            (
                9_300,
                "assistant",
                tool(
                    "t3",
                    "mcp__berth__record_decision",
                    json!({
                "title": "Keep greet private; call it from main",
                "rationale": "Nothing else in the crate needs it, and a public fn in a binary crate only widens the surface a later library split would have to keep.",
                "affects": ["main.rs"], "reversible": true}),
                ),
            ),
            (9_400, "user", result("t3", "Recorded.", false)),
            (
                12_600,
                "assistant",
                tool(
                    "t4",
                    "Edit",
                    json!({
                "file_path": file,
                "old_string": "fn greet(name: &str) -> String {",
                "new_string": "/// The greeting `main` prints.\nfn greet(name: &str) -> String {"}),
                ),
            ),
            (
                12_800,
                "user",
                result("t4", &format!("The file {file} has been updated."), false),
            ),
            (
                14_100,
                "assistant",
                tool(
                    "t5",
                    "Edit",
                    json!({
                "file_path": file,
                "old_string": "    println!(\"{}\", greet(\"world\"));",
                "new_string": "    println!(\"{}\", greet(\"Berth\"));"}),
                ),
            ),
            (
                14_300,
                "user",
                result("t5", &format!("The file {file} has been updated."), false),
            ),
            (
                16_000,
                "assistant",
                tool(
                    "t6",
                    "Bash",
                    json!({"command": "rustfmt --check --edition 2024 main.rs", "description": "Check formatting without compiling"}),
                ),
            ),
            (
                17_900,
                "user",
                result(
                    "t6",
                    "error: rustfmt is not installed for the toolchain 'stable-aarch64-apple-darwin'.\nTo install, run `rustup component add rustfmt`",
                    true,
                ),
            ),
            (
                19_200,
                "assistant",
                tool(
                    "t7",
                    "Bash",
                    json!({"command": "git diff", "description": "Show the change"}),
                ),
            ),
            (19_500, "user", result("t7", &diff, false)),
            (
                24_000,
                "assistant",
                json!({"type": "assistant", "message": {"content": [
                    {"type": "text", "text": "Both changes are in:\n\n- `main` now calls `greet(\"Berth\")`, so the program prints **Hello, Berth!**\n- `greet` has a one-line doc comment, as your review comment asked.\n\nrustfmt isn't installed here, so formatting is unchecked, and nothing was compiled."}
                ]}}),
            ),
            (
                25_800,
                "assistant",
                tool(
                    "t8",
                    "mcp__berth__write_summary",
                    json!({
                "done": ["You picked \"Berth\", so `main` now calls `greet(\"Berth\")`.", "Added a doc comment above greet."],
                "remaining": [], "follow_ups": []}),
                ),
            ),
            (
                26_000,
                "user",
                result("t8", "Summary recorded. End your turn now.", false),
            ),
            (
                27_400,
                "result",
                json!({"type": "result", "subtype": "success", "is_error": false, "total_cost_usd": 0.0812, "duration_ms": 27_400, "num_turns": 9}),
            ),
        ];
        let mut span = None;
        for (seq, (offset, kind, event)) in events.into_iter().enumerate() {
            let json = event.to_string();
            let id = self.store.insert_raw_event(&berth_core::NewRawEvent {
                workflow_id: wf,
                round_id: Some(round),
                branch_id: None,
                seq: i64::try_from(seq).unwrap_or(0),
                kind,
                json: &json,
            })?;
            self.store.set_raw_event_created_at(id, start + offset)?;
            span = Some(span.map_or((id, id), |(first, _)| (first, id)));
        }
        span.ok_or_else(|| anyhow!("no events"))
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
