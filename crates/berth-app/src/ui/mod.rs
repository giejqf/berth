//! GPUI side: a repo's windows (one or more, each a group of its slots; ADR
//! 0011), fed by backend events.

mod agent_dialog;
mod answer_card;
mod composer;
mod details;
mod diff_view;
mod empty_state;
mod files;
mod flow_view;
mod history;
pub mod kit;
mod modal;
mod motion;
mod new_workflow;
mod nodes;
mod palette;
mod rename_slot;
mod repo_state;
mod repo_window;
mod review_pane;
mod rewind;
pub mod rich;
mod select;
mod settings;
mod sign_in;
mod slot_rail;
mod terminal;
mod usage;
mod welcome;
pub mod windows;

use std::collections::HashMap;

use futures::StreamExt as _;
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{
    App, AppContext as _, Bounds, DisplayId, Entity, Global, PathPromptOptions, Pixels, Size,
    WeakEntity, WindowBounds, WindowHandle, WindowOptions, point, px, size,
};
use gpui_component::{Root, TitleBar};

use crate::backend::{BackendHandle, Command, EnvStatus, RecentRepo, UiEvent};
pub use repo_state::RepoState;

/// Claude Code / GitHub / Docker status, app-wide.
#[derive(Default)]
pub struct EnvGlobal(pub EnvStatus);
impl Global for EnvGlobal {}

/// Window options for Berth's windows: a 44px title bar drawn by the app,
/// traffic lights placed on it.
pub fn window_options(sz: Size<Pixels>, min: Size<Pixels>, cx: &App) -> WindowOptions {
    let mut options = TitleBar::window_options();
    if let Some(titlebar) = options.titlebar.as_mut() {
        titlebar.traffic_light_position = Some(point(px(15.), px(15.)));
    }
    options.window_bounds = Some(WindowBounds::Windowed(Bounds::centered(None, sz, cx)));
    options.window_min_size = Some(min);
    options
}

/// One open window of a repo.
#[derive(Clone)]
pub struct RepoWindowRef {
    pub repo_id: String,
    pub window_id: String,
    pub state: WeakEntity<RepoState>,
    pub view: WeakEntity<repo_window::RepoWindow>,
    pub handle: WindowHandle<Root>,
}

/// App-wide state: the backend handle and the open repo windows.
pub struct Berth {
    pub backend: BackendHandle,
    /// Each repo's windows, in the order they opened.
    repos: HashMap<String, Vec<RepoWindowRef>>,
    welcome: Option<WindowHandle<Root>>,
    /// The models the agent can run on, as the SDK last reported them.
    pub models: Vec<berth_protocol::ModelChoice>,
    /// The plan's limits, as a session last reported them (ADR 0014).
    pub quota: Option<berth_core::Quota>,
    /// Who Claude Code works as, as a session last reported it (ADR 0016).
    pub account: Option<berth_protocol::Account>,
    /// The review pane's width as the user dragged it (`settings.review_width`);
    /// `None` is the design's.
    pub review_width: Option<f32>,
}

impl Global for Berth {}

impl Berth {
    pub fn backend(cx: &App) -> BackendHandle {
        cx.global::<Berth>().backend.clone()
    }
}

pub fn init(backend: BackendHandle, events: UnboundedReceiver<UiEvent>, cx: &mut App) {
    files::register_actions(cx);
    cx.set_global(Berth {
        backend,
        repos: HashMap::new(),
        welcome: None,
        models: Vec::new(),
        quota: None,
        account: None,
        review_width: None,
    });
    cx.spawn(async move |cx| {
        let mut events = events;
        while let Some(event) = events.next().await {
            cx.update(|cx| route(event, cx));
        }
    })
    .detach();
}

/// Ask the user for a folder and open it as a repo.
pub fn prompt_open_repository(cx: &mut App) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Open Repository".into()),
    });
    let backend = Berth::backend(cx);
    cx.spawn(async move |_| {
        if let Ok(Ok(Some(paths))) = paths.await
            && let Some(path) = paths.into_iter().next()
        {
            backend.send(Command::OpenRepo { path });
        }
    })
    .detach();
}

