//! Terminals, as in VS Code: a bottom panel per window showing the selected
//! slot's terminals. Each slot keeps its own terminals while the app runs.

mod colors;
mod keys;
mod panel;
mod view;

pub use panel::{HEADER_H, panel};
pub use view::{TerminalEvent, TerminalView};
