//! Diff presets (AGENTS.md §7): resolve two tree snapshots and diff them.

use anyhow::{Result, anyhow};
use berth_core::SnapshotKind;
use berth_protocol::DiffPreset;

use super::{DiffRequest, DiffResult, Shared};

impl Shared {
    pub(super) async fn load_diff(&self, req: DiffRequest) -> Result<DiffResult> {
        let (view, slot) = {
            let core = self.core();
            let view = core.store.workflow_view(&req.workflow_id)?;
            let slot = core.store.slot(&view.workflow.slot_id)?;
            (view, slot)
        };
        if slot.checkout_path.is_none() {
            return Err(anyhow!("the slot has no checkout yet"));
        }
        let script = self.slot_script(&slot)?;
        // Sections that exist and have work in them (a closed workflow's trailing
        // start snapshot opens no real section).
        let mut sections: Vec<u32> = view
            .snapshots
            .iter()
            .filter(|s| s.kind == SnapshotKind::Start)
            .map(|s| s.section_no)
            .filter(|n| {
                view.rounds
                    .iter()
                    .any(|r| r.section_no == *n && r.is_work())
                    || view.snapshot(*n, SnapshotKind::Summary).is_some()
            })
            .collect();
        sections.sort_unstable();
        sections.dedup();

        let mut section = None;
        let (from, to): (String, Option<String>) = match req.preset {
            DiffPreset::Section => {
                // Default: the open section if work happened in it, else the last summarized one.
                let open = view.workflow.section_no;
                let open_has_rounds = view
                    .rounds
                    .iter()
                    .any(|r| r.section_no == open && r.is_work());
                let n = req.section.unwrap_or(if open_has_rounds || open == 1 {
                    open
                } else {
                    open - 1
                });
                section = Some(n);
                let start = view
                    .snapshot(n, SnapshotKind::Start)
                    .ok_or_else(|| anyhow!("section {n} has no start snapshot yet"))?;
                (
                    start.tree_sha.clone(),
                    view.snapshot(n, SnapshotKind::Summary)
                        .map(|s| s.tree_sha.clone()),
                )
            }
            DiffPreset::Workflow => {
                let start = view
                    .snapshot(1, SnapshotKind::Start)
                    .ok_or_else(|| anyhow!("the workflow has no start snapshot yet"))?;
                (start.tree_sha.clone(), None)
            }
            DiffPreset::Branch => {
                let base = berth_git::merge_base(&script, &slot.base_ref, "HEAD").await?;
                (base, None)
            }
            DiffPreset::Custom => {
                let (a, b) = req
                    .custom
                    .clone()
                    .ok_or_else(|| anyhow!("custom diff needs two refs"))?;
                (a, Some(b))
            }
        };
        let to = match to {
            Some(t) => t,
            None => self.snapshot_slot(&slot).await?,
        };
        let patch = berth_git::diff_patch(&script, &from, &to).await?;
        let parsed = berth_git::parse_diff(&patch)?;
        Ok(DiffResult {
            request: req,
            section,
            from,
            to,
            stats: parsed.stats(),
            patch,
            sections,
        })
    }
}