/// Type into the selected slot's active terminal, in `state`'s window (devtools).
#[cfg(feature = "devtools")]
pub fn type_in_terminal(state: Option<Entity<RepoState>>, text: &str, cx: &mut App) {
    let Some(state) = state.or_else(|| first_repo_state(cx)) else {
        return;
    };
    let view = state
        .read(cx)
        .slot_terminals()
        .and_then(|t| t.active())
        .cloned();
    if let Some(view) = view {
        let bytes = text.as_bytes().to_vec();
        view.update(cx, |v, cx| v.input(bytes, cx));
    }
}

/// The first open repo's state and window (devtools).
#[allow(dead_code)]
pub fn first_repo_state(cx: &App) -> Option<Entity<RepoState>> {
    live_repos(cx).into_iter().next()
}

#[allow(dead_code)]
pub fn first_repo_window(cx: &App) -> Option<WindowHandle<Root>> {
    repo_window_at(0, cx)
}

/// The `n`th open repo window, in the order they opened (devtools).
#[allow(dead_code)]
pub fn repo_window_at(n: usize, cx: &App) -> Option<WindowHandle<Root>> {
    let mut all: Vec<&RepoWindowRef> = cx
        .global::<Berth>()
        .repos
        .values()
        .flatten()
        .filter(|w| w.state.upgrade().is_some())
        .collect();
    all.sort_by_key(|w| w.handle.window_id().as_u64());
    all.get(n).map(|w| w.handle)
}

/// The repo state of the window `window` (devtools).
#[allow(dead_code)]
pub fn state_of(window: &gpui::Window, cx: &App) -> Option<Entity<RepoState>> {
    let handle = window.window_handle();
    cx.global::<Berth>()
        .repos
        .values()
        .flatten()
        .find(|w| gpui::AnyWindowHandle::from(w.handle) == handle)
        .and_then(|w| w.state.upgrade())
}

#[allow(dead_code)]
pub fn welcome_window(cx: &App) -> Option<WindowHandle<Root>> {
    cx.global::<Berth>().welcome
}

fn live_repos(cx: &App) -> Vec<Entity<RepoState>> {
    cx.global::<Berth>()
        .repos
        .values()
        .flatten()
        .filter_map(|w| w.state.upgrade())
        .collect()
}

/// Every open window's state for the repo.
fn repo_states(repo_id: &str, cx: &App) -> Vec<Entity<RepoState>> {
    cx.global::<Berth>()
        .repos
        .get(repo_id)
        .map(|v| v.iter().filter_map(|w| w.state.upgrade()).collect())
        .unwrap_or_default()
}

/// The repo's frontmost window's state (for what one window should show).
fn repo(repo_id: &str, cx: &App) -> Option<Entity<RepoState>> {
    windows::of_repo(repo_id, cx)
        .into_iter()
        .find_map(|w| w.state.upgrade())
}

