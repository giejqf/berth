//! GPUI side: one window per repo, fed by backend events.

mod answer_card;
mod composer;
mod details;
mod diff_view;
mod empty_state;
mod files;
mod flow_view;
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
pub mod rich;
mod settings;
mod slot_rail;
mod terminal;
mod welcome;

use std::collections::HashMap;

use futures::StreamExt as _;
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{
    App, AppContext as _, Bounds, Entity, Global, PathPromptOptions, Pixels, Size, WeakEntity,
    WindowBounds, WindowHandle, WindowOptions, point, px, size,
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

/// App-wide state: the backend handle and the open repo windows.
pub struct Berth {
    pub backend: BackendHandle,
    repos: HashMap<String, (WeakEntity<RepoState>, WindowHandle<Root>)>,
    welcome: Option<WindowHandle<Root>>,
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

/// Type into the selected slot's active terminal (devtools).
#[cfg(feature = "devtools")]
pub fn type_in_terminal(text: &str, cx: &mut App) {
    let Some(state) = first_repo_state(cx) else {
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
    cx.global::<Berth>()
        .repos
        .values()
        .find(|(s, _)| s.upgrade().is_some())
        .map(|(_, w)| *w)
}

#[allow(dead_code)]
pub fn welcome_window(cx: &App) -> Option<WindowHandle<Root>> {
    cx.global::<Berth>().welcome
}

fn live_repos(cx: &App) -> Vec<Entity<RepoState>> {
    cx.global::<Berth>()
        .repos
        .values()
        .filter_map(|(state, _)| state.upgrade())
        .collect()
}

fn repo(repo_id: &str, cx: &App) -> Option<Entity<RepoState>> {
    cx.global::<Berth>()
        .repos
        .get(repo_id)
        .and_then(|(s, _)| s.upgrade())
}

fn route(event: UiEvent, cx: &mut App) {
    match event {
        UiEvent::Environment(env) => {
            cx.set_global(EnvGlobal(env));
            cx.refresh_windows();
        }
        UiEvent::Settings { model, theme } => {
            crate::theme::set_appearance(crate::theme::Appearance::parse(&theme), cx);
            for handle in cx.windows() {
                let _ = handle.update(cx, |_, window, cx| crate::theme::apply(window, cx));
            }
            cx.refresh_windows();
            for state in live_repos(cx) {
                let (model, theme) = (model.clone(), theme.clone());
                state.update(cx, |s, cx| {
                    s.apply_scoped(UiEvent::Settings { model, theme }, cx);
                });
            }
        }
        UiEvent::ShowWelcome { recent, error } => show_welcome(recent, error, cx),
        UiEvent::RepoOpened(snapshot) => {
            let repo_id = snapshot.repo.id.clone();
            if let Some(entry) = cx.global::<Berth>().repos.get(&repo_id).cloned()
                && let Some(state) = entry.0.upgrade()
            {
                state.update(cx, |s, cx| s.refresh_snapshot(*snapshot, cx));
                let _ = entry.1.update(cx, |_, window, _| window.activate_window());
                return;
            }
            let gh = snapshot.gh.clone();
            open_repo_window(*snapshot, cx);
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
        UiEvent::Slots { repo_id, slots } => {
            if let Some(state) = repo(&repo_id, cx) {
                state.update(cx, |s, cx| s.set_slots(slots, cx));
            }
        }
        UiEvent::Workflow { repo_id, view } => {
            if let Some(state) = repo(&repo_id, cx) {
                state.update(cx, |s, cx| s.set_workflow(view, cx));
            }
        }
        UiEvent::SlotRequest {
            repo_id,
            workflow_id,
            request,
        } => {
            if let Some(state) = repo(&repo_id, cx) {
                state.update(cx, |s, cx| s.slot_request(workflow_id, request, cx));
            }
        }
        UiEvent::TerminalSpec {
            repo_id,
            slot_id,
            result,
        } => {
            if let Some(state) = repo(&repo_id, cx) {
                state.update(cx, |s, cx| s.terminal_spec(slot_id, result, cx));
            }
        }
        UiEvent::Origins {
            repo_id,
            source,
            result,
        } => {
            if let Some(state) = repo(&repo_id, cx) {
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

fn open_repo_window(snapshot: crate::backend::RepoSnapshot, cx: &mut App) {
    let repo_id = snapshot.repo.id.clone();
    let title = format!("{} — Berth", snapshot.repo.name());
    let mut options = window_options(size(px(1280.), px(800.)), size(px(900.), px(560.)), cx);
    if let Some(titlebar) = options.titlebar.as_mut() {
        titlebar.title = Some(title.into());
    }
    let backend = Berth::backend(cx);
    let mut created: Option<WeakEntity<RepoState>> = None;
    let handle = cx.open_window(options, |window, cx| {
        crate::theme::apply(window, cx);
        let state = cx.new(|_| RepoState::new(snapshot, backend));
        created = Some(state.downgrade());
        let view = cx.new(|cx| repo_window::RepoWindow::new(state, window, cx));
        cx.new(|cx| Root::new(view, window, cx))
    });
    match (handle, created) {
        (Ok(handle), Some(state)) => {
            cx.global_mut::<Berth>()
                .repos
                .insert(repo_id, (state, handle));
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
        let view = cx.new(|_| welcome::Welcome::new(recent, error));
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
    berth
        .repos
        .retain(|_, (state, _)| state.upgrade().is_some());
    if cx.windows().is_empty() {
        cx.quit();
    }
}
