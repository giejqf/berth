//! Messages between the agents of a repo's slots (`message_slot`, ADR 0021):
//! which slot a message is for, whether it reaches that agent now, and what
//! each side reads about it.

use std::fmt::Write as _;

use crate::model::{Slot, SlotState, Workflow, WorkflowState};

/// Agent messages in a row, without the user, that still reach the other
/// agent: a message and its answer. The next one waits for the user, so two
/// agents can't keep each other going.
pub const MAX_HOPS: u32 = 2;

/// What happens to a message when it arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// The recipient's agent is in a round: the message joins it and is read
    /// at the agent's next step (or after the user answers its question).
    Join,
    /// The recipient's agent is idle: the message starts a round of its own.
    Wake,
    /// It waits for the user in the recipient's slot; why, said to them.
    Hold(String),
}

/// The name a slot goes by: what the user called it, else its workflow's
/// title, else its own name.
#[must_use]
pub fn slot_name(slot: &Slot, workflow: Option<&Workflow>) -> String {
    slot.custom_name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .or_else(|| workflow.map(|w| w.origin.title.clone()))
        .unwrap_or_else(|| slot.name.clone())
}

/// The slot `to` names among the repo's `slots` (each with the workflow it
/// shows), other than the sender's: by its name, title, slot name or branch,
/// ignoring case; else the one whose name contains it. `Err` is what the
/// agent reads: no such slot, or which ones it could mean.
pub fn find_slot<'a>(
    slots: &'a [(Slot, Option<Workflow>)],
    from_slot_id: &str,
    to: &str,
) -> Result<(&'a Slot, &'a Workflow), String> {
    let want = to.trim().trim_matches('"').to_lowercase();
    let others: Vec<(&Slot, &Workflow)> = slots
        .iter()
        .filter(|(s, _)| s.id != from_slot_id && s.state != SlotState::Destroyed)
        .filter_map(|(s, w)| w.as_ref().map(|w| (s, w)))
        .collect();
    let names = |s: &Slot, w: &Workflow| {
        [
            slot_name(s, Some(w)),
            w.origin.title.clone(),
            s.name.clone(),
            s.branch.clone(),
        ]
        .map(|n| n.to_lowercase())
    };
    let exact: Vec<_> = others
        .iter()
        .filter(|(s, w)| names(s, w).contains(&want))
        .collect();
    let found = if exact.is_empty() && !want.is_empty() {
        others
            .iter()
            .filter(|(s, w)| names(s, w).iter().any(|n| n.contains(&want)))
            .collect()
    } else {
        exact
    };
    match found.as_slice() {
        [(s, w)] => Ok((s, w)),
        [] if others.is_empty() => {
            Err("There is no other slot in this repository to send a message to.".to_owned())
        }
        [] => Err(format!(
            "No slot is called \"{}\". The other slots:\n{}",
            to.trim(),
            listing(&others)
        )),
        many => Err(format!(
            "\"{}\" could be more than one slot; use its full name:\n{}",
            to.trim(),
            listing(&many.iter().map(|&&p| p).collect::<Vec<_>>())
        )),
    }
}

fn listing(slots: &[(&Slot, &Workflow)]) -> String {
    let mut out = String::new();
    for (s, w) in slots {
        let _ = writeln!(
            out,
            "- \"{}\" (branch `{}`)",
            slot_name(s, Some(w)),
            s.branch
        );
    }
    out.trim_end().to_owned()
}

/// Where a message `hop` long goes in a workflow in `state`; `live`:
/// whether that workflow's agent session is up, `slot` its slot's state.
#[must_use]
pub fn plan(state: WorkflowState, slot: SlotState, hop: u32, live: bool) -> Delivery {
    let hold = |why: &str| Delivery::Hold(why.to_owned());
    match (slot, state) {
        (SlotState::Failed, _) => hold("its slot failed"),
        (SlotState::Creating | SlotState::Destroying | SlotState::Destroyed, _)
        | (_, WorkflowState::Starting) => hold("its agent hasn't started yet"),
        (_, WorkflowState::Closed) => hold("its workflow is closed"),
        _ if hop > MAX_HOPS => hold(
            "the agents have messaged each other back and forth without you; you decide whether it goes on",
        ),
        // Waiting on an answer, the agent's question is still open in its
        // session: the message is read once the user answers.
        (_, WorkflowState::Running | WorkflowState::AwaitingAnswer) if live => Delivery::Join,
        (_, WorkflowState::AwaitingAnswer) => hold("it waits on your answer to its question"),
        (_, WorkflowState::Idle) => Delivery::Wake,
        (_, WorkflowState::Running) => hold("its agent isn't reachable right now"),
    }
}

/// The turn the recipient's agent reads.
#[must_use]
pub fn message_turn(from: &str, branch: &str, text: &str) -> String {
    format!(
        "Message from slot \"{from}\" (branch `{branch}`), from its agent:\n\n{}\n\n\
         If it needs an answer, send one with message_slot (to: \"{from}\").",
        text.trim()
    )
}

