//! A model's reasoning, kept in Claude Code's conversation as thinking
//! blocks Berth signs itself (ADR 0037).
//!
//! Claude Code keeps a thinking block, signature and all, and sends it back
//! with the conversation while the model stays the same; when the model
//! changes (back to Claude, say) it drops them, so Anthropic never sees
//! these. The signature says what the reasoning is: the Responses API's
//! encrypted reasoning item (sent back so the model goes on from it), a
//! Chat Completions `reasoning_content` (sent back where the server takes
//! it), or reasoning only to read (`<think>` text).

use serde_json::{Value, json};

const PREFIX: &str = "berth-reasoning:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Kept {
    /// A Responses API reasoning item: its id and encrypted content.
    Item { id: String, encrypted: String },
    /// A Chat Completions `reasoning_content`: the thinking's text.
    Content,
    /// To read, not to send back.
    Shown,
}

impl Kept {
    pub(crate) fn signature(&self) -> String {
        match self {
            Self::Item { id, encrypted } => {
                format!(
                    "{PREFIX}item:{}",
                    json!({ "id": id, "encrypted": encrypted })
                )
            }
            Self::Content => format!("{PREFIX}content"),
            Self::Shown => format!("{PREFIX}shown"),
        }
    }

    /// What a thinking block's signature says, if Berth wrote it.
    pub(crate) fn read(signature: &str) -> Option<Self> {
        let rest = signature.strip_prefix(PREFIX)?;
        if let Some(item) = rest.strip_prefix("item:") {
            let v: Value = serde_json::from_str(item).ok()?;
            return Some(Self::Item {
                id: v.get("id")?.as_str()?.to_owned(),
                encrypted: v.get("encrypted")?.as_str()?.to_owned(),
            });
        }
        match rest {
            "content" => Some(Self::Content),
            _ => Some(Self::Shown),
        }
    }
}

/// What a thinking block shows when the model gave no summary of it.
pub(crate) const UNSUMMARIZED: &str = "(The model reasoned here without a summary.)";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_read_back() {
        let item = Kept::Item {
            id: "rs_1".into(),
            encrypted: "gAAA\"==".into(),
        };
        assert_eq!(Kept::read(&item.signature()), Some(item));
        assert_eq!(Kept::read(&Kept::Content.signature()), Some(Kept::Content));
        assert_eq!(Kept::read(&Kept::Shown.signature()), Some(Kept::Shown));
        assert_eq!(Kept::read("EqQBCkYIBxgCKkB…"), None);
    }
}
