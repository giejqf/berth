//! Every action has a binding (AGENTS.md §11). Single letters act on the
//! focused pane and never type into a field (GPUI gives text inputs the
//! character first); ⌘-chords are global. ⌘K lists all of them.

use std::path::PathBuf;

use gpui::{Action, App, KeyBinding, actions};

actions!(
    berth,
    [
        // global
        OpenRepository,
        NewWorkflow,
        FollowUpFromOutcome,
        NewWorkflowHere,
        OpenSettings,
        CommandPalette,
        ToggleReview,
        NextSlot,
        PrevSlot,
        JumpToNeeding,
        ToggleRail,
        FocusComposer,
        AttachFiles,
        Interrupt,
        ScrollToNewest,
        FocusAnswer,
        CloseWindow,
        Quit,
        // slot rail
        RailNext,
        RailPrev,
        RailOpen,
        RenameSlot,
        DestroySlot,
        // event flow
        NextNode,
        PrevNode,
        ExpandNode,
        CollapseNode,
        ExpandAll,
        NodeDetails,
        // answer card
        AnswerNext,
        AnswerPrev,
        AnswerSubmit,
        AnswerFreeText,
        AnswerWithText,
        // review pane
        NextChange,
        PrevChange,
        NextSection,
        PrevSection,
        ToggleDiffMode,
        FilterFiles,
        ToggleFileList,
        AskAboutHunk,
        CommentLine,
        PostToPr,
        RefreshReview,
        ReviewSection,
        ReviewWorkflow,
        ReviewBranch,
        // review pane: files
        ShowFiles,
        NextFile,
        PrevFile,
        ToggleZoom,
        QuickLookFile,
        CopyShownFile,
        // dialogs
        Confirm,
        // terminal
        ToggleTerminal,
        HideTerminal,
        NewTerminal,
        KillTerminal,
        NextTerminal,
        PrevTerminal,
        ToggleTerminalSize,
        TerminalCopy,
        TerminalPaste,
        TerminalClear,
        TerminalSelectAll,
    ]
);

/// ⌘1–9: the nth slot in the rail (0-based).
#[derive(Clone, PartialEq, Action)]
#[action(namespace = berth, no_json)]
pub struct SelectSlot(pub usize);

/// 1–9 in the answer card: pick option n (0-based).
#[derive(Clone, PartialEq, Action)]
#[action(namespace = berth, no_json)]
pub struct PickOption(pub usize);

/// Open New workflow on an origin tab (⌘⇧I issue, ⌘⇧R pull request).
#[derive(Clone, PartialEq, Action)]
#[action(namespace = berth, no_json)]
pub struct NewWorkflowTab(pub usize);

/// ⌘1–4 in New workflow: switch the origin tab (0-based).
#[derive(Clone, PartialEq, Action)]
#[action(namespace = berth, no_json)]
pub struct OriginTab(pub usize);

/// Scroll the event flow so row `n` is at the top (devtools; no binding).
#[derive(Clone, PartialEq, Action)]
#[action(namespace = berth, no_json)]
pub struct ScrollFlowTo(pub usize);

/// Open the nth subagent lane of the latest group (devtools; no binding).
#[derive(Clone, PartialEq, Action)]
#[action(namespace = berth, no_json)]
pub struct ExpandLane(pub usize);

/// Show this file (an artifact or attachment, by `FileRef.id`) in the review
/// pane's Files tab (a click on it in the flow; no binding).
#[derive(Clone, PartialEq, Action)]
#[action(namespace = berth, no_json)]
pub struct ShowFile(pub String);

/// Copy this file (the app's copy): an image as image data, anything else as a
/// Finder file reference (a file's right-click menu; no binding).
#[derive(Clone, PartialEq, Action)]
#[action(namespace = berth, no_json)]
pub struct CopyFile(pub PathBuf);

/// Show this file (the app's copy) in Finder (a file's right-click menu).
#[derive(Clone, PartialEq, Action)]
#[action(namespace = berth, no_json)]
pub struct RevealFile(pub PathBuf);

/// Close this terminal (the × on a row of the terminal list; no binding).
#[derive(Clone, PartialEq, Action)]
#[action(namespace = berth, no_json)]
pub struct CloseTerminal(pub gpui::EntityId);

/// Attach these files to the next prompt (drops and devtools; no binding).
#[derive(Clone, PartialEq, Action)]
#[action(namespace = berth, no_json)]
pub struct AttachPaths {
    pub paths: Vec<PathBuf>,
}

/// One row of the command palette / shortcut list.
pub struct Shortcut {
    pub keys: &'static str,
    pub label: &'static str,
    pub group: &'static str,
}