/// What the sending agent reads about where its message went.
#[must_use]
pub fn sent_text(to: &str, delivery: &Delivery, waits_on_answer: bool) -> String {
    match delivery {
        Delivery::Join if waits_on_answer => format!(
            "Delivered to \"{to}\": its agent waits on the user's answer to a question and reads \
             your message after that. Any answer comes back to you as a message."
        ),
        Delivery::Join => format!(
            "Delivered to \"{to}\": its agent is working and reads your message at its next step. \
             Any answer comes back to you as a message."
        ),
        Delivery::Wake => format!(
            "Delivered to \"{to}\": its agent was idle and has started on your message. Any answer \
             comes back to you as a message."
        ),
        Delivery::Hold(why) => format!(
            "Held for the user in \"{to}\" ({why}); they decide whether its agent gets it. \
             Carry on with your own work."
        ),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use berth_protocol::{Origin, OriginKind};

    use super::*;

    fn slot(id: &str, name: &str, custom: Option<&str>) -> Slot {
        Slot {
            id: id.into(),
            repo_id: "r".into(),
            slot_type: "worktree".into(),
            name: name.into(),
            custom_name: custom.map(Into::into),
            model: None,
            effort: None,
            state: SlotState::Ready,
            state_detail: None,
            checkout_path: None,
            placement: "host".into(),
            branch: format!("berth/{name}"),
            base_ref: "origin/main".into(),
            created_at: 0,
        }
    }

    fn wf(title: &str) -> Workflow {
        let origin = Origin {
            kind: OriginKind::Thought,
            r#ref: String::new(),
            title: title.into(),
            body: String::new(),
            custom_prompt: None,
            url: None,
        };
        Workflow {
            id: "w".into(),
            slot_id: "s".into(),
            origin,
            state: WorkflowState::Idle,
            claude_session_id: None,
            section_no: 1,
            total_cost_usd: 0.0,
            last_error: None,
            created_at: 0,
            closed_at: None,
        }
    }

    #[test]
    fn finds_slots_by_name_title_or_part() {
        let slots = vec![
            (
                slot("a", "build-the-frontend", None),
                Some(wf("Build the frontend")),
            ),
            (
                slot("b", "build-the-api", Some("Backend")),
                Some(wf("Build the API")),
            ),
            (slot("c", "docs", None), Some(wf("Write the docs"))),
        ];
        assert_eq!(
            find_slot(&slots, "a", "backend").map(|f| &f.0.id),
            Ok(&"b".to_owned())
        );
        assert_eq!(
            find_slot(&slots, "a", "Build the API").map(|f| &f.0.id),
            Ok(&"b".to_owned())
        );
        assert_eq!(
            find_slot(&slots, "a", "\"docs\"").map(|f| &f.0.id),
            Ok(&"c".to_owned())
        );
        assert_eq!(
            find_slot(&slots, "a", "the do").map(|f| &f.0.id),
            Ok(&"c".to_owned())
        );
        // Not the sender's own slot.
        let own = find_slot(&slots, "a", "frontend").unwrap_err();
        assert!(own.contains("\"Backend\""), "{own}");
        let many = find_slot(&slots, "c", "build").unwrap_err();
        assert!(many.contains("more than one"), "{many}");
        let alone = find_slot(&slots[..1], "a", "x").unwrap_err();
        assert!(alone.contains("no other slot"), "{alone}");
    }

    #[test]
    fn delivery_follows_the_recipient_and_the_guard() {
        use WorkflowState as W;
        let ready = SlotState::Ready;
        assert_eq!(plan(W::Idle, ready, 1, false), Delivery::Wake);
        assert_eq!(plan(W::Running, ready, 2, true), Delivery::Join);
        assert_eq!(plan(W::AwaitingAnswer, ready, 1, true), Delivery::Join);
        for (state, slot, hop, live) in [
            (W::Idle, ready, MAX_HOPS + 1, false),
            (W::Running, ready, MAX_HOPS + 1, true),
            (W::AwaitingAnswer, ready, 1, false),
            (W::Closed, ready, 1, false),
            (W::Starting, ready, 1, false),
            (W::Idle, SlotState::Failed, 1, false),
        ] {
            assert!(
                matches!(plan(state, slot, hop, live), Delivery::Hold(_)),
                "{state:?} {slot:?} {hop}"
            );
        }
    }

    #[test]
    fn the_recipient_reads_who_wrote_and_how_to_answer() {
        let t = message_turn("Frontend", "berth/frontend", "  The API 500s.\n");
        assert!(t.starts_with("Message from slot \"Frontend\""), "{t}");
        assert!(t.contains("\n\nThe API 500s.\n\n"), "{t}");
        assert!(t.contains("to: \"Frontend\""), "{t}");
    }
}
