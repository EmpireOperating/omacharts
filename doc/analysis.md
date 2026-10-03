# Market data analysis

Research notes for **omacharts**, an Omarchy plugin for checking US stock indexes
and futures at higher timeframes.

Compiled 2026-10-03. Pricing and free-tier limits move; re-verify before acting
on any number here.

## Context

The goal is a free app people install, with no business behind it and no
server. The use case is **checking higher timeframes** — daily, hourly, and
resampled 4H — not execution, not scalping, not live P&L.

That framing settles most of the decisions below. Real-time data is solving a
problem omacharts doesn't have: a 15-minute delay is invisible on a candle that
takes four hours to form.

Existing prior art for the author: a related project already consumes Sierra
Chart data locally (`~/Downloads/SierraChart/Data`, `.scid` intraday +
`.dly` daily files per contract). Sierra remains the quality baseline for
comparison, and a Sierra adapter is the natural first non-default provider.

## The licensing constraint

This dominates provider choice more than any technical factor.

**Index values are licensed IP, not market data.** SPX, NDX and DJI are
products of S&P Dow Jones Indices and Nasdaq. Displaying them in a commercial
product — even delayed — requires a license from the index owner. This is why
many apps chart SPY/QQQ/DIA instead: ETFs are ordinary equities off the
consolidated tape (CTA/UTP), far cheaper, and delayed non-pro redistribution is
close to free.

**Futures are CME Group data.** The delayed feed (10 minutes) carries a cheap,
widely-granted redistribution license. Real-time means a CME license, per-user
reporting, and a bill that scales with user count.

### Why a local, installable app sidesteps all of it

If the app runs entirely on the user's machine and fetches with the user's own
credentials, there is no redistribution. Each install is a person getting their
own data under their own license, the same as opening a browser. For an
individual the cost is near zero — personal users qualify as **non-professional**
subscribers.

Two rules keep omacharts on that side of the line:

1. **Never ship an API key.** A shared key embedded in the binary makes every
   user's request *our* request, us the licensee, and us a redistributor on a
   personal-use agreement. This is the mistake that actually kills projects.
2. **Never run a server.** No proxy, no hosted cache, no convenience backend.
   The instant data passes through our infrastructure we are a redistributor
   again. Local fetch, local disk cache.

Both are easy to honor and are the normal shape for this kind of tool.

## Provider landscape

### Free, keyless — works on install

| Source | Covers | Notes |
|---|---|---|
| **Yahoo** (`query1.finance.yahoo.com`) | indexes, futures, equities | No key, no signup. Unofficial. What the whole Omarchy ecosystem uses. |
| **Stooq** | EOD indexes, equities, some futures | Plain CSV URLs, no key, generous limits. Ideal fallback. |
| **Investing.com** | daily futures CSV | Manual download, no account. |
| **FRED** | daily index closes, macro | Properly licensed, but S&P series capped ~10 years and no intraday. |
| **Dukascopy** | tick history for index CFDs (US500, USA100, USA30) | Free, deep, surprisingly underused. Not the futures contract but tracks closely. |

### Free tiers, key required

| Source | Free tier | Caveat |
|---|---|---|
| Nasdaq Data Link | 500 req/day | Real futures coverage. Best keyed free tier. |
| Alpaca | real-time IEX + history | Account needed (no funding). No futures. IEX is ~2% of volume. |
| Financial Modeling Prep | 250 req/day | US only. |
| EODHD | 20 calls/day | Futures coverage thin on free tier. |
| Alpha Vantage | 25 req/day | Tightened to near-demo levels. |
| marketstack | 100 req/month | Equities-focused. |

### Broker feeds — the cheap real-time path

Real-time via a *broker* costs a fraction of real-time via a *vendor*.

- **Tastytrade** — real-time futures **free** with a funded account. API
  included at no charge, streams over dxLink (dxFeed). Genuinely $0 for
  non-professionals. 14 days free on a new unfunded account, then delayed.
- **IBKR** — CME L1 bundle **$4.65/month**, and the fee is **waived if the
  account generates $20/month in commissions**. Delayed data is free with no
  subscription at all (`reqMarketDataType(3)`), covers CME futures and indexes,
  and is cleaner than Yahoo because it's licensed rather than scraped.

Neither is redistributable — they're licensed to the account holder. That fits
the BYO-credential model exactly.

### Paid vendors

- **Databento** — best-in-class for futures. Direct CME MDP 3.0 capture in
  colocation, nanosecond timestamps, normalized schemas from full order book
  (MBO) down to OHLCV aggregates, intraday replay that backfills from session
  open and transitions seamlessly into live. **Too expensive for this app's
  users** — see pricing below.
- **Barchart** — broad futures coverage, delayed feeds built for commercial
  redistribution, one vendor across futures/equities/indexes. The classic
  answer for a company shipping delayed futures.
- **dxFeed** — futures, equities, index feeds built for redistribution. Used by
  many broker front-ends.
