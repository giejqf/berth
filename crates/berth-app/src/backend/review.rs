//! Diff presets (AGENTS.md §7): resolve two tree snapshots and diff them.

use std::collections::VecDeque;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use berth_core::SnapshotKind;
use berth_git::Diff;
use berth_protocol::DiffPreset;

use super::{DiffRequest, DiffResult, Shared, lock};

/// The diffs computed last, by the two objects they compare: a pair of object
/// ids always diffs the same (a ref name can move, so it isn't kept). A live
/// tree is snapshotted again each time, and diffs from the cache while it is
/// the same tree.
#[derive(Default)]
pub(super) struct DiffCache(VecDeque<((String, String), Arc<Diff>)>);

impl DiffCache {
    const KEEP: usize = 24;

    fn get(&mut self, from: &str, to: &str) -> Option<Arc<Diff>> {
        let at = self.0.iter().position(|((a, b), _)| a == from && b == to)?;
        let entry = self.0.remove(at)?;
        let diff = entry.1.clone();
        self.0.push_back(entry);
        Some(diff)
    }

    fn put(&mut self, from: &str, to: &str, diff: &Arc<Diff>) {
        if !is_object_id(from) || !is_object_id(to) {
            return;
        }
        self.0.retain(|((a, b), _)| a != from || b != to);
        self.0
            .push_back(((from.to_owned(), to.to_owned()), diff.clone()));
        while self.0.len() > Self::KEEP {
            self.0.pop_front();
        }
    }
}

/// A full SHA-1 or SHA-256 object id, not a name that can move.
fn is_object_id(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

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
        let cached = lock(&self.diff_cache).get(&from, &to);
        let diff = match cached {
            Some(diff) => diff,
            None => {
                let patch = berth_git::diff_patch(&script, &from, &to).await?;
                let diff = Arc::new(berth_git::parse_diff(&patch)?);
                lock(&self.diff_cache).put(&from, &to, &diff);
                diff
            }
        };
        Ok(DiffResult {
            request: req,
            section,
            from,
            to,
            stats: diff.stats(),
            diff,
            sections,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(c: char) -> String {
        c.to_string().repeat(40)
    }

    #[test]
    fn diffs_are_kept_by_object_ids_newest_last() {
        let mut cache = DiffCache::default();
        let diff = Arc::new(Diff::default());
        cache.put(&id('a'), &id('b'), &diff);
        assert!(cache.get(&id('a'), &id('b')).is_some());
        assert!(cache.get(&id('b'), &id('a')).is_none());
        // A ref name can move: never kept.
        cache.put("main", &id('b'), &diff);
        assert!(cache.get("main", &id('b')).is_none());
        // The least recently used goes first: reading one keeps it.
        let nth = |i: usize| format!("{i:040}");
        cache.put(&id('c'), &nth(0), &diff);
        cache.get(&id('a'), &id('b'));
        for i in 1..DiffCache::KEEP {
            cache.put(&id('c'), &nth(i), &diff);
        }
        assert_eq!(cache.0.len(), DiffCache::KEEP);
        assert!(cache.get(&id('c'), &nth(0)).is_none());
        assert!(cache.get(&id('a'), &id('b')).is_some());
        assert!(cache.get(&id('c'), &nth(DiffCache::KEEP - 1)).is_some());
    }
}
