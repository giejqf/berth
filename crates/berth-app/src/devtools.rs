//! Development autopilot (feature `devtools`): runs a script against the live
//! UI and saves window screenshots, so the app can be checked without a human.
//!
//! `BERTH_DEV_SCRIPT=<file>` runs one command per line:
//!
//! ```text
//! wait 2                      # seconds
//! wait_state idle 120         # until the selected workflow is idle|awaiting|closed|running (timeout s)
//! shot flow                   # save $BERTH_DEV_SHOTS/flow.png
//! action NewWorkflow          # dispatch a keymap action in the repo window
//! action NewWorkflowTab 3     # … one taking a number
//! escape                      # close the top dialog/sheet
//! thought Title | body        # create a workflow from a thought
//! prompt text                 # send a prompt to the selected workflow
//! answer text                 # answer the pending question (bypasses the card)
//! keys 2 enter                # type keystrokes into the focused element
//! select 0                    # select the n-th slot (newest first)
//! secret linear LINEAR_API_KEY x  # store a connector key (use BERTH_SECRETS=memory)
//! term_type ls -la            # type into the selected slot's terminal (`keys enter` runs it)
//! fake_node Title             # append a just-recorded decision to the flow (view only)
//! fake_artifact /a.png | Title # append an artifact, the file used in place (view only)
//! quit
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use berth_core::WorkflowState;
use berth_protocol::{Origin, OriginKind};
use gpui::{App, AppContext as _, AsyncApp};
use gpui_component::WindowExt as _;

use crate::backend::{Command, NewWorkflow};
use crate::keymap;
use crate::ui;

