//! Clipboard read + paste-keystroke synthesis. Both halves of dictation's
//! "set clipboard, fire Ctrl/Cmd+V, restore prior clipboard" dance live
//! here so the snapshot/restore semantics stay paired.

pub mod copy;
pub mod paste;

pub use copy::capture_selection;
pub use paste::paste_text;
