//! Terminals (AGENTS.md §11): what a slot's shell runs, where, and with which
//! environment. The UI runs it under a PTY (`ui/terminal`).

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use berth_slot::Placement;

use super::Shared;
use super::types::TerminalSpec;

/// Inside the environment: bash if there is one, else sh, as a login shell.
const INSIDE_SHELL: &str =
    "if command -v bash >/dev/null 2>&1; then exec bash -l; else exec sh -l; fi";

impl Shared {
    /// A shell in the slot's checkout, seeing what the agent sees: the slot's
    /// `BERTH_*` variables, connector secrets and (on the host) connector CLIs.
    pub(super) fn terminal_spec(&self, slot_id: &str) -> Result<TerminalSpec> {
        let slot = self.core().store.slot(slot_id)?;
        let checkout = slot
            .checkout_path
            .clone()
            .ok_or_else(|| anyhow!("the slot has no checkout yet"))?;
        let script = self.slot_script(&slot)?;
        let mut env = script.env.vars();
        for (k, v) in [
            ("TERM", "xterm-256color"),
            ("COLORTERM", "truecolor"),
            ("TERM_PROGRAM", "Berth"),
            ("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION")),
        ] {
            env.push((k.to_owned(), v.to_owned()));
        }
        // Apps started from Finder have no locale; shells then mangle UTF-8.
        if std::env::var_os("LANG").is_none() {
            env.push(("LANG".to_owned(), "en_US.UTF-8".to_owned()));
        }

        if slot.placement == Placement::Inside.as_str() {
            // The checkout is inside the environment: `exec` gets us there.
            script.ty.ensure_executable()?;
            return Ok(TerminalSpec {
                program: script.ty.script().to_string_lossy().into_owned(),
                args: ["exec", "--", "/bin/sh", "-c", INSIDE_SHELL]
                    .map(str::to_owned)
                    .to_vec(),
                cwd: script.ty.dir.clone(),
                env,
                title: slot.slot_type.clone(),
            });
        }

        let base = std::env::var("PATH").unwrap_or_default();
        env.push((
            "PATH".to_owned(),
            berth_connectors::path_with_connectors(&self.enabled_connectors(), &base),
        ));
        env.push((
            "BERTH_SLOT_SCRIPT".to_owned(),
            script.ty.script().to_string_lossy().into_owned(),
        ));
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/bin/zsh".to_owned());
        let title = Path::new(&shell)
            .file_name()
            .map_or_else(|| shell.clone(), |n| n.to_string_lossy().into_owned());
        Ok(TerminalSpec {
            program: shell,
            args: vec!["-l".to_owned()],
            cwd: PathBuf::from(checkout),
            env,
            title,
        })
    }
}