pub fn start(cx: &mut App) {
    let Ok(script) = std::env::var("BERTH_DEV_SCRIPT") else {
        return;
    };
    let lines = match std::fs::read_to_string(&script) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, script, "devtools script unreadable");
            return;
        }
    };
    let shots = std::env::var("BERTH_DEV_SHOTS").map_or_else(|_| PathBuf::from("."), PathBuf::from);
    let _ = std::fs::create_dir_all(&shots);
    cx.spawn(async move |cx| {
        for line in lines
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            tracing::info!(line, "devtools");
            let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
            match cmd {
                "wait" => sleep(cx, arg.parse().unwrap_or(1.0)).await,
                "wait_state" => wait_state(cx, arg).await,
                "shot" => cx.update(|cx| shot(&shots.join(format!("{arg}.png")), cx)),
                "action" => cx.update(|cx| dispatch(arg, cx)),
                "escape" => cx.update(|cx| {
                    with_window(cx, |window, cx| {
                        window.close_dialog(cx);
                        window.close_sheet(cx);
                    });
                }),
                "thought" => cx.update(|cx| {
                    let (title, body) = arg.split_once('|').unwrap_or((arg, ""));
                    if let Some(state) = ui::first_repo_state(cx) {
                        let s = state.read(cx);
                        s.backend.send(Command::NewWorkflow(NewWorkflow {
                            repo_id: s.repo_id().to_owned(),
                            slot_type: None,
                            slot_id: None,
                            origin: Origin {
                                kind: OriginKind::Thought,
                                r#ref: String::new(),
                                title: title.trim().to_owned(),
                                body: body.trim().to_owned(),
                                custom_prompt: None,
                                url: None,
                            },
                        }));
                    }
                }),
                "term_type" => cx.update(|cx| ui::type_in_terminal(arg, cx)),
                "secret" => cx.update(|cx| {
                    let mut parts = arg.splitn(3, ' ');
                    if let (Some(connector), Some(key), Some(value)) =
                        (parts.next(), parts.next(), parts.next())
                        && let Some(state) = ui::first_repo_state(cx)
                    {
                        state.read(cx).backend.send(Command::SetSecret {
                            connector: connector.to_owned(),
                            key: key.to_owned(),
                            value: value.to_owned(),
                        });
                    }
                }),
                "prompt" | "answer" => cx.update(|cx| {
                    let Some(state) = ui::first_repo_state(cx) else {
                        return;
                    };
                    let s = state.read(cx);
                    let Some(view) = s.selected_view() else {
                        return;
                    };
                    let wf = view.workflow.id.clone();
                    let command = match view.pending_question() {
                        Some((_, q)) if cmd == "answer" => Command::Answer {
                            workflow_id: wf,
                            question_id: q.question_id.clone(),
                            text: arg.to_owned(),
                        },
                        _ => Command::SendPrompt {
                            workflow_id: wf,
                            text: arg.to_owned(),
                            attachments: vec![],
                        },
                    };
                    s.backend.send(command);
                }),
                // A real file drag over the composer (`drag <paths…>`), then `drop`.
                "drag" | "drop" => cx.update(|cx| {
                    let paths: Vec<PathBuf> = arg.split_whitespace().map(PathBuf::from).collect();
                    with_window(cx, |window, cx| {
                        let size = window.viewport_size();
                        // Over the composer's text box.
                        let position = gpui::point(size.width * 0.35, size.height - gpui::px(70.));
                        let events = if cmd == "drag" {
                            vec![
                                gpui::FileDropEvent::Entered {
                                    position,
                                    paths: gpui::ExternalPaths(paths.into()),
                                },
                                gpui::FileDropEvent::Pending { position },
                            ]
                        } else {
                            vec![gpui::FileDropEvent::Submit { position }]
                        };
                        for e in events {
                            window.dispatch_event(gpui::PlatformInput::FileDrop(e), cx);
                        }
                        // Hover hit-testing uses the previous frame's hitboxes.
                        window.draw(cx).clear(cx);
                        window.draw(cx).clear(cx);
                    });
                }),
                // Scroll the event flow by N pixels (positive = up / back in time).
                "scroll" => cx.update(|cx| {
                    let dy: f32 = arg.parse().unwrap_or(400.0);
                    with_window(cx, |window, cx| {
                        let size = window.viewport_size();
                        window.dispatch_event(
                            gpui::PlatformInput::ScrollWheel(gpui::ScrollWheelEvent {
                                position: gpui::point(size.width * 0.35, size.height * 0.4),
                                delta: gpui::ScrollDelta::Pixels(gpui::point(
                                    gpui::px(0.),
                                    gpui::px(dy),
                                )),
                                modifiers: gpui::Modifiers::default(),
                                touch_phase: gpui::TouchPhase::Moved,
                            }),
                            cx,
                        );
                        window.draw(cx).clear(cx);
                    });
                }),
                // Queue files in the composer, as a drop would.
                "attach" => cx.update(|cx| {
                    let paths = arg.split_whitespace().map(PathBuf::from).collect();
                    with_window(cx, |window, cx| {
                        window.dispatch_action(Box::new(keymap::AttachPaths { paths }), cx);
                    });
                }),
                "select" => cx.update(|cx| {
                    let Some(state) = ui::first_repo_state(cx) else {
                        return;
                    };
                    let n: usize = arg.parse().unwrap_or(0);
                    let id = state
                        .read(cx)
                        .slots
                        .iter()
                        .rev()
                        .nth(n)
                        .map(|s| s.slot.id.clone());
                    if let Some(id) = id {
                        state.update(cx, |s, cx| s.select_slot(id, cx));
                    }
                }),
                // Select the slot whose workflow title contains `arg`.
                "select_title" => cx.update(|cx| {
                    let Some(state) = ui::first_repo_state(cx) else {
                        return;
                    };
                    let id = state
                        .read(cx)
                        .slots
                        .iter()
                        .find(|s| {
                            s.workflow
                                .as_ref()
                                .is_some_and(|w| w.origin.title.contains(arg))
                        })
                        .map(|s| s.slot.id.clone());
                    if let Some(id) = id {
                        state.update(cx, |s, cx| s.select_slot(id, cx));
                    }
                }),
                // Pretend a workflow's round is live (`fake_live <preset>`):
                // the design's Working state without spending tokens.
                "fake_live" => cx.update(|cx| fake_live(arg, cx)),
                "fake_node" => cx.update(|cx| fake_node(arg, cx)),
                "fake_artifact" => cx.update(|cx| {
                    let (path, title) = arg.split_once('|').unwrap_or((arg, "Artifact"));
                    fake_artifact(path.trim(), title.trim(), cx);
                }),
                "expand_lane" => cx.update(|cx| {
                    let n: usize = arg.parse().unwrap_or(0);
                    with_window(cx, |window, cx| {
                        window.dispatch_action(Box::new(keymap::ExpandLane(n)), cx);
                        window.draw(cx).clear(cx);
                    });
                }),
                // Scroll the flow so row N is at the top.
                "flow_top" => cx.update(|cx| {
                    let n: usize = arg.parse().unwrap_or(0);
                    with_window(cx, |window, cx| {
                        window.dispatch_action(Box::new(keymap::ScrollFlowTo(n)), cx);
                        window.draw(cx).clear(cx);
                    });
                }),
                "comment" => cx.update(|cx| {
                    let Some(state) = ui::first_repo_state(cx) else {
                        return;
                    };
                    let s = state.read(cx);
                    let Some(wf) = s.selected_workflow_id() else {
                        return;
                    };
                    let Some(diff) = s.diffs.get(&wf).cloned() else {
                        return;
                    };
                    let (target, body) = arg.split_once(' ').unwrap_or((arg, ""));
                    let (path, line) = target.rsplit_once(':').unwrap_or((target, "1"));
                    s.backend
                        .send(Command::AddReviewComment(crate::backend::NewComment {
                            workflow_id: wf,
                            from: diff.from.clone(),
                            to: diff.to.clone(),
                            preset: diff.request.preset,
                            path: path.to_owned(),
                            side: berth_protocol::DiffSide::New,
                            line: line.parse().unwrap_or(1),
                            body: body.to_owned(),
                        }));
                }),
                "details_last" => cx.update(|cx| {
                    let Some(state) = ui::first_repo_state(cx) else {
                        return;
                    };
                    let s = state.read(cx);
                    let Some(view) = s.selected_view() else {
                        return;
                    };
                    if let Some(node) = view.nodes.iter().rev().find(|n| n.raw_first.is_some()) {
                        s.backend.send(Command::LoadDetails {
                            workflow_id: view.workflow.id.clone(),
                            node_id: node.id.clone(),
                        });
                    }
                }),
                "followup" => cx.update(|cx| {
                    let Some(state) = ui::first_repo_state(cx) else {
                        return;
                    };
                    let s = state.read(cx);
                    let Some(view) = s.selected_view() else {
                        return;
                    };
                    let fu = view.nodes.iter().rev().find_map(|n| match &n.payload {
                        berth_protocol::NodePayload::Outcome(o) => o.follow_ups.first().cloned(),
                        berth_protocol::NodePayload::Summary(sm) => sm.follow_ups.first().cloned(),
                        _ => None,
                    });
                    if let Some(follow_up) = fu {
                        s.backend.send(Command::FollowUpToSlot {
                            workflow_id: view.workflow.id.clone(),
                            follow_up,
                            reuse: false,
                        });
                    }
                }),
                // Real keystrokes through GPUI's dispatch, e.g. `keys 2 enter`.
                "keys" => cx.update(|cx| {
                    let strokes: Vec<gpui::Keystroke> = arg
                        .split_whitespace()
                        .filter_map(|k| gpui::Keystroke::parse(k).ok())
                        .collect();
                    with_window(cx, |window, cx| {
                        // A frame registers the focused input's handler.
                        window.draw(cx).clear(cx);
                        for k in strokes {
                            window.dispatch_keystroke(k, cx);
                        }
                    });
                }),
                "quit" => cx.update(|cx| cx.quit()),
                other => tracing::warn!(other, "unknown devtools command"),
            }
        }
    })
    .detach();
}

