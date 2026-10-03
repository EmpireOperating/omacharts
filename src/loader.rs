//! Fetching bars off the UI thread.
//!
//! The window never waits on the network. It paints whatever the cache holds,
//! asks for the gap, and repaints when the answer arrives. A failure is not an
//! empty chart — it is the same chart, marked stale.
//!
//! The worker opens its own [`Store`]: a rusqlite connection is `Send` but not
//! `Sync`, and WAL means a writer here never blocks the reader there.

use std::sync::{Arc, Condvar, Mutex};

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

#[derive(Clone, PartialEq, Debug)]
pub struct Request {
    /// True when nobody asked for this yet. Providers pace speculative work
    /// further apart and refuse it outright while being throttled, so filling
    /// the rail can never cost someone the chart they are looking at.
    pub speculative: bool,
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

/// The chart you are looking at. Jumps every queued prefetch.
pub const FOREGROUND: u32 = 0;

/// Where speculative work starts, leaving room beneath it for the symbols
/// immediately around the selection.
pub const BACKGROUND: u32 = 1_000;

/// Fetches bars on one worker thread, nearest-wanted first.
///
/// One thread, not one per request, for two reasons. The provider is paced —
/// requests are deliberately kept apart — so concurrency would only queue
/// inside the throttle anyway. And a queue can be reordered: when you arrow
/// onto a different symbol, the chart you are now looking at must overtake
/// the twenty prefetches queued behind it, not wait for them.
pub struct Loader {
    inner: Arc<Inner>,
}

struct Inner {
    queue: Mutex<Queue>,
    wake: Condvar,
}

struct Queue {
    jobs: Vec<Job>,
    /// Breaks ties so equal priorities keep the order they were asked in.
    next_seq: u64,
    shutdown: bool,
}

struct Job {
    request: Request,
    priority: u32,
    seq: u64,
}

impl Loader {
    pub fn new<P>(provider: P, sender: async_channel::Sender<Response>) -> Loader
    where
        P: Provider + 'static,
    {
        let inner = Arc::new(Inner {
            queue: Mutex::new(Queue { jobs: Vec::new(), next_seq: 0, shutdown: false }),
            wake: Condvar::new(),
        });

        let worker = inner.clone();
        std::thread::spawn(move || {
            while let Some(job) = worker.take() {
                let response = run(&job.request, &provider);
                // The window closing before we finish is normal, not an error.
                if sender.send_blocking(response).is_err() {
                    break;
                }
            }
        });

        Loader { inner }
    }

    /// Queue a fetch. Lower `priority` runs first.
    ///
    /// Asking again for something already queued keeps the better priority
    /// rather than queueing it twice — which is what happens constantly as you
    /// arrow down a watchlist and the same neighbours keep being re-offered.
    pub fn fetch(&self, request: Request, priority: u32) {
        let mut queue = self.inner.queue.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = queue
            .jobs
            .iter_mut()
            .find(|j| j.request.key == request.key && j.request.timeframe == request.timeframe)
        {
            existing.priority = existing.priority.min(priority);
            return;
        }
        let seq = queue.next_seq;
        queue.next_seq += 1;
        queue.jobs.push(Job { request, priority, seq });
        drop(queue);
        self.inner.wake.notify_one();
    }

    /// Forget everything that is not the chart on screen.
    ///
    /// Called when the selection moves: the old neighbours are no longer the
    /// nearest ones, and leaving them queued would spend the request budget on
    /// symbols that are now far away.
    pub fn drop_prefetches(&self) {
        let mut queue = self.inner.queue.lock().unwrap_or_else(|e| e.into_inner());
        queue.jobs.retain(|job| job.priority == FOREGROUND);
    }

    pub fn queued(&self) -> usize {
        self.inner.queue.lock().unwrap_or_else(|e| e.into_inner()).jobs.len()
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        let mut queue = self.inner.queue.lock().unwrap_or_else(|e| e.into_inner());
        queue.shutdown = true;
        drop(queue);
        self.inner.wake.notify_all();
    }
}

impl Inner {
    /// Block until there is a job, then hand back the most wanted one.
    fn take(&self) -> Option<Job> {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if queue.shutdown {
                return None;
            }
            if let Some(at) = best(&queue.jobs) {
                return Some(queue.jobs.remove(at));
            }
            queue = self.wake.wait(queue).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// Index of the job to run next: lowest priority, then earliest asked.
fn best(jobs: &[Job]) -> Option<usize> {
    jobs.iter()
        .enumerate()
        .min_by_key(|(_, job)| (job.priority, job.seq))
        .map(|(at, _)| at)
}

fn run<P: Provider>(request: &Request, provider: &P) -> Response {
    let fetch = |symbol: &str, timeframe, since| {
        if request.speculative {
            provider.bars_speculative(symbol, timeframe, since)
        } else {
            provider.bars(symbol, timeframe, since)
        }
    };
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

    match fetch(&request.symbol, native, since) {
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
                let full = match fetch(&request.symbol, native, None) {
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

    fn job(key: &str, priority: u32, seq: u64) -> Job {
        Job {
            request: Request {
                key: key.into(),
                symbol: key.into(),
                timeframe: Timeframe::days(1),
                speculative: false,
            },
            priority,
            seq,
        }
    }

    #[test]
    fn the_most_wanted_job_runs_first() {
        let jobs = vec![job("far", 9, 0), job("near", 1, 1), job("chart", FOREGROUND, 2)];
        assert_eq!(best(&jobs), Some(2), "the chart on screen jumps the queue");

        let jobs = vec![job("far", 9, 0), job("near", 1, 1)];
        assert_eq!(best(&jobs), Some(1), "then the nearest neighbour");
    }

    #[test]
    fn equal_priorities_keep_their_order() {
        let jobs = vec![job("b", 3, 7), job("a", 3, 2)];
        assert_eq!(best(&jobs), Some(1), "asked for first, so run first");
    }

    #[test]
    fn an_empty_queue_has_nothing_to_run() {
        assert_eq!(best(&[]), None);
    }

    #[test]
    fn closeness_scales_with_magnitude() {
        assert!(close(1.0, 1.0000001));
        assert!(close(20000.0, 20000.5));
        assert!(!close(1.0, 1.5));
        assert!(!close(100.0, 10.0));
    }
}
