//! Interactive TUI configuration viewer using ratatui.
//!
//! Read-only for now: shows the parsed configuration and current state,
//! keyboard-driven navigation. Editing capabilities land in a follow-up
//! commit.

mod app;
mod render;

pub use app::run_config_editor;