async fn sleep(cx: &mut AsyncApp, secs: f64) {
    cx.background_executor()
        .timer(Duration::from_secs_f64(secs.max(0.0)))
        .await;
}

async fn wait_state(cx: &mut AsyncApp, arg: &str) {
    let mut parts = arg.split_whitespace();
    let want = parts.next().unwrap_or("idle").to_owned();
    let timeout: f64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(120.0);
    let deadline = Instant::now() + Duration::from_secs_f64(timeout);
    while Instant::now() < deadline {
        let state = cx.update(|cx| {
            ui::first_repo_state(cx)
                .and_then(|s| s.read(cx).selected_view())
                .map(|v| v.workflow.state)
        });
        let hit = match state {
            Some(WorkflowState::Idle) => matches!(want.as_str(), "idle" | "settled"),
            Some(WorkflowState::AwaitingAnswer) => matches!(want.as_str(), "awaiting" | "settled"),
            Some(WorkflowState::Closed) => matches!(want.as_str(), "closed" | "settled"),
            Some(WorkflowState::Running) => want == "running",
            Some(WorkflowState::Starting) | None => false,
        };
        if hit {
            return;
        }
        sleep(cx, 0.5).await;
    }
    tracing::warn!(want, "wait_state timed out");
}

/// Mark workflows as running and give them the design's live activity.
fn fake_live(preset: &str, cx: &mut App) {
    use berth_core::activity::{RoundActivity, Step};
    let Some(state) = ui::first_repo_state(cx) else {
        return;
    };
    let now = ui::kit::now_ms();
    let step = |id: &str, doing: &str, done: &str, start: i64, end: Option<i64>| Step {
        tool_use_id: id.into(),
        doing: doing.into(),
        done: done.into(),
        started_at: now - start * 1000,
        finished_at: end.map(|e| now - e * 1000),
        failed: false,
    };
    let lives: Vec<(&str, RoundActivity)> = match preset {
        "working" => vec![
            (
                "Add a unit test for greet",
                RoundActivity {
                    started_at: now - 72_000,
                    title: Some("Running both checks in parallel".into()),
                    steps: vec![
                        step(
                            "1",
                            "Reading `Cargo.toml`",
                            "Read `Cargo.toml` — found a binary crate, no lib",
                            72,
                            Some(34),
                        ),
                        step(
                            "2",
                            "Running `cargo test`",
                            "Ran `cargo test` — 0 tests, compiles clean",
                            34,
                            Some(13),
                        ),
                        step(
                            "3",
                            "Reading `main.rs` to decide where the test lives",
                            "Read `main.rs`",
                            5,
                            None,
                        ),
                    ],
                    tool_calls: 3,
                    subagents: 2,
                    branch_steps: Default::default(),
                },
            ),
            (
                "Migrate config to TOML",
                RoundActivity {
                    started_at: now - 20_000,
                    title: Some("Reading crate layout".into()),
                    steps: vec![step(
                        "c1",
                        "Reading crate layout",
                        "Read crate layout",
                        20,
                        None,
                    )],
                    tool_calls: 1,
                    subagents: 0,
                    branch_steps: Default::default(),
                },
            ),
        ],
        _ => vec![(
            "Migrate config to TOML",
            RoundActivity {
                started_at: now - 160_000,
                title: Some("Editing config.rs".into()),
                steps: vec![step(
                    "c1",
                    "Editing `config.rs`",
                    "Edited `config.rs`",
                    160,
                    None,
                )],
                tool_calls: 4,
                subagents: 0,
                branch_steps: Default::default(),
            },
        )],
    };
    state.update(cx, |s, cx| {
        for (title, activity) in lives {
            let Some(entry) = s
                .slots
                .iter_mut()
                .find(|e| e.workflow.as_ref().is_some_and(|w| w.origin.title == title))
            else {
                continue;
            };
            let Some(wf) = entry.workflow.as_mut() else {
                continue;
            };
            wf.state = berth_core::WorkflowState::Running;
            let id = wf.id.clone();
            if let Some(v) = s.views.get(&id) {
                let mut v = (**v).clone();
                v.workflow.state = berth_core::WorkflowState::Running;
                s.views.insert(id.clone(), std::sync::Arc::new(v));
            }
            s.activity.insert(id, activity);
        }
        cx.notify();
    });
}