/// Every binding, as the palette lists it.
pub const SHORTCUTS: &[Shortcut] = &[
    Shortcut {
        keys: "⌘O",
        label: "Open repository",
        group: "Global",
    },
    Shortcut {
        keys: "⌘N",
        label: "New workflow",
        group: "Global",
    },
    Shortcut {
        keys: "⌘⇧N",
        label: "New workflow from the selected outcome's follow-up",
        group: "Global",
    },
    Shortcut {
        keys: "⌥⌘N",
        label: "New workflow in the selected slot, keeping its files",
        group: "Global",
    },
    Shortcut {
        keys: "⌘,",
        label: "Settings",
        group: "Global",
    },
    Shortcut {
        keys: "⌘K",
        label: "Command palette",
        group: "Global",
    },
    Shortcut {
        keys: "⌘\\",
        label: "Toggle the review pane",
        group: "Global",
    },
    Shortcut {
        keys: "⌘1–9",
        label: "Jump to the nth slot in the rail",
        group: "Global",
    },
    Shortcut {
        keys: "⌘⇧] ⌘⇧[",
        label: "Next / previous slot",
        group: "Global",
    },
    Shortcut {
        keys: "⌘⇧A",
        label: "Jump to the slot that needs you first",
        group: "Slot rail",
    },
    Shortcut {
        keys: "⌘⇧I",
        label: "New workflow from a GitHub issue",
        group: "Global",
    },
    Shortcut {
        keys: "⌘⇧R",
        label: "New workflow from a pull request",
        group: "Global",
    },
    Shortcut {
        keys: "⌘⇧L",
        label: "Collapse or expand the rail",
        group: "Slot rail",
    },
    Shortcut {
        keys: "↑ ↓",
        label: "Move between slots",
        group: "Slot rail",
    },
    Shortcut {
        keys: "↩",
        label: "Open the slot",
        group: "Slot rail",
    },
    Shortcut {
        keys: "R",
        label: "Rename the slot (or double-click it)",
        group: "Slot rail",
    },
    Shortcut {
        keys: "⌫",
        label: "Destroy slot",
        group: "Slot rail",
    },
    Shortcut {
        keys: "J K",
        label: "Next / previous node",
        group: "Event flow",
    },
    Shortcut {
        keys: "→ ←",
        label: "Expand / collapse a subagent lane or a long node",
        group: "Event flow",
    },
    Shortcut {
        keys: "E",
        label: "Expand all lanes in the group",
        group: "Event flow",
    },
    Shortcut {
        keys: "D",
        label: "Open details (transcript) for the node",
        group: "Event flow",
    },
    Shortcut {
        keys: "⌘.",
        label: "Interrupt the agent",
        group: "Event flow",
    },
    Shortcut {
        keys: "⌘↩",
        label: "Send the prompt",
        group: "Event flow",
    },
    Shortcut {
        keys: "⌘↑",
        label: "Scroll to the newest node",
        group: "Event flow",
    },
    Shortcut {
        keys: "⌘L",
        label: "Focus the composer",
        group: "Event flow",
    },
    Shortcut {
        keys: "⌘⇧O",
        label: "Attach files",
        group: "Event flow",
    },
    Shortcut {
        keys: "A",
        label: "Focus the answer card",
        group: "Answer card",
    },
    Shortcut {
        keys: "1–9",
        label: "Pick an option",
        group: "Answer card",
    },
    Shortcut {
        keys: "↑ ↓",
        label: "Move between options",
        group: "Answer card",
    },
    Shortcut {
        keys: "↩",
        label: "Answer with the selection",
        group: "Answer card",
    },
    Shortcut {
        keys: "Tab",
        label: "Move to the free-text answer",
        group: "Answer card",
    },
    Shortcut {
        keys: "⌘↩",
        label: "Answer with the free text",
        group: "Answer card",
    },
    Shortcut {
        keys: "⌘↓ ⌘↑",
        label: "Next / previous change",
        group: "Review pane",
    },
    Shortcut {
        keys: "⌘] ⌘[",
        label: "Next / previous section",
        group: "Review pane",
    },
    Shortcut {
        keys: "⌘⇧U",
        label: "Unified / split",
        group: "Review pane",
    },
    Shortcut {
        keys: "⌘F",
        label: "Filter files",
        group: "Review pane",
    },
    Shortcut {
        keys: "⌘⇧F",
        label: "Toggle file list",
        group: "Review pane",
    },
    Shortcut {
        keys: "/",
        label: "Ask the agent about this hunk",
        group: "Review pane",
    },
    Shortcut {
        keys: "⌘R",
        label: "Refresh the diff",
        group: "Review pane",
    },
    Shortcut {
        keys: "← →",
        label: "Previous / next file (Files)",
        group: "Review pane",
    },
    Shortcut {
        keys: "Z",
        label: "Fit or actual size (Files)",
        group: "Review pane",
    },
    Shortcut {
        keys: "Space",
        label: "Open the file in Quick Look (Files)",
        group: "Review pane",
    },
    Shortcut {
        keys: "⌘C",
        label: "Copy the file (Files; right-click for Show in Finder)",
        group: "Review pane",
    },
    Shortcut {
        keys: "C",
        label: "Comment on the hovered or focused line",
        group: "Comments & dialogs",
    },
    Shortcut {
        keys: "⌘⇧P",
        label: "Post pending comments to the PR",
        group: "Comments & dialogs",
    },
    Shortcut {
        keys: "esc",
        label: "Cancel the comment, close the dialog",
        group: "Comments & dialogs",
    },
    Shortcut {
        keys: "⌘1–4",
        label: "Switch origin tab in New workflow",
        group: "Comments & dialogs",
    },
    Shortcut {
        keys: "⌘↩",
        label: "Create workflow / confirm",
        group: "Comments & dialogs",
    },
    Shortcut {
        keys: "⌃`",
        label: "Show, focus or collapse the slot's terminal",
        group: "Terminal",
    },
    Shortcut {
        keys: "⌃⇧`",
        label: "New terminal in this slot",
        group: "Terminal",
    },
    Shortcut {
        keys: "⌘⇧] ⌘⇧[",
        label: "Next / previous terminal",
        group: "Terminal",
    },
    Shortcut {
        keys: "⌘K",
        label: "Clear the terminal",
        group: "Terminal",
    },
    Shortcut {
        keys: "⌘C ⌘V",
        label: "Copy the selection / paste",
        group: "Terminal",
    },
];

