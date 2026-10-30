//! The context window and the plan's limits (ADR 0014): what the composer's
//! usage ring shows. Sessions report them (`session.context`,
//! `session.quota`); the rules for reading them are here.

use berth_protocol::{ContextCategory, ContextKind, QuotaWindow, SessionQuota};
use serde::{Deserialize, Serialize};

/// The key of the plan's extra-usage spend: money, not a limit, so it never
/// counts as the tightest window.
pub const EXTRA_USAGE: &str = "extra_usage";

/// A workflow's context window as its session last reported it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUsage {
    pub categories: Vec<ContextCategory>,
    /// The window as Claude Code reported it, when it did (ADR 0025).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// When it was measured (unix ms).
    pub at: i64,
}

impl ContextUsage {
    fn sum(&self, kind: ContextKind) -> u64 {
        self.categories
            .iter()
            .filter(|c| c.kind == kind)
            .map(|c| c.tokens)
            .sum()
    }

    /// Tokens the conversation, prompts and tools take up.
    #[must_use]
    pub fn used(&self) -> u64 {
        self.sum(ContextKind::Used)
    }

    /// Tokens left before Claude Code compacts the conversation.
    #[must_use]
    pub fn free(&self) -> u64 {
        self.sum(ContextKind::Free)
    }

    /// The reserve kept for compaction (none when auto-compact is off).
    #[must_use]
    pub fn buffer(&self) -> u64 {
        self.sum(ContextKind::Buffer)
    }

    /// The whole window: as Claude Code reported it, else used, free and
    /// the compaction reserve (which add up to more than a small window).
    #[must_use]
    pub fn window(&self) -> u64 {
        self.max_tokens
            .filter(|&m| m > 0)
            .unwrap_or_else(|| self.used() + self.free() + self.buffer())
    }

    /// The share of the window in use, 0–1.
    #[must_use]
    pub fn fraction(&self) -> f32 {
        let window = self.window();
        if window == 0 {
            return 0.;
        }
        #[allow(clippy::cast_precision_loss)]
        let f = self.used() as f32 / window as f32;
        f.clamp(0., 1.)
    }

    /// The parts in use, largest first, without empty ones.
    #[must_use]
    pub fn parts(&self) -> Vec<&ContextCategory> {
        let mut parts: Vec<_> = self
            .categories
            .iter()
            .filter(|c| c.kind == ContextKind::Used && c.tokens > 0)
            .collect();
        parts.sort_by_key(|c| std::cmp::Reverse(c.tokens));
        parts
    }
}

/// The account's plan limits as last reported, by any session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Quota {
    /// Plan limits apply (a claude.ai subscription).
    pub available: bool,
    pub plan: Option<String>,
    pub windows: Vec<QuotaWindow>,
    /// When it was last updated (unix ms).
    pub at: i64,
}

impl Quota {
    /// Fold a report into what was known: a complete one replaces it, a
    /// partial one (a rate-limit event) updates the windows it names and
    /// keeps the rest.
    #[must_use]
    pub fn apply(known: Option<&Quota>, report: &SessionQuota, now: i64) -> Quota {
        let mut quota = match known {
            Some(q) if !report.complete => q.clone(),
            _ => Quota {
                available: report.available,
                plan: None,
                windows: Vec::new(),
                at: now,
            },
        };
        quota.at = now;
        if report.complete {
            quota.available = report.available;
            quota.plan.clone_from(&report.plan);
            quota.windows.clone_from(&report.windows);
            return quota;
        }
        // A rate-limit event only comes where plan limits apply.
        quota.available = true;
        for w in &report.windows {
            match quota.windows.iter_mut().find(|k| k.key == w.key) {
                Some(k) => {
                    if w.label.is_some() {
                        k.label.clone_from(&w.label);
                    }
                    if w.utilization.is_some() {
                        k.utilization = w.utilization;
                    }
                    if w.resets_at.is_some() {
                        k.resets_at = w.resets_at;
                    }
                    if w.status.is_some() {
                        k.status = w.status;
                    }
                }
                None => quota.windows.push(w.clone()),
            }
        }
        quota
    }

    /// The limit windows (not the extra-usage spend) as they stand at `now`.
    pub fn limits(&self, now: i64) -> impl Iterator<Item = (&QuotaWindow, Option<f64>)> {
        self.windows
            .iter()
            .filter(|w| w.key != EXTRA_USAGE)
            .map(move |w| (w, used_now(w, now)))
    }