fn route(event: UiEvent, cx: &mut App) {
    match event {
        UiEvent::Environment(env) => {
            cx.set_global(EnvGlobal(*env));
            cx.refresh_windows();
        }
        UiEvent::Models(models) => {
            cx.global_mut::<Berth>().models = models;
            cx.refresh_windows();
        }
        UiEvent::Quota(quota) => {
            cx.global_mut::<Berth>().quota = quota;
            cx.refresh_windows();
        }
        UiEvent::Account(account) => {
            cx.global_mut::<Berth>().account = account;
            cx.refresh_windows();
        }
        UiEvent::Settings {
            model,
            theme,
            review_width,
        } => {
            cx.global_mut::<Berth>().review_width = review_width;
            crate::theme::set_appearance(crate::theme::Appearance::parse(&theme), cx);
            for handle in cx.windows() {
                let _ = handle.update(cx, |_, window, cx| crate::theme::apply(window, cx));
            }
            cx.refresh_windows();
            for state in live_repos(cx) {
                let (model, theme) = (model.clone(), theme.clone());
                state.update(cx, |s, cx| {
                    s.apply_scoped(
                        UiEvent::Settings {
                            model,
                            theme,
                            review_width,
                        },
                        cx,
                    );
                });
            }
        }
        UiEvent::ShowWelcome { recent, error } => show_welcome(recent, error, cx),
        UiEvent::RepoOpened(snapshot) => {
            let repo_id = snapshot.repo.id.clone();
            // Open already: its windows take the fresh snapshot, the front one
            // comes forward.
            let open = windows::of_repo(&repo_id, cx);
            if let Some(front) = open.first() {
                let handle = front.handle;
                for state in repo_states(&repo_id, cx) {
                    let snapshot = (*snapshot).clone();
                    state.update(cx, |s, cx| s.refresh_snapshot(snapshot, cx));
                }
                let _ = handle.update(cx, |_, window, _| window.activate_window());
                return;
            }
            let gh = snapshot.gh.clone();
            // One window per group of slots, each a little down and right of
            // the last.
            let windows = snapshot.windows.clone();
            for (i, window_id) in windows.into_iter().enumerate() {
                let placement = (i > 0).then(|| {
                    let mut b = Bounds::centered(None, size(px(1280.), px(800.)), cx);
                    b.origin += point(px(32. * i as f32), px(32. * i as f32));
                    (b, None)
                });
                open_repo_window((*snapshot).clone(), window_id, placement, Vec::new(), cx);
            }
            if !gh.logged_in
                && let Some(state) = repo(&repo_id, cx)
            {
                let message = if gh.installed {
                    "GitHub is not connected. Run `gh auth login` in a terminal, then reopen the repository."
                } else {
                    "GitHub CLI not found. Install it (`brew install gh`) and run `gh auth login`."
                };
                state.update(cx, |s, cx| {
                    s.toast(crate::backend::ToastLevel::Info, message.to_owned(), cx);
                });
            }
        }
        UiEvent::RepoFailed { path, message } => {
            tracing::warn!(path = %path.display(), %message, "open repo failed");
        }
        UiEvent::Toast {
            repo_id,
            level,
            message,
        } => {
            let targets = match repo_id.and_then(|id| repo(&id, cx)) {
                Some(state) => vec![state],
                None => live_repos(cx),
            };
            if targets.is_empty() {
                tracing::info!(%message, "toast without window");
            }
            for state in targets {
                state.update(cx, |s, cx| s.toast(level, message.clone(), cx));
            }
        }
        UiEvent::History {
            repo_id,
            entries,
            unlisted,
        } => {
            for state in repo_states(&repo_id, cx) {
                let (entries, unlisted) = (entries.clone(), unlisted.clone());
                state.update(cx, |s, cx| s.set_history(entries, unlisted, cx));
            }
        }
        UiEvent::Commands { repo_id, commands } => {
            for state in repo_states(&repo_id, cx) {
                let commands = commands.clone();
                state.update(cx, |s, cx| {
                    s.commands = commands;
                    cx.notify();
                });
            }
        }
        UiEvent::Slots { repo_id, slots } => {
            for state in repo_states(&repo_id, cx) {
                let slots = slots.clone();
                state.update(cx, |s, cx| s.set_slots(slots, cx));
            }
        }
        UiEvent::Workflow { repo_id, view } => {
            for state in repo_states(&repo_id, cx) {
                let view = view.clone();
                state.update(cx, |s, cx| s.set_workflow(view, cx));
            }
        }
        // Asked in the window that shows the asking workflow.
        UiEvent::SlotRequest {
            repo_id,
            workflow_id,
            request,
        } => {
            let shows = |s: &RepoState| {
                s.slots
                    .iter()
                    .any(|e| e.workflow.as_ref().is_some_and(|w| w.id == workflow_id))
            };
            let target = repo_states(&repo_id, cx)
                .into_iter()
                .find(|s| shows(s.read(cx)))
                .or_else(|| repo(&repo_id, cx));
            if let Some(state) = target {
                state.update(cx, |s, cx| s.slot_request(workflow_id, request, cx));
            }
        }
        // A terminal starts in the window that shows its slot.
        UiEvent::TerminalSpec {
            repo_id,
            slot_id,
            result,
        } => {
            let target = repo_states(&repo_id, cx)
                .into_iter()
                .find(|s| s.read(cx).slots.iter().any(|e| e.slot.id == slot_id));
            if let Some(state) = target {
                state.update(cx, |s, cx| s.terminal_spec(slot_id, result, cx));
            }
        }
        UiEvent::Origins {
            repo_id,
            source,
            result,
        } => {
            for state in repo_states(&repo_id, cx) {
                let (source, result) = (source.clone(), result.clone());
                state.update(cx, |s, cx| s.set_origins(source, result, cx));
            }
        }
        // Workflow-scoped events: the window that knows the workflow takes them.
        other => {
            for state in live_repos(cx) {
                state.update(cx, |s, cx| s.apply_scoped(other.clone(), cx));
            }
        }
    }
}