/// What the command palette can run: `(label, keys, action)`.
pub fn palette_actions() -> Vec<(&'static str, &'static str, Box<dyn Action>)> {
    vec![
        ("New workflow", "⌘N", Box::new(NewWorkflow)),
        (
            "New workflow in this slot…",
            "⌥⌘N",
            Box::new(NewWorkflowHere),
        ),
        (
            "New workflow from a GitHub issue",
            "⌘⇧I",
            Box::new(NewWorkflowTab(1)),
        ),
        (
            "New workflow from a pull request",
            "⌘⇧R",
            Box::new(NewWorkflowTab(2)),
        ),
        (
            "New workflow from the outcome's follow-up",
            "⌘⇧N",
            Box::new(FollowUpFromOutcome),
        ),
        ("Open repository…", "⌘O", Box::new(OpenRepository)),
        ("Settings", "⌘,", Box::new(OpenSettings)),
        ("Toggle the review pane", "⌘\\", Box::new(ToggleReview)),
        ("Section diff", "", Box::new(ReviewSection)),
        ("Workflow diff", "", Box::new(ReviewWorkflow)),
        ("Branch diff", "", Box::new(ReviewBranch)),
        ("Unified / split diff", "⌘⇧U", Box::new(ToggleDiffMode)),
        ("Refresh the diff", "⌘R", Box::new(RefreshReview)),
        ("Post pending comments to the PR", "⌘⇧P", Box::new(PostToPr)),
        ("Focus the composer", "⌘L", Box::new(FocusComposer)),
        ("Attach files…", "⌘⇧O", Box::new(AttachFiles)),
        ("Focus the answer card", "A", Box::new(FocusAnswer)),
        ("Interrupt the agent", "⌘.", Box::new(Interrupt)),
        ("Next slot", "⌘⇧]", Box::new(NextSlot)),
        ("Previous slot", "⌘⇧[", Box::new(PrevSlot)),
        (
            "Jump to the slot that needs you",
            "⌘⇧A",
            Box::new(JumpToNeeding),
        ),
        ("Collapse or expand the rail", "⌘⇧L", Box::new(ToggleRail)),
        (
            "Show or collapse the terminal",
            "⌃`",
            Box::new(ToggleTerminal),
        ),
        ("Hide the terminal panel", "", Box::new(HideTerminal)),
        ("New terminal", "⌃⇧`", Box::new(NewTerminal)),
        ("Kill the terminal", "", Box::new(KillTerminal)),
        (
            "Maximize or restore the terminal",
            "",
            Box::new(ToggleTerminalSize),
        ),
        ("Show the workflow's files", "", Box::new(ShowFiles)),
        ("Rename slot…", "", Box::new(RenameSlot)),
        ("Destroy slot…", "", Box::new(DestroySlot)),
        ("Close window", "⌘W", Box::new(CloseWindow)),
    ]
}