- **Polygon.io** — excellent US equities and options, great WebSocket,
  developer-friendly. Indices product exists; futures support has been moving —
  verify current state.
- **Avoid for redistribution:** IQFeed and IBKR (ToS forbids it), Yahoo (no
  commercial license at all).

## Pricing reference

### Databento (the one that got ruled out)

Usage-based live pricing for CME was **discontinued in April 2025**. Live data
now requires a subscription plan:

| Plan | Cost | Notes |
|---|---|---|
| Usage-Based | $/GB | **Historical only** |
| Standard | **$199/month** | Live data included, license fees included. L1 history 12mo, L2/L3 1mo |
| Plus | $1,750/month | 16+ yr L1 history, external distribution allowed |
| Unlimited | $4,500/month | Full history, all schemas |

New accounts get **$125 in credits**, expiring 6 months after signup.

The widely-quoted "$32.65–36.50/month" figure is **only CME's license
passthrough**, not the price of the service. At ~$199/month per user, Databento
is a non-starter for a free app.

### CME Group exchange fees (Jan 2026, non-professional)

These are the real floor — the exchange is cheap, vendors are not.

| Data | Per exchange | All four (CME/CBOT/NYMEX/COMEX) |
|---|---|---|
| Top of Book (L1) | $1.55/mo | **$4.65/mo** |
| Depth of Market (L2) | $12.10/mo | **$36.50/mo** |

Professional non-display runs $1,219/month for comparison.

### Summary of realistic options for omacharts users

| Option | Cost | Latency | Fit |
|---|---|---|---|
| **Yahoo** | $0 | 10–15 min delayed | ✅ default |
| **Stooq** | $0 | EOD | ✅ fallback |
| Nasdaq Data Link | $0 | EOD | optional |
| IBKR delayed | $0 (account) | delayed | optional adapter |
| IBKR real-time | $4.65/mo or waived | real-time | optional adapter |
| Tastytrade | $0 (funded account) | real-time | optional adapter |
| Sierra | existing setup | real-time | author's own adapter |
| Databento | ~$199/mo | real-time | ❌ ruled out |

## Yahoo / yfinance detail

### Endpoint

`https://query1.finance.yahoo.com/v8/finance/chart/{symbol}?range=…&interval=…`

Use **`v8/finance/chart` for everything, including quotes**. The older
`v7/finance/quote` now requires a cookie + crumb handshake (via
`v1/test/getcrumb`); the chart endpoint needs none of it and
`meta.regularMarketPrice` in the response gives the quote anyway. This is why
the whole plugin ecosystem consolidated on it.

Symbol search: `v1/finance/search`.

### Interval limits (Yahoo-side, no key changes them)

| Interval | History available |
|---|---|
| `1m` | 30 days total, **max 7 days per request** |
| `2m`, `5m`, `15m`, `30m`, `90m` | 60 days |
| `1h` | **730 days (~2 years)** |
| `1d`, `5d`, `1wk`, `1mo` | full history, decades |

**`1h` is the sweet spot for omacharts**: two years of context, one request per
symbol, aggregates cleanly. Daily is effectively unlimited.

### Delay by asset class

- **Futures** — 10 minutes delayed
- **Indexes** — ~15 minutes delayed
- **US equities** — closest to real-time, varies by symbol, nothing guaranteed

### Gotchas

**There is no 4H interval.** Resample from `1h`. For futures this matters:
Sierra builds 4H bars off the *session* (18:00 ET open), while naive resampling
buckets on midnight UTC or local midnight. Set the resample origin to session
open or the bars will silently disagree with Sierra.

**The last bar repaints.** The forming candle updates as it goes and is also
10–15 min stale for futures. Drop the incomplete bar or mark it visually as
forming; don't compute indicators on it.

**Rate limiting is real in 2026.** `YFRateLimitError` / HTTP 429, and blocked
IPs can stay blocked for days. But this comes from pulling 1m bars across many
symbols. omacharts's pattern — a handful of symbols at higher timeframes — is a
few dozen requests a day, nowhere near the threshold. Higher timeframes are the
one use case where Yahoo is well-behaved.

**Endpoints change without notice.** This is the real risk, not legal exposure.
A second adapter turns breakage into a degradation instead of an outage.

## What existing Omarchy plugins do

Surveyed the official marketplace registry (`omacom/omarchy-plugin-marketplace`,
4,816 listed sources, 37 finance-related plugins) and read the source of 18
stock/market plugins.

### The verdict is unanimous

**14 of 18 use `query1.finance.yahoo.com/v8/finance/chart/`** — 31 references
across the corpus, by far the most-used endpoint. These are QML/Quickshell
plugins calling the endpoint directly rather than through `yfinance`, but it's
the same endpoint.

Keyed providers are the rare exception:

