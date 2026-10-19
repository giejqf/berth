//! Text the app sends to the agent (AGENTS.md §5.5).

use std::fmt::Write as _;

use berth_protocol::{DiffPreset, FollowUp, Origin, OriginKind, Prompt, ReviewComment};

/// Serialize a Prompt as the next user turn, with review comments grouped by preset:
///
/// ```text
/// Review comments on <preset> diff:
/// - <path>:<line> (<old|new>): <comment>
/// ```
#[must_use]
pub fn serialize_prompt(prompt: &Prompt) -> String {
    let mut out = prompt.text.trim().to_owned();
    let mut presets: Vec<DiffPreset> = Vec::new();
    for c in &prompt.review_comments {
        if !presets.contains(&c.preset) {
            presets.push(c.preset);
        }
    }
    for preset in presets {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        let _ = write!(out, "Review comments on {} diff:", preset.as_str());
        for c in prompt.review_comments.iter().filter(|c| c.preset == preset) {
            let _ = write!(out, "\n- {}", comment_line(c));
        }
    }
    if !prompt.attachments.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str("Attached files:");
        for f in &prompt.attachments {
            let _ = write!(
                out,
                "\n- {} ({}, {}): {}",
                f.name,
                f.mime,
                crate::files::human_size(f.size_bytes),
                f.slot_path.as_deref().unwrap_or("not copied into the slot")
            );
        }
    }
    out
}

fn comment_line(c: &ReviewComment) -> String {
    format!(
        "{}:{} ({}): {}",
        c.path,
        c.line,
        c.side.as_str(),
        c.body.trim().replace('\n', " ")
    )
}