pub fn bindings() -> Vec<KeyBinding> {
    let mut b = vec![
        KeyBinding::new("cmd-o", OpenRepository, None),
        KeyBinding::new("cmd-n", NewWorkflow, None),
        KeyBinding::new("cmd-shift-n", FollowUpFromOutcome, None),
        KeyBinding::new("cmd-alt-n", NewWorkflowHere, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-k", CommandPalette, None),
        KeyBinding::new("cmd-\\", ToggleReview, None),
        KeyBinding::new("cmd-shift-]", NextSlot, None),
        KeyBinding::new("cmd-shift-[", PrevSlot, None),
        KeyBinding::new("cmd-shift-a", JumpToNeeding, None),
        KeyBinding::new("cmd-shift-l", ToggleRail, None),
        KeyBinding::new("cmd-shift-i", NewWorkflowTab(1), None),
        KeyBinding::new("cmd-shift-r", NewWorkflowTab(2), None),
        KeyBinding::new("cmd-l", FocusComposer, None),
        KeyBinding::new("cmd-shift-o", AttachFiles, None),
        KeyBinding::new("cmd-.", Interrupt, None),
        KeyBinding::new("cmd-up", ScrollToNewest, Some("EventFlow")),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-q", Quit, None),
        // terminal (keys without ⌘ go to the shell)
        KeyBinding::new("ctrl-`", ToggleTerminal, None),
        KeyBinding::new("ctrl-~", NewTerminal, None),
        KeyBinding::new("ctrl-shift-`", NewTerminal, None),
        KeyBinding::new("cmd-c", TerminalCopy, Some("Terminal")),
        KeyBinding::new("cmd-v", TerminalPaste, Some("Terminal")),
        KeyBinding::new("cmd-k", TerminalClear, Some("Terminal")),
        KeyBinding::new("cmd-a", TerminalSelectAll, Some("Terminal")),
        KeyBinding::new("cmd-shift-]", NextTerminal, Some("Terminal")),
        KeyBinding::new("cmd-shift-[", PrevTerminal, Some("Terminal")),
        // Tab completes in the shell instead of moving focus (gpui-component's Root).
        KeyBinding::new("tab", gpui::NoAction {}, Some("Terminal")),
        KeyBinding::new("shift-tab", gpui::NoAction {}, Some("Terminal")),
        // slot rail
        KeyBinding::new("down", RailNext, Some("SlotRail")),
        KeyBinding::new("up", RailPrev, Some("SlotRail")),
        KeyBinding::new("enter", RailOpen, Some("SlotRail")),
        KeyBinding::new("r", RenameSlot, Some("SlotRail")),
        KeyBinding::new("backspace", DestroySlot, Some("SlotRail")),
        KeyBinding::new("a", FocusAnswer, Some("SlotRail")),
        // event flow
        KeyBinding::new("j", NextNode, Some("EventFlow")),
        KeyBinding::new("k", PrevNode, Some("EventFlow")),
        KeyBinding::new("right", ExpandNode, Some("EventFlow")),
        KeyBinding::new("left", CollapseNode, Some("EventFlow")),
        KeyBinding::new("e", ExpandAll, Some("EventFlow")),
        KeyBinding::new("d", NodeDetails, Some("EventFlow")),
        KeyBinding::new("a", FocusAnswer, Some("EventFlow")),
        KeyBinding::new("a", FocusAnswer, Some("RepoWindow")),
        // answer card
        KeyBinding::new("down", AnswerNext, Some("AnswerCard")),
        KeyBinding::new("up", AnswerPrev, Some("AnswerCard")),
        KeyBinding::new("enter", AnswerSubmit, Some("AnswerCard")),
        KeyBinding::new("tab", AnswerFreeText, Some("AnswerCard")),
        KeyBinding::new("cmd-enter", AnswerWithText, Some("AnswerCard")),
        // review pane
        KeyBinding::new("cmd-down", NextChange, Some("ReviewPane")),
        KeyBinding::new("cmd-up", PrevChange, Some("ReviewPane")),
        KeyBinding::new("cmd-]", NextSection, Some("ReviewPane")),
        KeyBinding::new("cmd-[", PrevSection, Some("ReviewPane")),
        KeyBinding::new("cmd-shift-u", ToggleDiffMode, None),
        KeyBinding::new("cmd-f", FilterFiles, Some("ReviewPane")),
        KeyBinding::new("cmd-shift-f", ToggleFileList, Some("ReviewPane")),
        KeyBinding::new("/", AskAboutHunk, Some("ReviewPane")),
        KeyBinding::new("c", CommentLine, Some("ReviewPane")),
        KeyBinding::new("cmd-shift-p", PostToPr, None),
        KeyBinding::new("cmd-r", RefreshReview, None),
        KeyBinding::new("right", NextFile, Some("ReviewFiles")),
        KeyBinding::new("left", PrevFile, Some("ReviewFiles")),
        KeyBinding::new("z", ToggleZoom, Some("ReviewFiles")),
        KeyBinding::new("space", QuickLookFile, Some("ReviewFiles")),
        KeyBinding::new("cmd-c", CopyShownFile, Some("ReviewFiles")),
        // dialogs
        KeyBinding::new("cmd-enter", Confirm, Some("Dialog")),
    ];
    for n in 0..9 {
        b.push(KeyBinding::new(
            &format!("cmd-{}", n + 1),
            SelectSlot(n),
            None,
        ));
        b.push(KeyBinding::new(
            &format!("{}", n + 1),
            PickOption(n),
            Some("AnswerCard"),
        ));
    }
    for n in 0..4 {
        b.push(KeyBinding::new(
            &format!("cmd-{}", n + 1),
            OriginTab(n),
            Some("NewWorkflow"),
        ));
    }
    // Single keys (A, C, J, /, 1–9…) act on a pane, but a text field or a
    // terminal inside it must get them as typing. macOS sends a key to the
    // bindings before the text input, so switch them off in those contexts.
    let typed: std::collections::BTreeSet<String> = b
        .iter()
        .filter_map(|binding| match binding.keystrokes() {
            [k] if !k.modifiers().modified() && k.key().chars().count() == 1 => {
                Some(k.key().to_owned())
            }
            _ => None,
        })
        .collect();
    for key in typed {
        for context in ["Input", "Terminal"] {
            b.push(KeyBinding::new(&key, gpui::NoAction {}, Some(context)));
        }
    }
    b
}

pub fn init(cx: &mut App) {
    cx.bind_keys(bindings());
}

#[cfg(test)]
mod tests {
    use gpui::{KeyContext, Keymap, Keystroke};

    use super::*;

    /// Which actions a keystroke resolves to with this focus path.
    fn resolve(keys: &str, contexts: &[&str]) -> Vec<String> {
        let keymap = Keymap::new(bindings());
        let stack: Vec<KeyContext> = contexts
            .iter()
            .map(|c| KeyContext::parse(c).unwrap_or_default())
            .collect();
        let input = [Keystroke::parse(keys).unwrap_or_else(|e| panic!("{keys}: {e}"))];
        keymap
            .bindings_for_input(&input, &stack)
            .0
            .iter()
            .map(|b| b.action().name().to_owned())
            .collect()
    }

    #[test]
    fn terminal_toggle_is_bound_everywhere() {
        assert_eq!(
            resolve("ctrl-`", &["RepoWindow"]),
            ["berth::ToggleTerminal"]
        );
        assert_eq!(
            resolve("ctrl-`", &["RepoWindow", "Terminal"]),
            ["berth::ToggleTerminal"]
        );
    }

    #[test]
    fn single_keys_type_in_fields_and_terminals() {
        assert_eq!(resolve("a", &["RepoWindow"]), ["berth::FocusAnswer"]);
        assert!(resolve("a", &["RepoWindow", "Input"]).is_empty());
        assert!(resolve("a", &["RepoWindow", "Terminal"]).is_empty());
        assert!(resolve("c", &["RepoWindow", "ReviewPane", "Input"]).is_empty());
        assert!(resolve("tab", &["Root", "RepoWindow", "Terminal"]).is_empty());
    }

    #[test]
    fn r_renames_the_slot_from_the_rail_only() {
        assert_eq!(
            resolve("r", &["RepoWindow", "SlotRail"]),
            ["berth::RenameSlot"]
        );
        assert!(resolve("r", &["RepoWindow", "EventFlow"]).is_empty());
        // The rename field itself takes the letter.
        assert!(resolve("r", &["RepoWindow", "Dialog", "Input"]).is_empty());
    }

    #[test]
    fn terminal_keeps_vs_code_chords() {
        // Deepest context first: the terminal's ⌘K wins over the palette.
        assert_eq!(
            resolve("cmd-k", &["RepoWindow", "Terminal"])
                .first()
                .map(String::as_str),
            Some("berth::TerminalClear")
        );
        assert_eq!(resolve("cmd-k", &["RepoWindow"]), ["berth::CommandPalette"]);
    }
}
