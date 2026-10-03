//! Everything omacharts knows that is not a widget.
//!
//! This crate links neither GTK nor SQLite. The app owns storage and drawing;
//! the engine owns themes, bars, the symbol index and the data providers. That
//! split keeps the parts that deserve tests testable without a display, and
//! keeps the provider boundary honest.

pub mod bars;
pub mod omarchy;
pub mod provider;
pub mod providers;
pub mod symbols;
pub mod theme;

pub use bars::{resample, Bar, Timeframe};
pub use provider::{Capability, Provider, ProviderError};
pub use symbols::{Instrument, InstrumentKind, SearchHit, SearchIndex};
pub use theme::{
    theme_bars, BarScheme, BarSlot, ColorChoice, Direction, Mode, Source, Swatch, Theme,
    UiColors, UiSlot,
};
