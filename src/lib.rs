//! omacharts: fast, beautiful market charts for Omarchy.
//!
//! The app half. Storage, drawing and the GTK window live here; everything
//! worth testing without a display lives in `omacharts-engine`.

pub mod bar_plugin;
pub mod cache;
pub mod cli;
pub mod inventory;
pub mod loader;
pub mod migrations;
pub mod store;
pub mod theming;
pub mod ui;
