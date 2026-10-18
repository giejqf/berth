//! Where the sidecar lives and how to start it (AGENTS.md §3, §4).

use std::path::{Path, PathBuf};

/// How to run the sidecar on the host, plus the bundle dir handed to slots.
#[derive(Debug, Clone)]
pub struct SidecarLaunch {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// `BERTH_SIDECAR_BUNDLE`: directory containing `sidecar.mjs`.
    pub bundle_dir: PathBuf,
}

impl SidecarLaunch {
    /// Resolution order: `$BERTH_SIDECAR` (a command line), the app bundle's
    /// `Contents/Resources/sidecar`, the workspace's built `sidecar/dist`, and
    /// finally the workspace sources via `tsx` (dev).
    #[must_use]
    pub fn resolve() -> Self {
        let node = which("node").unwrap_or_else(|| PathBuf::from("node"));
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../sidecar");
        let bundled = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|d| d.join("../Resources/sidecar")))
            .filter(|d| d.join("sidecar.mjs").exists());
        let dist = workspace.join("dist");
        let bundle_dir = bundled.clone().unwrap_or_else(|| dist.clone());

        if let Ok(cmd) = std::env::var("BERTH_SIDECAR") {
            let mut parts = cmd.split_whitespace().map(str::to_owned);
            if let Some(program) = parts.next() {
                return Self {
                    program: program.into(),
                    args: parts.collect(),
                    bundle_dir,
                };
            }
        }
        if let Some(dir) = bundled {
            return Self {
                program: node,
                args: vec![path_str(&dir.join("sidecar.mjs"))],
                bundle_dir: dir,
            };
        }
        let tsx = workspace.join("node_modules/.bin/tsx");
        if tsx.exists() && std::env::var_os("BERTH_SIDECAR_DIST").is_none() {
            return Self {
                program: tsx,
                args: vec![path_str(&workspace.join("src/index.ts"))],
                bundle_dir,
            };
        }
        Self {
            program: node,
            args: vec![path_str(&dist.join("sidecar.mjs"))],
            bundle_dir,
        }
    }

    /// `node <bundle>/sidecar.mjs` as run inside a slot (`inside` placement).
    #[must_use]
    pub fn inside_args(&self) -> (String, Vec<String>) {
        (
            "node".to_owned(),
            vec![path_str(&self.bundle_dir.join("sidecar.mjs"))],
        )
    }
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Find `name` on `$PATH`.
#[must_use]
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}
