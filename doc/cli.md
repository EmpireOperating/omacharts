# Driving Omacharts from a terminal

Everything the window can do can be done from here. This page is written for
somebody — or something — with no other knowledge of the app.

If you are a program rather than a person, read this instead and skip the
prose:

```
omacharts surface --json
```

That emits every command, every argument and flag with its type, the values
the enumerated ones accept, the exit codes, and a worked example per command.
It is generated from the same table the parser is built from, so it cannot
describe a command that does not exist.

## Where commands run

- **With the app open**, a command is handed to the running window and takes
  effect immediately — a watchlist you create in a terminal appears in the
  rail without a restart. The output and the exit status come back to your
  terminal as if it had run locally.
- **With nothing running**, the same command runs against the stored database.
  No GTK, no display, no window. This works over ssh.

You do not choose between them and there is no flag for it.

## Exit codes

An exit code is the only thing a script can rely on without parsing text.

| Code | Meaning |
|------|---------|
| 0 | the command did what it says |
| 1 | something went wrong that none of the others describe |
| 2 | the command was not spelled in a way the parser accepts |
| 3 | what was named does not exist |
| 4 | the name fits more than one thing; say which with `id:N` |
| 5 | understood, and refused |
| 6 | the command meant the chart you are looking at, and no window is open |

Data goes to stdout, errors to stderr, always.

## Naming things

Watchlists, sections and chartbooks are named by name, case-insensitively. If
two share a name, the command fails with code 4 and tells you the ids; say
which you meant with `id:N`:

```
omacharts watchlist show id:3
```

## Worked example: fine-tuning the chart in front of you

This is what the whole thing is for. Somebody is looking at a chart and says
"put a 200-day average on it". No identifiers, no lookups:

```
$ omacharts chart indicator add sma --period 200 --color Amber --style dashed
added SMA(200) on pos:0 AAPL 1D in "Macro"
  [exit 0]
```

**Every chart and chartbook command defaults to the chart you are looking at**,
in the chartbook that is open. Naming one explicitly still works and overrides
the default. The output always names what it actually hit — the symbol, the
resolution and the chartbook — so you can confirm it reached the right chart
without asking a second question.

That default only exists while a window does. With nothing running there is no
"the one I am looking at", and the command says so rather than quietly acting
on whatever was stored last:

```
$ omacharts chart indicator add rsi
omacharts: no window is open, so there is no focused chart to act on; name one, or start Omacharts
  [exit 6]
```

To find out what is open before changing it:

```
$ omacharts status show
chartbook  Macro (id:0)
* pos:0  AAPL       1D    unlinked
```

Everything `status` prints is spelled the way the other commands accept it, so
it composes without translation. **Prefer `pos:N` over a raw chart id**: the
window hands out fresh ids whenever it rebuilds an arrangement, so an id read
a moment ago can name a different chart. A position names the same place on
the screen either way.

## Indicators and their parameters

Every indicator the app has, with every parameter it exposes:

| Indicator | Parameters |
|---|---|
| `sma`, `ema` | `--period` |
| `rsi` | `--period`, `--overbought`, `--oversold`, `--height` |
| `atr` | `--period`, `--height` |
| `volume` | `--height` |
| `vwap` | `--anchor`, `--bands`, `--band-alpha` |
| `volume_profile` | `--anchor`, `--rows` (a number or `auto`), `--value-area`, `--poc-color` |

Anything that draws a line also takes `--color`, `--width` and `--style`, and
everything takes `--visible`.

Colours come in two kinds, and the difference matters:

- **A palette name** — Blue, Amber, Violet, Teal, Rose, Green, Orange, Cyan —
  is re-resolved every time the desktop theme changes, so the indicator follows
  the theme.
- **`#rrggbb`** is the exact colour you picked, and is never touched again.

```
omacharts chart indicator add vwap --anchor month --bands 1,2 --band-alpha 0.3
omacharts chart indicator add volume_profile --rows auto --color Violet --poc-color Amber
omacharts chart indicator set rsi --period 21 --overbought 80
```

`set` reconfigures one already on the chart. If the chart has two of a kind it
asks which, with `--id`.

A parameter an indicator has no use for is refused rather than ignored —
`--period` on a volume profile is an error, not a no-op, because silence would
read as the command having worked.

Whether a pointer on one chart draws a line on the ones linked to it:

```
omacharts chart crosshair off
```

## Worked example: the top US semiconductors in their own watchlist

This is a real session, run exactly as written.

```
$ omacharts watchlist create Semis
created watchlist "Semis" (id 3)
  [exit 0]

$ omacharts watchlist add Semis NVDA AMD AVGO TSM MU
added 5 to "Semis": NVDA AMD AVGO TSM MU
  [exit 0]

$ omacharts watchlist show Semis
Semis

  (no section)
    NVDA
    AMD
    AVGO
    TSM
    MU
  [exit 0]
```

Did not know the tickers? Search for them first:

```
$ omacharts symbol search semiconductor --limit 3
SMH        VanEck Semiconductor ETF           ETF      NASDAQ
SOXX       iShares Semiconductor ETF          ETF      NASDAQ
TSM        Taiwan Semiconductor ADR           Stock    NYSE
  [exit 0]
```

Every command takes `--json` when you would rather parse it:

```
$ omacharts watchlist show Semis --json
{"id":3,"name":"Semis","sections":[{"id":7,"name":"","root":true,"collapsed":false,
"symbols":[{"symbol":"NVDA","suffix":null,"display":"NVDA"}, ...]}]}
```

## Failure is unambiguous

A name that does not exist and a command that was misspelled fail differently,
so a caller can tell them apart without reading English:

```
$ omacharts watchlist show Nope
omacharts: no watchlist called "Nope"; try `omacharts watchlist list`
  [exit 3]

$ omacharts watchlist addd Semis NVDA
error: unrecognized subcommand 'addd'

  tip: a similar subcommand exists: 'add'
  [exit 2]
```

Mutations say what they changed rather than succeeding silently, so you can
confirm your own work without a second query.

Adding a symbol twice is not an error. Deleting something that is not there
is reported as not found rather than passing quietly.

## Setting up an arrangement of charts

```
omacharts chartbook create Semis --watchlist Semis --symbol NVDA --switch
omacharts chart split horizontal
omacharts chart set --symbol AMD --resolution 1h
omacharts chart list
```

`chart list` prints the id of each chart, which is what `--chart` takes.
Commands act on the focused chart of the open chartbook unless you say
otherwise.

A `chart set` with one bad value changes nothing at all — everything is
checked before anything is written, so you never get a half-applied chart.

## The command groups

| Group | What it covers |
|-------|----------------|
| `status` | what the app has open right now |
| `symbol` | searching the instrument inventory |
| `watchlist` | watchlists and the symbols in them |
| `section` | the named groups inside a watchlist |
| `chartbook` | saved arrangements of charts |
| `chart` | the charts inside a chartbook |
| `config` | stored preferences |
| `cache` | the cached market data |

`omacharts <group> --help` and `omacharts <group> <command> --help` both work,
as does `omacharts help <group> <command>`.
