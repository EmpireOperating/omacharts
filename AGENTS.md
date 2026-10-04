# Working on Omacharts

## Every feature ships with its command

Anything the app can do, `omacharts` can do from a terminal, in the same
change that adds it. A feature with no command is not finished.

This is not a nice-to-have that fell out of building a CLI. It is the point:
a capability you can only reach by clicking cannot be scripted, cannot be
tested from the outside, and cannot be used by an agent at all. The moment
one feature is exempt, the CLI stops being something anybody can rely on,
and the next person has no reason to keep it true either.

### What needs a command, and what does not

In scope — anything that changes **what you are looking at**, or **what is
stored as content or structure**:

- watchlists, their sections, and the symbols in them
- chartbooks: creating, renaming, deleting, switching
- chart layouts: splitting, closing, which chart holds what
- a chart's symbol, resolution, indicators, bar style, session, link group
- which watchlist a chartbook shows
- settings that are real preferences
- cache management

Out of scope — **presentational geometry**, which only means anything while a
window is on screen:

- sidebar width, split ratios, divider positions, indicator pane heights
- which pane is maximized
- scroll and zoom position

Those exist to be dragged with a mouse. A command to set one to 289 pixels is
noise in the help output that makes the useful commands harder to find.

The test to apply: **if a person would reasonably want to script it, or an
agent would need it to set up a working arrangement, it is in.** If it only
exists because something had to have a number, it is out. Note that splitting
a chart is firmly in and the *ratio* of the split is out — building a two-by-
two of particular symbols is worth scripting; nudging a divider to 47% is not.

## Commands act on what you are looking at

Every chart and chartbook verb defaults to the focused chart in the open
chartbook. That is what makes "add an RSI to this" work in one command rather
than a `status` call, a parse, and an identifier threaded into a second one —
and an agent that has to guess an identifier will eventually guess wrong.

Two rules keep that honest, and a new verb has to follow both:

- **Say what it acted on.** With an implicit target, naming the chart in the
  output is the only way anyone catches it reaching the wrong one. "added
  SMA(200) on pos:0 AAPL 1D in Macro", not "ok".
- **Refuse rather than fall back.** No window open means no focused chart.
  That is `EXIT_NO_WINDOW`, with its own message, never an answer taken from
  what was stored when the window last closed — an agent cannot tell stale
  from live and will act on it.

A chart is named back as `pos:N`, its position in the arrangement, not its id.
`materialise_book` hands out fresh pane ids every time it rebuilds, so an id is
only good until the next rebuild. Positions survive. Anything that reports a
chart, or remembers one across a rebuild, has to use the position.

## Where the surface is defined

`src/cli/spec.rs`, and nowhere else. One table describes every command, and
the parser, `--help`, the man page, the shell completions and the JSON surface
are all built from it. A description of a parser kept beside the parser is
wrong by the second release.

The authoritative machine-readable description is:

```
omacharts surface --json
```

That is what anything driving this from a script should read — every command,
every argument and flag with its type, the values enumerated ones accept, the
exit codes, and a worked example per command. `--help` is the path for people
and is held to the same standard, but prose cannot say that a flag takes an
integer without ambiguity, and the JSON can.

## Adding a command

1. Add a `Verb` to the right `Noun` in `src/cli/spec.rs`. Give it a real
   `example` — an invocation that works exactly as written, because examples
   are what an agent copies.
2. Set `writes` if it changes anything, and `workspace` if what it changes is
   the stored arrangement of charts rather than a database table. Those two
   flags are what decide whether a window that is open gets flushed before the
   command and refreshed after it.
3. Add the arm in `src/cli/exec.rs`.
4. Return text, never print. The same code answers a terminal in this process
   and a terminal in somebody else's.

Nothing else needs touching. `cargo test` fails if the table and the parser
disagree, if a verb has no example, if an example names the wrong command, or
if the enumerated values drift from what the engine actually accepts.

## How a command reaches a window that is already open

GTK hands a second invocation's arguments to the instance already running, and
`GApplicationCommandLine` carries that instance's output and exit status back
to the terminal that typed it. So a command runs **inside the app** when there
is one, which is what makes a watchlist created in a terminal appear in the
rail immediately.

With nothing running, the same command runs in the invoking process against
the database, with no GTK and no display. That is what makes it work over ssh.

`src/main.rs` decides between the two by asking the session bus whether the
application id is owned. Nothing polls and nothing watches a file: a command
is a push, and costs exactly nothing until one arrives.

Two rules follow, and breaking either is how this gets slow:

- **Never add a timer or a file watch** to notice external changes. The app is
  told; it does not look.
- **Refresh what changed, not everything.** Rebuilding the window because a
  watchlist gained a symbol is fine with five symbols and stutters with five
  hundred.

## Verifying parity

Before calling a feature done:

```
omacharts surface --json | jq -r '.commands[].command'
```

Read that list against what the UI can do. If the feature you just added is
not in it, and it is not presentational geometry, it is not finished.