/// Append an artifact (a local file, used in place) to the selected workflow's
/// flow, as if the agent had presented it (the view only; nothing is stored).
fn fake_artifact(path: &str, title: &str, cx: &mut App) {
    let name = path.rsplit('/').next().unwrap_or(path).to_owned();
    let size_bytes = std::fs::metadata(path).map_or(0, |m| m.len());
    push_fake_node(
        berth_protocol::NodePayload::Artifact(berth_protocol::Artifact {
            artifact_id: berth_core::ids::new_id(),
            title: title.to_owned(),
            description: Some("Added by the devtools script.".to_owned()),
            file: berth_protocol::FileRef {
                id: berth_core::ids::new_id(),
                mime: berth_core::files::mime_for(&name).to_owned(),
                name,
                size_bytes,
                slot_path: None,
                stored: Some(path.to_owned()),
                thumbnail: None,
            },
            error: None,
        }),
        cx,
    );
}

/// Append a decision titled `title` to the selected workflow's flow, as if
/// the agent had just recorded it (the view only; nothing is stored).
fn fake_node(title: &str, cx: &mut App) {
    push_fake_node(
        berth_protocol::NodePayload::Decision(berth_protocol::Decision {
            title: title.to_owned(),
            rationale: "Added by the devtools script.".to_owned(),
            affects: Vec::new(),
            reversible: true,
        }),
        cx,
    );
}

