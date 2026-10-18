//! Attachments (user → agent) and artifacts (agent → user). Files are read and
//! written where the agent runs: on the host for `host` placement, through
//! `slot.sh exec` for `inside` (§6.1), so paths mean the same thing to both.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use berth_core::files::{MAX_ATTACHMENT_BYTES, mime_for, safe_file_name, wants_thumbnail};
use berth_core::ids::new_id;
use berth_core::{Engine, Slot, paths};
use berth_protocol::FileRef;
use berth_slot::{ExecOutput, Placement};

use super::Shared;

/// Largest artifact the app copies out of a slot (screen recordings can be big).
const MAX_ARTIFACT_BYTES: u64 = 500 * 1024 * 1024;

/// Writes stdin to `<git dir>/berth-attachments/<id>/<name>` and prints its
/// absolute path. Inside the git dir the file is visible to the agent in both
/// placements and never shows up in snapshots or diffs.
const COPY_INTO_SLOT: &str = r#"set -e
d="$(git rev-parse --git-path berth-attachments)/$1"
mkdir -p "$d"
cat > "$d/$2"
cd "$d" && printf '%s/%s' "$(pwd -P)" "$2""#;

impl Shared {
    /// Copy a user's file into app storage and into the slot.
    pub(super) async fn attach(
        &self,
        slot: &Slot,
        workflow_id: &str,
        source: &Path,
    ) -> Result<FileRef> {
        let meta = tokio::fs::metadata(source)
            .await
            .with_context(|| format!("cannot read {}", source.display()))?;
        if !meta.is_file() {
            return Err(anyhow!("{} is not a file", source.display()));
        }
        if meta.len() > MAX_ATTACHMENT_BYTES {
            return Err(anyhow!(
                "{} is larger than {} MB",
                source.display(),
                MAX_ATTACHMENT_BYTES / 1024 / 1024
            ));
        }
        let bytes = tokio::fs::read(source).await?;
        let id = new_id();
        let name = safe_file_name(&source.to_string_lossy());
        let stored = store_copy(workflow_id, &id, &name, &bytes).await?;

        let out = self
            .run_beside_agent(
                slot,
                "/bin/sh",
                &[
                    "-c".into(),
                    COPY_INTO_SLOT.into(),
                    "sh".into(),
                    id.clone(),
                    name.clone(),
                ],
                Some(bytes),
            )
            .await?;
        if !out.success() {
            return Err(anyhow!(
                "could not copy {name} into the slot: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let slot_path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        let mime = mime_for(&name).to_owned();
        let thumbnail = if wants_thumbnail(&mime) {
            thumbnail(&stored).await
        } else {
            None
        };
        Ok(FileRef {
            id,
            name,
            mime,
            size_bytes: meta.len(),
            slot_path: Some(slot_path),
            stored: Some(stored.to_string_lossy().into_owned()),
            thumbnail: thumbnail.map(|p| p.to_string_lossy().into_owned()),
        })
    }

    /// Run a command where the agent runs, with cwd = the checkout.
    async fn run_beside_agent(
        &self,
        slot: &Slot,
        program: &str,
        args: &[String],
        stdin: Option<Vec<u8>>,
    ) -> Result<ExecOutput> {
        if slot.placement == Placement::Inside.as_str() {
            let script = self.slot_script(slot)?;
            return Ok(script
                .exec(slot.checkout_path.as_deref(), program, args, stdin)
                .await?);
        }
        let checkout = slot
            .checkout_path
            .as_deref()
            .ok_or_else(|| anyhow!("slot {} has no checkout yet", slot.name))?;
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .current_dir(checkout)
            .stdin(if stdin.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("could not run {program}"))?;
        if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
            use tokio::io::AsyncWriteExt as _;
            pipe.write_all(&bytes).await?;
        }
        let out = child.wait_with_output().await?;
        Ok(ExecOutput {
            status: out.status.code().unwrap_or(-1),
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }

    /// Copy an artifact the agent presented out of the slot (`Effect::FetchArtifact`).
    pub(super) async fn fetch_artifact(
        self: Arc<Self>,
        workflow_id: String,
        node_id: String,
        slot_path: String,
        size_bytes: u64,
    ) {
        let result = async {
            if size_bytes > MAX_ARTIFACT_BYTES {
                return Err(anyhow!(
                    "too large to show ({} MB; the limit is {} MB)",
                    size_bytes / 1024 / 1024,
                    MAX_ARTIFACT_BYTES / 1024 / 1024
                ));
            }
            let slot = {
                let core = self.core();
                core.store
                    .slot(&core.store.workflow(&workflow_id)?.slot_id)?
            };
            let out = self
                .run_beside_agent(&slot, "cat", &["--".into(), slot_path.clone()], None)
                .await?;
            if !out.success() {
                return Err(anyhow!(
                    "could not read {slot_path}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            let name = safe_file_name(&slot_path);
            let stored = store_copy(&workflow_id, &new_id(), &name, &out.stdout).await?;
            let thumb = if wants_thumbnail(mime_for(&name)) {
                thumbnail(&stored).await
            } else {
                None
            };
            Ok((
                stored.to_string_lossy().into_owned(),
                thumb.map(|p| p.to_string_lossy().into_owned()),
            ))
        }
        .await
        .map_err(|e| format!("{e:#}"));
        if let Err(e) = &result {
            tracing::warn!(error = %e, slot_path, "artifact fetch failed");
        }
        // Release the core lock before emitting: `emit_workflow` takes it again.
        let saved = Engine::artifact_fetched(&self.core().store, &node_id, result);
        match saved {
            Ok(()) => self.emit_workflow(&workflow_id),
            Err(e) => tracing::error!(error = %e, "artifact bookkeeping failed"),
        }
    }
}

/// Write a pasted image to disk so it can be attached like a dropped file.
pub fn stage_pasted_image(bytes: &[u8], ext: &str) -> Result<PathBuf> {
    let dir = paths::app_support_dir().join("pasted");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!(
        "pasted-{}.{ext}",
        berth_core::ids::short(&new_id())
    ));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

async fn store_copy(workflow_id: &str, id: &str, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let dir = paths::files_dir(workflow_id);
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join(format!("{}-{name}", berth_core::ids::short(id)));
    tokio::fs::write(&path, bytes).await?;
    Ok(path)
}

/// A Quick Look thumbnail (first frame of a video, first page of a PDF, a
/// rendered page for HTML), or `None` if Quick Look has no preview for it.
async fn thumbnail(file: &Path) -> Option<PathBuf> {
    let dir = file.parent()?.join("thumbnails");
    tokio::fs::create_dir_all(&dir).await.ok()?;
    let status = tokio::process::Command::new("/usr/bin/qlmanage")
        .args(["-t", "-s", "800", "-o"])
        .arg(&dir)
        .arg(file)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .ok()?;
    let thumb = dir.join(format!("{}.png", file.file_name()?.to_string_lossy()));
    (status.success() && thumb.exists()).then_some(thumb)
}

/// Show a file in Quick Look: read-only for every type, so attachments and
/// artifacts never open in an editor (§1.1).
pub(super) fn quick_look(path: &Path) -> Result<()> {
    std::process::Command::new("/usr/bin/qlmanage")
        .arg("-p")
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .with_context(|| format!("could not preview {}", path.display()))
}
