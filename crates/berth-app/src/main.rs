//! Berth: agentic coding where the human never touches code.

mod assets;
mod backend;
#[cfg(feature = "devtools")]
mod devtools;
mod keymap;
#[cfg(feature = "devtools")]
mod seed;
mod shell_env;
mod theme;
mod ui;

use gpui::{App, Menu, MenuItem};

fn main() {
    shell_env::import_login_shell_env();
    disable_font_smoothing();
    let _log_guard = init_logging();

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .thread_name("berth-tokio")
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("berth: failed to start runtime: {e}");
            std::process::exit(1);
        }
    };
    // Devtools: seed the design scenario before the backend opens the store.
    #[cfg(feature = "devtools")]
    let seeded: Option<std::path::PathBuf> =
        std::env::var("BERTH_DEV_SEED").ok().and_then(|scenario| {
            seed::seed(&scenario)
                .map_err(|e| eprintln!("berth: seed failed: {e:#}"))
                .ok()
                .flatten()
        });
    #[cfg(not(feature = "devtools"))]
    let seeded: Option<std::path::PathBuf> = None;
    let (ui_tx, ui_rx) = futures::channel::mpsc::unbounded();
    let backend = match backend::start(runtime.handle(), ui_tx) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("berth: {e:#}");
            std::process::exit(1);
        }
    };

    gpui_platform::application()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            if let Err(e) = cx.text_system().add_fonts(assets::fonts()) {
                tracing::error!(error = %e, "could not load the bundled fonts");
            }
            keymap::init(cx);
            register_actions(cx);
            cx.set_menus([
                Menu::new("Berth").items([
                    MenuItem::action("Settings…", keymap::OpenSettings),
                    MenuItem::separator(),
                    MenuItem::action("Quit Berth", keymap::Quit),
                ]),
                Menu::new("File").items([
                    MenuItem::action("Open Repository…", keymap::OpenRepository),
                    MenuItem::action("New Workflow…", keymap::NewWorkflow),
                    MenuItem::separator(),
                    MenuItem::action("Close Window", keymap::CloseWindow),
                ]),
                Menu::new("Workflow").items([
                    MenuItem::action("Command Palette…", keymap::CommandPalette),
                    MenuItem::separator(),
                    MenuItem::action("Focus Composer", keymap::FocusComposer),
                    MenuItem::action("Attach Files…", keymap::AttachFiles),
                    MenuItem::action("Interrupt Agent", keymap::Interrupt),
                    MenuItem::action("Follow Up from Outcome", keymap::FollowUpFromOutcome),
                    MenuItem::separator(),
                    MenuItem::action("Next Slot", keymap::NextSlot),
                    MenuItem::action("Previous Slot", keymap::PrevSlot),
                    MenuItem::action("Slot That Needs You", keymap::JumpToNeeding),
                    MenuItem::action("Collapse Rail", keymap::ToggleRail),
                    MenuItem::separator(),
                    MenuItem::action("Destroy Slot…", keymap::DestroySlot),
                ]),
                Menu::new("Review").items([
                    MenuItem::action("Toggle Review Pane", keymap::ToggleReview),
                    MenuItem::action("Section Diff", keymap::ReviewSection),
                    MenuItem::action("Workflow Diff", keymap::ReviewWorkflow),
                    MenuItem::action("Branch Diff", keymap::ReviewBranch),
                    MenuItem::separator(),
                    MenuItem::action("Split / Unified", keymap::ToggleDiffMode),
                    MenuItem::action("Next Change", keymap::NextChange),
                    MenuItem::action("Previous Change", keymap::PrevChange),
                    MenuItem::action("Next Section", keymap::NextSection),
                    MenuItem::action("Previous Section", keymap::PrevSection),
                    MenuItem::action("Post Comments to PR", keymap::PostToPr),
                    MenuItem::action("Refresh", keymap::RefreshReview),
                ]),
            ]);
            ui::init(backend.clone(), ui_rx, cx);
            backend.send(backend::Command::LoadSettings);
            backend.send(backend::Command::CheckEnvironment);
            match seeded
                .clone()
                .or_else(|| std::env::args().nth(1).map(Into::into))
            {
                Some(path) => backend.send(backend::Command::OpenRepo { path }),
                None => backend.send(backend::Command::OpenLastRepo),
            }
            cx.on_window_closed(|cx, _| ui::window_closed(cx)).detach();
            #[cfg(feature = "devtools")]
            devtools::start(cx);
            cx.activate(true);
        });
    drop(runtime);
}

/// The design's type is antialiased without CoreGraphics' stroke dilation
/// (CSS `-webkit-font-smoothing: antialiased`). GPUI reads that from the app's
/// own `AppleFontSmoothing` default, so set it in Berth's domain.
fn disable_font_smoothing() {
    let exe = std::env::current_exe().ok();
    let bundled = exe.as_ref().and_then(|e| {
        e.parent()?
            .parent()?
            .join("Info.plist")
            .exists()
            .then_some(())
    });
    let domain = match (bundled, &exe) {
        (Some(()), _) => "com.berth.app".to_owned(),
        (None, Some(e)) => e
            .file_name()
            .map_or_else(|| "berth".to_owned(), |n| n.to_string_lossy().into_owned()),
        (None, None) => return,
    };
    let current = std::process::Command::new("/usr/bin/defaults")
        .args(["read", &domain, "AppleFontSmoothing"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    if current.as_deref() != Some("0") {
        let _ = std::process::Command::new("/usr/bin/defaults")
            .args(["write", &domain, "AppleFontSmoothing", "-int", "0"])
            .status();
    }
}

fn register_actions(cx: &mut App) {
    cx.on_action(|_: &keymap::Quit, cx| cx.quit());
    cx.on_action(|_: &keymap::OpenRepository, cx| ui::prompt_open_repository(cx));
}

fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};
    let dir = berth_core::paths::logs_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let appender = tracing_appender::rolling::daily(&dir, "berth.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_env("BERTH_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let stderr =
        std::env::var_os("BERTH_LOG_STDERR").map(|_| fmt::layer().with_writer(std::io::stderr));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_ansi(false).with_writer(writer))
        .with(stderr)
        .init();
    Some(guard)
}
