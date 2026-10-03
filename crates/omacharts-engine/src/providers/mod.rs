//! The data providers.
//!
//! v0 ships Yahoo alone. Another source arrives as a new module here
//! implementing [`crate::Provider`], and nothing above the trait changes.

pub mod yahoo;

pub use yahoo::Yahoo;
