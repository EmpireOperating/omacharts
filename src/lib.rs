//! omacharts: fast, beautiful market charts for the Linux desktop.
//!
//! The app half. Storage, drawing and the GTK window live here; everything
//! worth testing without a display lives in `omacharts-engine`.

pub mod loader;
pub mod store;
pub mod theming;
pub mod ui;
