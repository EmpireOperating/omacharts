//! Fetching bars off the UI thread.
//!
//! The window never waits on the network. It paints whatever the cache holds,
//! asks for the gap, and repaints when the answer arrives. A failure is not an
//! empty chart — it is the same chart, marked stale.
//!
//! The worker opens its own [`Store`]: a rusqlite connection is `Send` but not
//! `Sync`, and WAL means a writer here never blocks the reader there.

use omacharts_engine::{Bar, Provider, ProviderError, Timeframe};

use crate::store::Store;

/// How many already-cached bars a tail fetch deliberately re-requests.
///
/// Yahoo serves split- and dividend-adjusted prices, so a split rewrites an
/// instrument's whole history behind our back and the cache silently stops
/// matching. Overlapping a few bars catches that the moment it happens, for
/// the price of a handful of comparisons.
const OVERLAP: i64 = 5;

/// Prices agreeing to within this fraction are the same price. Yahoo's
/// adjusted values wobble in the last decimal place between responses.
const TOLERANCE: f64 = 0.0005;

pub struct Request {
    /// Cache key: `provider:symbol`.
    pub key: String,
    /// The provider's own spelling.
    pub symbol: String,
    /// What the chart is showing. Fetched at `timeframe.native()`.
    pub timeframe: Timeframe,
}

pub enum Response {
    /// Bars at the *native* timeframe. The caller folds to what it is showing.
    Bars { key: String, timeframe: Timeframe, bars: Vec<Bar> },
    /// Nothing new arrived. `bars` is whatever the cache already had.
    Failed { key: String, timeframe: Timeframe, bars: Vec<Bar>, error: String, rate_limited: bool },
}

/// Read the cache, fetch only what is missing, merge, and reply.
pub fn spawn<P>(request: Request, provider: P, sender: async_channel::Sender<Response>)
where
    P: Provider + 'static,
{
    std::thread::spawn(move || {
        let response = run(&request, &provider);
        // The window closing before we finish is normal, not an error.
        let _ = sender.send_blocking(response);
    });
}

fn run<P: Provider>(request: &Request, provider: &P) -> Response {
    let native = request.timeframe.native();
    let Ok(store) = Store::open() else {
        return Response::Failed {
            key: request.key.clone(),
            timeframe: native,
            bars: Vec::new(),
            error: "could not open the local database".into(),
            rate_limited: false,
        };
    };

    let cached = store.load_bars(&request.key, native);
    let coverage = store.coverage(&request.key, native);

    // Ask only for the gap. A fresh series asks for everything; an existing
    // one asks from a few bars before where it ends.
    let since = coverage.map(|c| c.last_ts - OVERLAP * native.seconds());

    match provider.bars(&request.symbol, native, since) {
        Ok(fresh) if fresh.is_empty() => Response::Bars {
            key: request.key.clone(),
            timeframe: native,
            bars: cached,
        },
        Ok(fresh) => {
            // If the overlap disagrees, the cached history was adjusted out
            // from under us and cannot be trusted. Start over.
            if !cached.is_empty() && !overlap_agrees(&cached, &fresh) {
                store.drop_series(&request.key, native);
                let full = match provider.bars(&request.symbol, native, None) {
                    Ok(full) => full,
                    // We dropped the cache and could not refill it; report
                    // honestly rather than showing a series we know is stale.
                    Err(error) => {
                        return Response::Failed {
                            key: request.key.clone(),
                            timeframe: native,
                            bars: Vec::new(),
                            error: error.to_string(),
                            rate_limited: matches!(error, ProviderError::RateLimited),
                        }
                    }
                };
                store.write_bars(&request.key, native, &full);
                return Response::Bars { key: request.key.clone(), timeframe: native, bars: full };
            }

            let merged = store.merge_bars(&request.key, native, &fresh);
            Response::Bars { key: request.key.clone(), timeframe: native, bars: merged }
        }
        Err(error) => Response::Failed {
            key: request.key.clone(),
            timeframe: native,
            bars: cached,
            error: error.to_string(),
            rate_limited: matches!(error, ProviderError::RateLimited),
        },
    }
}

/// Do the bars we already had still match what the provider just sent for the
/// same timestamps?
///
/// Only completed bars count. The newest cached bar may have been forming when
/// we stored it, so it is expected to differ and is excluded.
fn overlap_agrees(cached: &[Bar], fresh: &[Bar]) -> bool {
    let newest_complete = cached.last().map(|b| b.ts).unwrap_or(i64::MIN);
    let mut compared = 0;
    for bar in fresh {
        if bar.ts >= newest_complete {
            continue;
        }
        if let Ok(at) = cached.binary_search_by_key(&bar.ts, |b| b.ts) {
            compared += 1;
            if !close(cached[at].close, bar.close) || !close(cached[at].open, bar.open) {
                return false;
            }
        }
    }
    // No shared completed bars means nothing to contradict.
    let _ = compared;
    true
}

fn close(a: f64, b: f64) -> bool {
    let scale = a.abs().max(b.abs()).max(1.0);
    (a - b).abs() / scale <= TOLERANCE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(ts: i64, close: f64) -> Bar {
        Bar { ts, open: close, high: close, low: close, close, volume: 1.0 }
    }

    #[test]
    fn an_unchanged_overlap_agrees() {
        let cached = vec![bar(100, 10.0), bar(200, 11.0), bar(300, 12.0)];
        let fresh = vec![bar(100, 10.0), bar(200, 11.0), bar(300, 99.0), bar(400, 13.0)];
        // 300 is the newest cached bar and may have been forming, so its
        // disagreement is expected and ignored.
        assert!(overlap_agrees(&cached, &fresh));
    }

    #[test]
    fn a_split_adjusted_history_disagrees() {
        let cached = vec![bar(100, 100.0), bar(200, 110.0), bar(300, 120.0)];
        // A 10:1 split rewrites every completed bar.
        let fresh = vec![bar(100, 10.0), bar(200, 11.0), bar(300, 12.0)];
        assert!(!overlap_agrees(&cached, &fresh));
    }

    #[test]
    fn rounding_noise_is_not_a_split() {
        let cached = vec![bar(100, 100.0), bar(200, 110.0), bar(300, 120.0)];
        let fresh = vec![bar(100, 100.00001), bar(200, 109.99998), bar(300, 120.0)];
        assert!(overlap_agrees(&cached, &fresh));
    }

    #[test]
    fn no_shared_history_is_not_a_disagreement() {
        let cached = vec![bar(100, 10.0)];
        let fresh = vec![bar(500, 50.0), bar(600, 60.0)];
        assert!(overlap_agrees(&cached, &fresh));
    }

    #[test]
    fn closeness_scales_with_magnitude() {
        assert!(close(1.0, 1.0000001));
        assert!(close(20000.0, 20000.5));
        assert!(!close(1.0, 1.5));
        assert!(!close(100.0, 10.0));
    }
}