fn push_fake_node(payload: berth_protocol::NodePayload, cx: &mut App) {
    let Some(state) = ui::first_repo_state(cx) else {
        return;
    };
    state.update(cx, |s, cx| {
        let Some(view) = s.selected_view() else {
            return;
        };
        let mut v = (*view).clone();
        let seq = v.nodes.last().map_or(0, |n| n.seq + 1);
        v.nodes.push(berth_core::Node {
            id: format!("devtools-{seq}"),
            workflow_id: v.workflow.id.clone(),
            round_id: None,
            branch_id: None,
            seq,
            payload,
            raw_first: None,
            raw_last: None,
            created_at: ui::kit::now_ms(),
        });
        s.views
            .insert(v.workflow.id.clone(), std::sync::Arc::new(v));
        cx.notify();
    });
}

fn with_window(cx: &mut App, f: impl FnOnce(&mut gpui::Window, &mut App)) {
    if let Some(handle) = ui::first_repo_window(cx) {
        let _ = cx.update_window(handle.into(), |_, window, cx| f(window, cx));
    }
}

fn dispatch(name: &str, cx: &mut App) {
    let action: Box<dyn gpui::Action> = match name {
        "NewWorkflow" => Box::new(keymap::NewWorkflow),
        "NewWorkflowHere" => Box::new(keymap::NewWorkflowHere),
        "ChooseModel" => Box::new(keymap::ChooseModel),
        "ToggleReview" => Box::new(keymap::ToggleReview),
        "ReviewSection" => Box::new(keymap::ReviewSection),
        "ReviewWorkflow" => Box::new(keymap::ReviewWorkflow),
        "ReviewBranch" => Box::new(keymap::ReviewBranch),
        "ToggleDiffMode" => Box::new(keymap::ToggleDiffMode),
        "OpenSettings" => Box::new(keymap::OpenSettings),
        "DestroySlot" => Box::new(keymap::DestroySlot),
        "RenameSlot" => Box::new(keymap::RenameSlot),
        "ShowFiles" => Box::new(keymap::ShowFiles),
        "ShowFindings" => Box::new(keymap::ShowFindings),
        "NextFinding" => Box::new(keymap::NextFinding),
        "NextChoice" => Box::new(keymap::NextChoice),
        "WriteDecision" => Box::new(keymap::WriteDecision),
        "SendDecisions" => Box::new(keymap::SendDecisions),
        "NextFile" => Box::new(keymap::NextFile),
        "PrevFile" => Box::new(keymap::PrevFile),
        "ToggleZoom" => Box::new(keymap::ToggleZoom),
        "FocusComposer" => Box::new(keymap::FocusComposer),
        "NextSlot" => Box::new(keymap::NextSlot),
        "PrevSlot" => Box::new(keymap::PrevSlot),
        "ToggleTerminal" => Box::new(keymap::ToggleTerminal),
        "HideTerminal" => Box::new(keymap::HideTerminal),
        "ToggleRail" => Box::new(keymap::ToggleRail),
        "NewTerminal" => Box::new(keymap::NewTerminal),
        "KillTerminal" => Box::new(keymap::KillTerminal),
        "NextTerminal" => Box::new(keymap::NextTerminal),
        "ToggleTerminalSize" => Box::new(keymap::ToggleTerminalSize),
        "TerminalClear" => Box::new(keymap::TerminalClear),
        other => match other
            .strip_prefix("NewWorkflowTab ")
            .and_then(|n| n.trim().parse().ok())
        {
            Some(n) => Box::new(keymap::NewWorkflowTab(n)),
            None => {
                tracing::warn!(other, "unknown action");
                return;
            }
        },
    };
    with_window(cx, move |window, cx| window.dispatch_action(action, cx));
}

fn shot(path: &std::path::Path, cx: &mut App) {
    let Some(handle) = ui::first_repo_window(cx).or_else(|| ui::welcome_window(cx)) else {
        tracing::warn!("no window to capture");
        return;
    };
    // Draw a fresh frame first: without a display link (asleep or headless
    // display) the window does not redraw on its own.
    // `update_window` (not `WindowHandle::update`) so the root view is not leased while drawing.
    let result = cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.render_to_image()
    });
    match result {
        Ok(Ok(image)) => match image.save(path) {
            Ok(()) => tracing::info!(path = %path.display(), "screenshot saved"),
            Err(e) => tracing::error!(error = %e, "screenshot save failed"),
        },
        Ok(Err(e)) | Err(e) => tracing::error!(error = %e, "screenshot failed"),
    }
}
