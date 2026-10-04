# Omacharts

Fast, beautiful market charts for [Omarchy](https://omarchy.org).

![Omacharts](doc/screenshot.png)

## Features

- **Fast.** Launches in under 200ms and keeps every series it has fetched, so
  arrowing down a watchlist redraws instantly rather than loading.
- **Beautiful.** The chart is the protagonist. There is no header bar, the
  controls appear when the pointer is near them and get out of the way when it
  is not, and nothing is drawn that you did not ask for. It follows your
  Omarchy theme as you change it, and generates an indicator palette for
  whichever theme is active, so overlays come out distinguishable, legible
  against the candles, and in keeping with everything else on screen. Any of
  the 22 themes, including the awkward ones.
- **Configurable layout.** Split a chart horizontally or vertically, as deep as
  you like, and resize the panes with the mouse or the keyboard. Each chart
  keeps its own symbol, resolution, indicators and settings. Keep as many
  arrangements as you want as chartbooks, each with its own watchlist, and
  switch between them from the strip along the bottom. It all comes back the
  way you left it.
- **Linked or parked.** Linked charts follow the watchlist together; unlink one
  and it stays where you left it.
- **Keyboard first.** Hotkeys for the whole app: split and close charts,
  resize them, walk the chartbooks, step the resolution, rotate the
  watchlists. Type a letter to find a symbol, a number to set a resolution.
  Press `?` for the lot.
- **Indicators.** Moving averages, VWAP with bands, volume, volume profile,
  RSI and ATR, each in its own resizable strip. More coming.
- **A bar widget** for the Omarchy bar: your watchlist with sparklines, live,
  still there after the window closes.
- **Agent ready.** A rich CLI covers everything the window does, and says what
  it can do in a form a script or an agent can read.

## Installing

Needs Rust, GTK 4 and libadwaita.

```sh
git clone https://github.com/jorgemanrubia/omacharts
cd omacharts
cargo build --release
./bin/install
```

## Data

Yahoo Finance's public endpoint, no account or key needed, cached locally in
SQLite. Prices are delayed about 15 minutes, which is invisible on the
timeframes this is built for. Not affiliated with Yahoo.

## Roadmap

Rough order, and nothing here is a promise.

- [ ] **Beautiful annotations.** Trendlines, levels and notes that stay where
      you put them, and look like they belong on the chart rather than on top
      of it.
- [ ] **More data feeds.** Yahoo is one provider behind one interface. Others
      can sit behind the same one, including the paid ones with real-time
      prices.
- [ ] **More indicators.** MACD and Bollinger bands are the obvious gaps.

Pull requests are welcome.

## Licence

MIT. See [LICENSE](LICENSE).

## From a terminal

Everything the window can do, `omacharts` can do from a command line, and a
command takes effect in a window that is already open, straight away.

```
omacharts watchlist create Semis
omacharts watchlist add Semis NVDA AMD AVGO TSM MU
```

See [doc/cli.md](doc/cli.md), or `omacharts surface --json` for the whole
command surface in a form a script can read.

## From an agent

Because the whole command surface is machine-readable, an agent can drive this
app as well as a person can. Nothing about that is tied to one agent: any of
them can run a command and read `omacharts surface --json`, which describes
every command, argument and allowed value in a form meant to be parsed rather
than skimmed. [AGENTS.md](AGENTS.md) covers the conventions, and is read by
Claude and Codex alike.

What is left is discovery. An agent has to know the app is there before it
thinks to ask, so Omacharts ships a skill for Claude that makes it reach for
Omacharts when you say "what's semis doing" or "set me up for the open", from
anywhere on the machine. The equivalent for another agent is a few lines
pointing it at `omacharts surface --json`; the surface does the rest.

Install the skill as a plugin, which keeps it updated with the app:

```sh
claude plugin marketplace add /usr/share/omacharts/claude-plugin
claude plugin install omacharts@omacharts
```

Or, if you have just installed the package and are already in a terminal:

```sh
omacharts skill install
```

Both are opt-in and nothing installs either one for you: putting a charting
app on your machine is not an agreement to have your agent's configuration
written into. `omacharts skill --help` has the rest: `uninstall` takes it
back out, and `status` says where it is. See
[doc/cli.md](doc/cli.md#teaching-an-agent-about-this-app).