    /// The window closest to its limit at `now`, with its use (percent):
    /// what the ring's inner arc shows.
    #[must_use]
    pub fn tightest(&self, now: i64) -> Option<(&QuotaWindow, f64)> {
        self.limits(now)
            .filter_map(|(w, used)| used.map(|u| (w, u)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
    }
}

/// A window's use at `now`, in percent: none known, or nothing once it has
/// reset since it was reported.
#[must_use]
pub fn used_now(w: &QuotaWindow, now: i64) -> Option<f64> {
    if w.resets_at.is_some_and(|at| at <= now) {
        return Some(0.);
    }
    w.utilization.map(|u| u.clamp(0., 100.))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use berth_protocol::QuotaStatus;

    use super::*;

    fn cat(name: &str, tokens: u64, kind: ContextKind) -> ContextCategory {
        ContextCategory {
            name: name.into(),
            tokens,
            kind,
        }
    }

    fn window(key: &str, utilization: Option<f64>, resets_at: Option<i64>) -> QuotaWindow {
        QuotaWindow {
            key: key.into(),
            label: None,
            utilization,
            resets_at,
            status: None,
        }
    }

    #[test]
    fn context_counts_the_window_without_deferred_tools() {
        let c = ContextUsage {
            categories: vec![
                cat("System prompt", 3_000, ContextKind::Used),
                cat("Messages", 47_000, ContextKind::Used),
                cat("MCP tools (deferred)", 9_000, ContextKind::Deferred),
                cat("Skills", 0, ContextKind::Used),
                cat("Free space", 117_000, ContextKind::Free),
                cat("Autocompact buffer", 33_000, ContextKind::Buffer),
            ],
            max_tokens: None,
            at: 0,
        };
        assert_eq!((c.used(), c.window()), (50_000, 200_000));
        assert!((c.fraction() - 0.25).abs() < 1e-6);
        let names: Vec<_> = c.parts().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Messages", "System prompt"]);
        let empty = ContextUsage {
            categories: vec![],
            max_tokens: None,
            at: 0,
        };
        assert!(empty.fraction().abs() < f32::EPSILON);
        // A small window, set for another provider's model: its parts and
        // reserve add up to more than it, and the window is what counts.
        let small = ContextUsage {
            categories: vec![
                cat("System tools", 41_600, ContextKind::Used),
                cat("Free space", 0, ContextKind::Free),
                cat("Autocompact buffer", 33_000, ContextKind::Buffer),
            ],
            max_tokens: Some(48_000),
            at: 0,
        };
        assert_eq!(small.window(), 48_000);
        assert!((small.fraction() - 0.866_666_7).abs() < 1e-3);
    }

    #[test]
    fn reports_replace_and_events_update() {
        let full = SessionQuota {
            complete: true,
            available: true,
            plan: Some("max".into()),
            windows: vec![
                window("five_hour", Some(20.), Some(5_000)),
                window("seven_day", Some(60.), Some(90_000)),
                window(EXTRA_USAGE, Some(95.), None),
            ],
        };
        let q = Quota::apply(None, &full, 1_000);
        // Extra usage is spend, not a limit.
        let (tight, used) = q.tightest(1_000).unwrap();
        assert_eq!((tight.key.as_str(), used), ("seven_day", 60.));

        // An event moves one window and keeps the rest.
        let event = SessionQuota {
            complete: false,
            available: true,
            plan: None,
            windows: vec![QuotaWindow {
                status: Some(QuotaStatus::Warning),
                ..window("five_hour", Some(85.), None)
            }],
        };
        let q = Quota::apply(Some(&q), &event, 2_000);
        assert_eq!(q.plan.as_deref(), Some("max"));
        assert_eq!(q.windows.len(), 3);
        let five = &q.windows[0];
        assert_eq!((five.utilization, five.resets_at), (Some(85.), Some(5_000)));
        assert_eq!(five.status, Some(QuotaStatus::Warning));
        assert_eq!(q.tightest(2_000).unwrap().0.key, "five_hour");
        // Once it resets, it is empty again.
        assert_eq!(q.tightest(6_000).unwrap().0.key, "seven_day");

        // A complete report without limits (an API key) replaces all of it.
        let none = SessionQuota {
            complete: true,
            available: false,
            plan: None,
            windows: vec![],
        };
        let q = Quota::apply(Some(&q), &none, 3_000);
        assert!(!q.available && q.windows.is_empty() && q.tightest(3_000).is_none());
    }
}
