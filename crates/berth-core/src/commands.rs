//! Slash commands (ADR 0015): which prompts are commands, which commands
//! Berth does not send (it does their job itself, or they need Claude Code's
//! own terminal), and how the composer completes them.

use berth_protocol::CommandInfo;
use serde::{Deserialize, Serialize};

/// The slash command `text` starts with, without the slash: `compact` for
/// "/compact keep the API notes". A path is not a command ("/Users/me/a.rs
/// is broken"): a command's name has no further slash.
#[must_use]
pub fn slash_command(text: &str) -> Option<&str> {
    let first = text.split_whitespace().next()?;
    let name = first.strip_prefix('/')?;
    let mut chars = name.chars();
    let starts = chars.next().is_some_and(|c| c.is_ascii_alphanumeric());
    let rest = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'));
    (starts && rest).then_some(name)
}

/// Why Berth does not send `/name`: what does its job here instead. `None`
/// when it goes to Claude Code. `terminal` lists the commands bound to Claude
/// Code's own terminal (the session's `terminal_slash_commands`).
#[must_use]
pub fn blocked(name: &str, terminal: &[String]) -> Option<&'static str> {
    Some(match name {
        "model" | "effort" | "fast" => {
            "Berth sets the model and effort per slot: use the model chip (⌘⇧M)."
        }
        "rewind" => "Use Rewind conversation (R in the flow), so the flow rewinds with it.",
        "resume" | "continue" => "Use Open from History (⌘Y).",
        "clear" | "reset" | "new" => "For a fresh conversation, start a new workflow here (⌥⌘N).",
        "exit" | "quit" => "The session ends with its workflow; there is nothing to exit.",
        "login" | "logout" => "Sign in to Claude Code in a terminal: claude /login.",
        _ if terminal.iter().any(|t| t == name) => {
            "This command works only in Claude Code's own terminal."
        }
        _ => return None,
    })
}

/// The commands a repo's sessions offer, as the last one reported them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commands {
    pub commands: Vec<CommandInfo>,
    pub terminal: Vec<String>,
}

impl Commands {
    /// The command `name` is, by its name or one of its aliases.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&CommandInfo> {
        self.commands.iter().find(|c| c.name == name).or_else(|| {
            self.commands
                .iter()
                .find(|c| c.aliases.iter().any(|a| a == name))
        })
    }

    /// Whether `text` runs a command: it starts with `/name` and the session
    /// offers that name (or, before any session reported its list, any
    /// `/name`). "/tmp is full" is prose once the list is known.
    #[must_use]
    pub fn is_command(&self, text: &str) -> bool {
        slash_command(text)
            .is_some_and(|name| self.commands.is_empty() || self.find(name).is_some())
    }

    /// Why `/name` is not sent, resolving an alias to its command first.
    #[must_use]
    pub fn blocked(&self, name: &str) -> Option<&'static str> {
        blocked(name, &self.terminal).or_else(|| {
            let c = self.find(name)?;
            blocked(&c.name, &self.terminal)
        })
    }

    /// The commands for what is typed after the slash, best first: names
    /// that start with it, then aliases that do, then names containing it.
    /// Hidden ones (`__name`) are left out.
    #[must_use]
    pub fn completions(&self, typed: &str) -> Vec<&CommandInfo> {
        let typed = typed.to_ascii_lowercase();
        let rank = |c: &CommandInfo| -> Option<u8> {
            let name = c.name.to_ascii_lowercase();
            if name.starts_with(&typed) {
                Some(0)
            } else if c
                .aliases
                .iter()
                .any(|a| a.to_ascii_lowercase().starts_with(&typed))
            {
                Some(1)
            } else if name.contains(&typed) {
                Some(2)
            } else {
                None
            }
        };
        let mut found: Vec<(u8, &CommandInfo)> = self
            .commands
            .iter()
            .filter(|c| !c.name.starts_with("__"))
            .filter_map(|c| rank(c).map(|r| (r, c)))
            .collect();
        found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
        found.into_iter().map(|(_, c)| c).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(name: &str, aliases: &[&str]) -> CommandInfo {
        CommandInfo {
            name: name.into(),
            description: String::new(),
            argument_hint: String::new(),
            aliases: aliases.iter().map(|&a| a.to_owned()).collect(),
            builtin: true,
        }
    }

    #[test]
    fn commands_are_told_from_paths() {
        assert_eq!(slash_command("/compact"), Some("compact"));
        assert_eq!(slash_command("  /compact keep notes"), Some("compact"));
        assert_eq!(
            slash_command("/anthropic-skills:pdf a.pdf"),
            Some("anthropic-skills:pdf")
        );
        assert_eq!(slash_command("/Users/me/a.rs is broken"), None);
        assert_eq!(slash_command("/ hello"), None);
        assert_eq!(slash_command("fix /compact"), None);
        assert_eq!(slash_command(""), None);
    }

    #[test]
    fn berth_keeps_some_commands_to_itself() {
        let terminal = vec!["color".to_owned()];
        assert!(blocked("model", &terminal).is_some());
        assert!(blocked("color", &terminal).is_some());
        assert!(blocked("compact", &terminal).is_none());
        let known = Commands {
            commands: vec![
                cmd("usage", &["cost"]),
                cmd("resume", &["continue-session"]),
            ],
            terminal,
        };
        assert!(known.blocked("continue-session").is_some());
        assert!(known.blocked("cost").is_none());
    }

    #[test]
    fn prose_is_not_a_command_once_the_list_is_known() {
        let unknown = Commands::default();
        assert!(unknown.is_command("/tmp is full"));
        let known = Commands {
            commands: vec![cmd("compact", &[]), cmd("usage", &["cost"])],
            terminal: vec![],
        };
        assert!(!known.is_command("/tmp is full"));
        assert!(known.is_command("/compact keep notes"));
        assert!(known.is_command("/cost"));
    }

    #[test]
    fn completions_rank_prefixes_first() {
        let known = Commands {
            commands: vec![
                cmd("context", &[]),
                cmd("compact", &[]),
                cmd("usage", &["cost"]),
                cmd("__remote", &[]),
                cmd("code-review", &[]),
            ],
            terminal: vec![],
        };
        let names = |typed| -> Vec<String> {
            known
                .completions(typed)
                .iter()
                .map(|c| c.name.clone())
                .collect()
        };
        assert_eq!(names("co"), ["code-review", "compact", "context", "usage"]);
        assert_eq!(names("view"), ["code-review"]);
        assert_eq!(names("").len(), 4);
    }
}