| Plugin | Data source | Key required |
|---|---|---|
| `rookepoole/omarchy-market-pulse` | Alpaca (IEX) | yes |
| `xdamus/omarchy-stock-exchange` | Twelve Data + CNBC fallback | yes |
| `mryll/tickerbar` | Finnhub + CoinGecko + CNBC | yes |
| everyone else | Yahoo | **no** |

Crypto plugins go straight to Binance / Coinbase / CoinGecko / Hyperliquid, all
keyless. **Nobody in the entire marketplace uses a paid feed** — requiring an
API key kills install-and-go.

### Symbols already in use

Futures: `ES=F` (8 refs), `GC=F` (7), `SI=F`, `NQ=F`, `CL=F`, `YM=F`, `NG=F`,
`HG=F`.

Indexes: `^GSPC` (45), `^SPX` (21), `^IXIC` (14), `^DJI` (13), `^FTSE`, `^VIX`,
`^N225`, `^NDX`, `^TNX`.

So charting GC and ES through Yahoo is well-trodden here, not pioneering.

### Observed conventions

- Typical params: `range=1d&interval=5m` for sparklines, `interval=1d` for
  longer views
- Polling timers cluster at **15–35 seconds** (far more aggressive than omacharts
  needs)
- The mature plugins cache and handle 429s; the minimal ones do neither

### Projects worth reading

- **`dmitry-solomadin/omastocks`** — the most thorough of the lot. Yahoo plus
  `stockanalysis.com` as a secondary source, with caching and backoff already
  worked out.
- **`tcballard/omarchy-markets`** — explicitly documents itself as hardening the
  ticker pattern with "offline resilience and a stricter long-running service
  contract." 26 cache-touching files.
- **`CostaFot/omarchy-markets`** — a port of the Markets extension for Command
  Palette. Heavy caching (23 rate-limit-aware files, 21 cache files).
- **`mryll/tickerbar`** — interesting as the multi-provider example (Finnhub +
  CoinGecko + CNBC + regional sources).
- **`5d0tal1gat0r/omarchy-stocks`** — "Yahoo Finance, no API key", clean minimal
  reference.
- **`DPRC137/omarchy-market`** — Yahoo for stocks plus three crypto venues, with
  live charts.

Weak references (no caching, no rate-limit handling — these break first):
`Macs9319/OmaStockTicker`, `duketopceo/omarchy-ticker`.

## Recommended approach

**Provider interface with pluggable adapters.** One normalized bar schema,
vendors behind an interface. This costs almost nothing up front and keeps every
later decision a one-file change.

1. **Yahoo as the zero-config default.** Works the instant someone installs,
   covers `^GSPC` / `^NDX` / `^VIX` / `ES=F` / `NQ=F` / `GC=F` / `CL=F`.
2. **Stooq as the fallback adapter.** The single highest-value addition —
   Yahoo *will* break occasionally, and a second source makes that a non-event
   instead of an outage.
3. **Opt-in adapters** for people who want more: Sierra (already written),
   IBKR, Tastytrade, Nasdaq Data Link. User supplies their own credentials.

### Design details to get right early

- **Cache to local disk**, with refresh tied to bar period — refetch daily bars
  a few times a day, not every 30 seconds. Free sources are rate-limited and a
  chart that refetches on every pane switch gets throttled within minutes.
- **Build continuous contracts ourselves** from individual contract months, with
  an explicit roll rule. Vendors differ on roll date and back-adjustment, so
  `ES=F` from Yahoo won't match a rolled series from Databento or Sierra. Don't
  let chart history silently change shape when someone switches adapters.
- **Session-aware resampling** for the 4H timeframe (see gotchas above).
- **Show the feed and its delay in the UI**, per provider. Two lines of code,
  stops us confusing ourselves about which chart we're looking at, and it's good
  faith toward the data sources.

## Open questions

- Which symbols ship in the default watchlist?
- Quickshell/QML bar plugin, standalone TUI, or both?
- Does the Sierra adapter read `.scid`/`.dly` directly, or via Sierra's DTC
  protocol?

## Sources

- [Databento pricing](https://databento.com/pricing)
- [Databento: new CME pricing plans](https://databento.com/blog/introducing-new-cme-pricing-plans)
- [Databento: live CME open to all users](https://roadmap.databento.com/announcements/live-cme-data-is-now-open-to-all-users-starting-at-3265month)
- [CME Group January 2026 market data fee list](https://www.cmegroup.com/market-data/files/january-2026-market-data-fee-list.pdf)
- [IBKR market data pricing](https://www.interactivebrokers.com/en/pricing/market-data-pricing.php)
- [Tastytrade: live quotes and account funding](https://support.tastytrade.com/support/s/solutions/articles/43000475299)
- [Tastytrade developer docs](https://developer.tastytrade.com/basic-api-usage/)
- [yfinance rate limiting discussion](https://github.com/ranaroussi/yfinance/discussions/2431)
- [Omarchy plugin marketplace](https://github.com/omacom/omarchy-plugin-marketplace)
