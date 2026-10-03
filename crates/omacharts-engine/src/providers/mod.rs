//! The data providers.
//!
//! v0 ships Yahoo alone. Sierra, IBKR and the rest arrive as new modules here
//! implementing [`crate::Provider`], and nothing above the trait changes.

pub mod yahoo;

pub use yahoo::Yahoo;
