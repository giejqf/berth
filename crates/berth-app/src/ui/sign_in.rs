//! "Sign in to Claude Code" and "Switch Claude account" (ADR 0030): Berth
//! opens Claude Code's own login in Terminal (`claude "/login"`), where the
//! user picks the account, and offers no sign-in of its own. Berth sees the
//! result when one of its windows comes back to the front.

use crate::backend::AgentFix;

/// Where to get Node.js when agents can't run without it.
pub const NODE_URL: &str = "https://nodejs.org/en/download";

/// The button that fixes why agents can't run.
pub fn fix_label(fix: AgentFix) -> &'static str {
    match fix {
        AgentFix::SignIn => "Sign in…",
        AgentFix::GetNode => "Get Node.js",
        AgentFix::SetUpProvider => "Settings…",
    }
}
