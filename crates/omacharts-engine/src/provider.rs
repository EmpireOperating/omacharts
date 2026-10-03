//! The data provider boundary.
//!
//! Everything that knows how to talk to a market data source lives behind this
//! trait. v0 ships Yahoo alone, but a broker feed or a paid vendor arrives as
//! another implementation and nothing above this line changes — which is the
//! whole reason the boundary exists.

use std::fmt;

use crate::bars::{Bar, Timeframe};
use crate::symbols::{Instrument, InstrumentKind};

#[derive(Debug)]
pub enum ProviderError {
    /// The provider cannot serve this instrument or timeframe at all.
    Unsupported(String),
    /// Rate limited. The cache is the defence; back off and show what we have.
    RateLimited,
    Network(String),
    /// A response arrived but did not look like data.
    Malformed(String),
    /// The provider has no such symbol.
    NotFound,
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderError::Unsupported(what) => write!(f, "not supported: {what}"),
            ProviderError::RateLimited => write!(f, "rate limited"),
            ProviderError::Network(e) => write!(f, "network: {e}"),
            ProviderError::Malformed(e) => write!(f, "unexpected response: {e}"),
            ProviderError::NotFound => write!(f, "no such symbol"),
        }
    }
}

impl std::error::Error for ProviderError {}

/// What a provider can serve for one timeframe.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Capability {
    pub timeframe: Timeframe,
    /// How far back the provider will go, in days. `None` means "as far as it
    /// has".
    pub history_days: Option<u32>,
    /// The widest window a single request may ask for, in days.
    ///
    /// Separate from `history_days` because providers cap the two
    /// independently: Yahoo keeps a month of one-minute bars but refuses to
    /// hand over more than a week at a time, and asking for the month returns
    /// nothing at all rather than an error.
    pub max_request_days: Option<u32>,
}

pub trait Provider: Send + Sync {
    /// Stable id, used as the `adapter` key in stored symbol mappings.
    fn id(&self) -> &'static str;

    /// What to call it in the UI.
    fn label(&self) -> &'static str;

    /// How stale this provider's data is for a kind of instrument, in minutes.
    /// Shown to the user — a chart must never imply it is live when it is not.
    fn delay_minutes(&self, kind: InstrumentKind) -> u32;

    /// This instrument in the provider's own symbology, if it serves it.
    fn symbol_for(&self, instrument: &Instrument) -> Option<String>;

    /// Natively served timeframes. Derived ones are folded by the caller.
    fn capabilities(&self) -> &'static [Capability];

    /// Bars for `symbol`, oldest first, at a timeframe the provider serves
    /// natively.
    ///
    /// `since` is an inclusive lower bound in unix seconds; `None` asks for as
    /// much history as the provider will give. Implementations must not return
    /// bars they had to invent, and the final bar may still be forming.
    fn bars(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        since: Option<i64>,
    ) -> Result<Vec<Bar>, ProviderError>;

    /// Bars for something nobody asked for yet.
    ///
    /// Same data, lower claim on the provider: implementations are expected to
    /// pace this further apart and to refuse it outright while they are being
    /// throttled, so filling a watchlist in the background can never cost
    /// someone the chart they are actually looking at. Defaults to a normal
    /// fetch for providers with no rate limit worth respecting.
    fn bars_speculative(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        since: Option<i64>,
    ) -> Result<Vec<Bar>, ProviderError> {
        self.bars(symbol, timeframe, since)
    }

    /// Does this provider serve the timeframe natively?
    fn serves(&self, timeframe: Timeframe) -> bool {
        self.capabilities().iter().any(|c| c.timeframe == timeframe)
    }
}
