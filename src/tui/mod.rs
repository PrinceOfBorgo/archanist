//! Read-only TUI viewer for the loaded config and state, built on
//! [`ratatui`]. Renders the components list, per-component recipe
//! summary, and the currently-recorded state side by side.

mod app;
mod render;

pub use app::run_config_editor;