/// Open window `window_id` of the repo: centered, or at `placement` (bounds on
/// a display), with terminals its slots bring from another window.
pub(crate) fn open_repo_window(
    snapshot: crate::backend::RepoSnapshot,
    window_id: String,
    placement: Option<(Bounds<Pixels>, Option<DisplayId>)>,
    terminals: Vec<(String, repo_state::SlotTerminals)>,
    cx: &mut App,
) {
    let repo_id = snapshot.repo.id.clone();
    let title = format!("{} — Berth", snapshot.repo.name());
    let mut options = window_options(size(px(1280.), px(800.)), size(px(900.), px(560.)), cx);
    if let Some(titlebar) = options.titlebar.as_mut() {
        titlebar.title = Some(title.into());
    }
    if let Some((bounds, display)) = placement {
        options.window_bounds = Some(WindowBounds::Windowed(bounds));
        options.display_id = display;
    }
    let backend = Berth::backend(cx);
    let mut created = None;
    let id = window_id.clone();
    let handle = cx.open_window(options, |window, cx| {
        crate::theme::apply(window, cx);
        let state = cx.new(|_| RepoState::new(snapshot, id, backend));
        let view = cx.new(|cx| {
            let mut v = repo_window::RepoWindow::new(state.clone(), window, cx);
            v.adopt_terminals(terminals, window, cx);
            v
        });
        created = Some((state.downgrade(), view.downgrade()));
        cx.new(|cx| Root::new(view, window, cx))
    });
    match (handle, created) {
        (Ok(handle), Some((state, view))) => {
            cx.global_mut::<Berth>()
                .repos
                .entry(repo_id.clone())
                .or_default()
                .push(RepoWindowRef {
                    repo_id,
                    window_id,
                    state,
                    view,
                    handle,
                });
            close_welcome(cx);
        }
        (Err(e), _) => tracing::error!(error = %e, "open repo window"),
        (Ok(_), None) => {}
    }
}

fn show_welcome(recent: Vec<RecentRepo>, error: Option<String>, cx: &mut App) {
    if let Some(handle) = cx.global::<Berth>().welcome {
        let _ = handle.update(cx, |_, window, _| window.activate_window());
        return;
    }
    let options = window_options(size(px(1280.), px(800.)), size(px(900.), px(560.)), cx);
    let handle = cx.open_window(options, |window, cx| {
        crate::theme::apply(window, cx);
        let view = cx.new(|cx| welcome::Welcome::new(recent, error, window, cx));
        cx.new(|cx| Root::new(view, window, cx))
    });
    match handle {
        Ok(h) => cx.global_mut::<Berth>().welcome = Some(h),
        Err(e) => tracing::error!(error = %e, "open welcome window"),
    }
}

fn close_welcome(cx: &mut App) {
    if let Some(handle) = cx.global_mut::<Berth>().welcome.take() {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
}

/// Forget windows that were closed; quit when none remain.
pub fn window_closed(cx: &mut App) {
    let berth = cx.global_mut::<Berth>();
    for windows in berth.repos.values_mut() {
        windows.retain(|w| w.state.upgrade().is_some());
    }
    berth.repos.retain(|_, windows| !windows.is_empty());
    if cx.windows().is_empty() {
        cx.quit();
    }
}