/// The first turn of a workflow, built from its Origin.
#[must_use]
pub fn origin_turn(origin: &Origin) -> String {
    let mut out = match origin.kind {
        OriginKind::GhIssue => format!(
            "Work on GitHub issue {}: {}",
            display_ref(&origin.r#ref),
            origin.title
        ),
        OriginKind::GhPr => format!(
            "Continue GitHub pull request {}: {}",
            display_ref(&origin.r#ref),
            origin.title
        ),
        OriginKind::Linear => format!("Work on Linear issue {}: {}", origin.r#ref, origin.title),
        OriginKind::Posthog => format!(
            "Investigate PostHog error {}: {}",
            origin.r#ref, origin.title
        ),
        OriginKind::FollowUp => format!("Follow-up: {}", origin.title),
        OriginKind::Thought => origin.title.clone(),
    };
    if !origin.body.trim().is_empty() {
        out.push_str("\n\n");
        out.push_str(origin.body.trim());
    }
    if let Some(extra) = origin
        .custom_prompt
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        out.push_str("\n\n");
        out.push_str(extra.trim());
    }
    out
}

fn display_ref(r: &str) -> String {
    if r.chars().all(|c| c.is_ascii_digit()) && !r.is_empty() {
        format!("#{r}")
    } else {
        r.to_owned()
    }
}

/// Context the app appends to the sidecar's system prompt.
#[must_use]
pub fn session_context(ctx: &SessionContext<'_>) -> String {
    let mut out = String::from("## Berth context\n");
    let _ = writeln!(out, "- Repository: {}", ctx.repo_name);
    let _ = writeln!(
        out,
        "- Slot: {} (type `{}`, agent placement `{}`)",
        ctx.slot_name, ctx.slot_type, ctx.placement
    );
    if ctx.reused {
        let _ = writeln!(
            out,
            "- This slot is reused: the checkout is as earlier work left it, on branch `{}` (based on `{}`), \
             with any uncommitted changes and build output. Check `git status` before assuming a clean tree, \
             and don't reset it or switch branches unless the task asks for it.",
            ctx.branch, ctx.base_ref
        );
    } else {
        let _ = writeln!(
            out,
            "- Work on branch `{}` (based on `{}`).",
            ctx.branch, ctx.base_ref
        );
    }
    let _ = writeln!(
        out,
        "- Workflow origin: {} — {}",
        ctx.origin.kind.label(),
        ctx.origin.title
    );
    if let Some(exec) = ctx.slot_exec {
        let _ = writeln!(
            out,
            "- Gates (tests, lint, builds) run inside the slot environment: `{exec} exec -- <command>`. \
             Edit files directly in the checkout."
        );
    }
    out
}

pub struct SessionContext<'a> {
    pub repo_name: &'a str,
    pub slot_name: &'a str,
    pub slot_type: &'a str,
    pub placement: &'a str,
    pub branch: &'a str,
    pub base_ref: &'a str,
    pub origin: &'a Origin,
    /// The slot hosted an earlier workflow; its checkout is kept as is.
    pub reused: bool,
    /// `slot.sh` path when gates must go through `exec` (host placement).
    pub slot_exec: Option<&'a str>,
}

/// Origin for "Create slot from this" on a follow-up.
#[must_use]
pub fn follow_up_origin(follow_up: &FollowUp) -> Origin {
    Origin {
        kind: OriginKind::FollowUp,
        r#ref: follow_up.id.clone(),
        title: follow_up.title.clone(),
        body: follow_up.description.clone(),
        custom_prompt: None,
        url: None,
    }
}

/// How to deliver an answer when the question's tool call no longer exists
/// (the sidecar restarted while it was pending).
#[must_use]
pub fn answer_as_turn(question: &str, answer: &str) -> String {
    format!(
        "Earlier you asked: \"{}\"\nMy answer: {}",
        question.trim(),
        answer.trim()
    )
}

/// A git branch name for a new workflow: `berth/<slug>-<short id>`.
#[must_use]
pub fn branch_name(title: &str, id: &str) -> String {
    let mut slug = String::new();
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-');
    let short = crate::ids::short(id);
    if slug.is_empty() {
        format!("berth/{short}")
    } else {
        format!("berth/{slug}-{short}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use berth_protocol::DiffSide;

    #[test]
    fn prompt_with_review_comments_matches_spec() {
        let p = Prompt {
            text: "Please fix these".into(),
            attachments: vec![],
            review_comments: vec![
                ReviewComment {
                    preset: DiffPreset::Section,
                    path: "src/a.rs".into(),
                    line: 12,
                    side: DiffSide::New,
                    body: "rename this".into(),
                },
                ReviewComment {
                    preset: DiffPreset::Branch,
                    path: "b.rs".into(),
                    line: 3,
                    side: DiffSide::Old,
                    body: "why removed?\nreally".into(),
                },
                ReviewComment {
                    preset: DiffPreset::Section,
                    path: "src/c.rs".into(),
                    line: 1,
                    side: DiffSide::New,
                    body: "ok".into(),
                },
            ],
        };
        assert_eq!(
            serialize_prompt(&p),
            "Please fix these\n\nReview comments on section diff:\n- src/a.rs:12 (new): rename this\n\
             - src/c.rs:1 (new): ok\n\nReview comments on branch diff:\n- b.rs:3 (old): why removed? really"
        );
        assert_eq!(
            serialize_prompt(&Prompt {
                text: "plain".into(),
                review_comments: vec![],
                attachments: vec![],
            }),
            "plain"
        );
    }

    #[test]
    fn attachments_are_listed_with_slot_paths() {
        let p = Prompt {
            text: "What is wrong here?".into(),
            review_comments: vec![],
            attachments: vec![berth_protocol::FileRef {
                id: "f1".into(),
                name: "shot.png".into(),
                mime: "image/png".into(),
                size_bytes: 2048,
                slot_path: Some("/repo/.git/berth-attachments/f1/shot.png".into()),
                stored: None,
                thumbnail: None,
            }],
        };
        assert_eq!(
            serialize_prompt(&p),
            "What is wrong here?\n\nAttached files:\n- shot.png (image/png, 2 KB): /repo/.git/berth-attachments/f1/shot.png"
        );
    }

    #[test]
    fn origin_turns() {
        let o = Origin {
            kind: OriginKind::GhIssue,
            r#ref: "42".into(),
            title: "Crash on start".into(),
            body: "Steps…".into(),
            custom_prompt: Some("Keep it small.".into()),
            url: None,
        };
        assert_eq!(
            origin_turn(&o),
            "Work on GitHub issue #42: Crash on start\n\nSteps…\n\nKeep it small."
        );
    }

    #[test]
    fn a_reused_slot_tells_the_agent_its_tree_is_kept() {
        let origin = Origin {
            kind: OriginKind::Thought,
            r#ref: String::new(),
            title: "Next task".into(),
            body: String::new(),
            custom_prompt: None,
            url: None,
        };
        let ctx = |reused| SessionContext {
            repo_name: "demo",
            slot_name: "fix",
            slot_type: "worktree",
            placement: "host",
            branch: "berth/fix",
            base_ref: "main",
            origin: &origin,
            reused,
            slot_exec: None,
        };
        let fresh = session_context(&ctx(false));
        assert!(fresh.contains("Work on branch `berth/fix` (based on `main`)."));
        let reused = session_context(&ctx(true));
        assert!(reused.contains("This slot is reused"));
        assert!(reused.contains("`git status`"));
        assert!(reused.contains("branch `berth/fix`"));
        assert!(!reused.contains("Work on branch"));
    }

    #[test]
    fn branch_names_are_slugged() {
        assert_eq!(
            branch_name("Fix: the  login bug!", "0193abcd-ffff"),
            "berth/fix-the-login-bug-0193abcd"
        );
        assert_eq!(branch_name("???", "0193abcd-ffff"), "berth/0193abcd");
    }
}
