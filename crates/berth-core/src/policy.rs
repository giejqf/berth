//! What Settings lets agents do beyond their own slot (ADR 0032): message
//! other slots' agents, and run subagents, how many at once.

/// Messages between slots' agents (`message_slot`, ADR 0021), as Settings
/// allows them (`settings.slot_messages`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MessagePolicy {
    /// Every message reaches the other agent, however many in a row.
    Always,
    /// The loop guard: a message and its answer reach the other agent; the
    /// next one in a row waits for the user (`messages::MAX_HOPS`).
    #[default]
    Limited,
    /// Agents can't message other slots: no `message_slot`, and the session
    /// context doesn't list the other slots.
    Off,
}

impl MessagePolicy {
    pub const ALL: [Self; 3] = [Self::Always, Self::Limited, Self::Off];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Limited => "limited",
            Self::Off => "off",
        }
    }

    /// The setting's value; anything else (or nothing) is the default.
    #[must_use]
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("always") => Self::Always,
            Some("off") => Self::Off,
            _ => Self::Limited,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Always => "Always allow",
            Self::Limited => "Limited",
            Self::Off => "Disabled",
        }
    }

    /// What Settings says of it.
    #[must_use]
    pub fn explain(self) -> &'static str {
        match self {
            Self::Always => "Every message reaches the other agent, without you.",
            Self::Limited => {
                "A message and its answer reach the other agent; the next one in a row waits for you."
            }
            Self::Off => "Agents can't message other slots.",
        }
    }

    /// Whether agents get `message_slot`.
    #[must_use]
    pub fn allows(self) -> bool {
        self != Self::Off
    }
}

/// What the agent reads when it messages another slot with messages off
/// (a session that started before they were turned off).
pub const MESSAGES_OFF: &str = "Messages between slots are turned off in Berth's settings, so \
     nothing was sent. If the other slot's work needs to know this, tell the user (in your summary \
     or reply).";

/// How many subagents an agent may run at once (`settings.max_subagents`):
/// `None`, as many as Claude Code runs; `Some(0)`, none.
pub const SUBAGENT_CHOICES: [Option<u32>; 6] = [Some(0), Some(1), Some(2), Some(3), Some(5), None];

/// The setting's value: a number, else no limit.
#[must_use]
pub fn parse_max_subagents(value: Option<&str>) -> Option<u32> {
    value.and_then(|v| v.trim().parse().ok())
}

/// What the setting stores for `max`.
#[must_use]
pub fn max_subagents_value(max: Option<u32>) -> String {
    max.map_or_else(|| "none".to_owned(), |n| n.to_string())
}

/// The choice's name in Settings.
#[must_use]
pub fn max_subagents_label(max: Option<u32>) -> String {
    match max {
        Some(0) => "Off".to_owned(),
        Some(n) => n.to_string(),
        None => "No limit".to_owned(),
    }
}

/// What Settings says of it.
#[must_use]
pub fn explain_max_subagents(max: Option<u32>) -> String {
    match max {
        Some(0) => "Agents work alone, without subagents.".to_owned(),
        Some(1) => "An agent runs one subagent at a time.".to_owned(),
        Some(n) => format!("An agent runs up to {n} subagents at a time."),
        None => "Claude Code decides how many subagents run at once.".to_owned(),
    }
}

/// Both, as the backend reads them from the settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AgentLimits {
    pub messages: MessagePolicy,
    pub max_subagents: Option<u32>,
}

impl AgentLimits {
    /// From `settings.slot_messages` and `settings.max_subagents`.
    #[must_use]
    pub fn from_settings(messages: Option<&str>, max_subagents: Option<&str>) -> Self {
        Self {
            messages: MessagePolicy::parse(messages),
            max_subagents: parse_max_subagents(max_subagents),
        }
    }

    /// Whether a live session must restart to follow `next`: what its agent
    /// was given at the start (the `message_slot` tool, the subagent limit)
    /// changed. Always allow and Limited differ only in the app.
    #[must_use]
    pub fn session_differs(self, next: Self) -> bool {
        self.messages.allows() != next.messages.allows() || self.max_subagents != next.max_subagents
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        for p in MessagePolicy::ALL {
            assert_eq!(MessagePolicy::parse(Some(p.as_str())), p);
        }
        assert_eq!(MessagePolicy::parse(None), MessagePolicy::Limited);
        assert_eq!(MessagePolicy::parse(Some("bogus")), MessagePolicy::Limited);
        for max in SUBAGENT_CHOICES {
            assert_eq!(
                parse_max_subagents(Some(&max_subagents_value(max))),
                max,
                "{max:?}"
            );
        }
        assert_eq!(parse_max_subagents(None), None);
    }

    #[test]
    fn a_session_restarts_only_for_what_its_agent_was_given() {
        let limited = AgentLimits::default();
        let always = AgentLimits {
            messages: MessagePolicy::Always,
            ..limited
        };
        let off = AgentLimits {
            messages: MessagePolicy::Off,
            ..limited
        };
        let two = AgentLimits {
            max_subagents: Some(2),
            ..limited
        };
        assert!(!limited.session_differs(always));
        assert!(limited.session_differs(off));
        assert!(off.session_differs(always));
        assert!(limited.session_differs(two));
        assert!(!two.session_differs(two));
    }
}
