//! What the command line can do, written down once.
//!
//! The parser, `--help`, the JSON surface, the shell completions and the man
//! page are all built from the table below rather than maintained beside each
//! other. A description of a parser that lives apart from the parser is wrong
//! by the second release, and the whole point of this CLI is that something
//! which cannot ask questions can rely on what it is told.
//!
//! Adding a command means adding a [`Verb`] here and an arm in
//! [`crate::cli::exec`]. Nothing else has to be touched, and nothing else is
//! allowed to describe the surface.

/// A positional argument.
pub struct Arg {
    pub name: &'static str,
    pub help: &'static str,
    pub required: bool,
    /// Takes the rest of the line, like a list of symbols.
    pub many: bool,
    /// The only values accepted, when there is a fixed set. Empty otherwise.
    pub values: &'static [&'static str],
}

impl Arg {
    const fn req(name: &'static str, help: &'static str) -> Arg {
        Arg { name, help, required: true, many: false, values: &[] }
    }
    const fn opt(name: &'static str, help: &'static str) -> Arg {
        Arg { name, help, required: false, many: false, values: &[] }
    }
    const fn many(name: &'static str, help: &'static str) -> Arg {
        Arg { name, help, required: true, many: true, values: &[] }
    }
    const fn of(mut self, values: &'static [&'static str]) -> Arg {
        self.values = values;
        self
    }
}

/// A named option. A `value` of `None` is a switch.
pub struct Flag {
    pub long: &'static str,
    pub value: Option<&'static str>,
    pub help: &'static str,
    pub values: &'static [&'static str],
}

impl Flag {
    const fn switch(long: &'static str, help: &'static str) -> Flag {
        Flag { long, value: None, help, values: &[] }
    }
    const fn valued(long: &'static str, value: &'static str, help: &'static str) -> Flag {
        Flag { long, value: Some(value), help, values: &[] }
    }
    const fn of(mut self, values: &'static [&'static str]) -> Flag {
        self.values = values;
        self
    }
}

pub struct Verb {
    pub name: &'static str,
    pub about: &'static str,
    pub args: &'static [Arg],
    pub flags: &'static [Flag],
    /// One real invocation. Examples are what an agent copies, so every verb
    /// has one and it has to be a command that works as written.
    pub example: &'static str,
    /// Offers `--json`.
    pub json: bool,
    /// Changes something. A window that is open has to be told, which is what
    /// [`crate::cli::Live`] is for — and it is why this flag exists here
    /// rather than being inferred from the verb's name.
    pub writes: bool,
    /// Reads or writes the stored arrangement of charts rather than the
    /// database tables, so a running window has to flush it first.
    pub workspace: bool,
}

pub struct Noun {
    pub name: &'static str,
    pub about: &'static str,
    pub verbs: &'static [Verb],
}

/// How a watchlist, section or chartbook is named on the command line.
pub const SELECTOR: &str =
    "a name, case-insensitive, or `id:N` when two of them share one";

const STYLES: &[&str] = &["candles", "ohlc"];
const SESSIONS: &[&str] = &["regular", "extended"];
const SPLITS: &[&str] = &["horizontal", "vertical"];

pub const SURFACE: &[Noun] = &[
    Noun {
        name: "symbol",
        about: "Search the instrument inventory",
        verbs: &[
            Verb {
                name: "search",
                about: "Find instruments matching a query, best first",
                args: &[Arg::req("QUERY", "what to look for: a ticker or part of a name")],
                flags: &[Flag::valued("limit", "N", "how many to show (default 10)")],
                example: "omacharts symbol search semiconductor --limit 5",
                json: true,
                writes: false,
                workspace: false,
            },
            Verb {
                name: "show",
                about: "Everything known about one instrument",
                args: &[
                    Arg::req("SYMBOL", "the canonical ticker, without a venue suffix"),
                    Arg::opt("SUFFIX", "the venue suffix for a listing abroad, such as DE"),
                ],
                flags: &[],
                example: "omacharts symbol show SAP DE",
                json: true,
                writes: false,
                workspace: false,
            },
        ],
    },
    Noun {
        name: "watchlist",
        about: "Watchlists and the symbols in them",
        verbs: &[
            Verb {
                name: "list",
                about: "Every watchlist, in the order the app shows them",
                args: &[],
                flags: &[],
                example: "omacharts watchlist list --json",
                json: true,
                writes: false,
                workspace: false,
            },
            Verb {
                name: "show",
                about: "One watchlist's sections and symbols",
                args: &[Arg::opt("LIST", SELECTOR)],
                flags: &[],
                example: "omacharts watchlist show Semis",
                json: true,
                writes: false,
                workspace: false,
            },
            Verb {
                name: "create",
                about: "Make an empty watchlist",
                args: &[Arg::req("NAME", "what to call it")],
                flags: &[],
                example: "omacharts watchlist create Semis",
                json: true,
                writes: true,
                workspace: false,
            },
            Verb {
                name: "rename",
                about: "Give a watchlist a different name",
                args: &[Arg::req("LIST", SELECTOR), Arg::req("NAME", "the new name")],
                flags: &[],
                example: "omacharts watchlist rename Semis Semiconductors",
                json: true,
                writes: true,
                workspace: false,
            },
            Verb {
                name: "delete",
                about: "Remove a watchlist and everything in it",
                args: &[Arg::req("LIST", SELECTOR)],
                flags: &[],
                example: "omacharts watchlist delete Semis",
                json: true,
                writes: true,
                workspace: false,
            },
            Verb {
                name: "add",
                about: "Put symbols in a watchlist",
                args: &[Arg::req("LIST", SELECTOR), Arg::many("SYMBOL", "one or more tickers")],
                flags: &[
                    Flag::valued("section", "NAME", "put them in this section rather than at the top"),
                    Flag::valued("suffix", "S", "venue suffix, applied to every symbol given"),
                ],
                example: "omacharts watchlist add Semis NVDA AMD AVGO TSM MU",
                json: true,
                writes: true,
                workspace: false,
            },
            Verb {
                name: "remove",
                about: "Take symbols out of a watchlist",
                args: &[Arg::req("LIST", SELECTOR), Arg::many("SYMBOL", "one or more tickers")],
                flags: &[
                    Flag::valued("section", "NAME", "only from this section"),
                    Flag::valued("suffix", "S", "venue suffix, applied to every symbol given"),
                ],
                example: "omacharts watchlist remove Semis MU",
                json: true,
                writes: true,
                workspace: false,
            },
            Verb {
                name: "feed",
                about: "The default watchlist with quotes, as the bar widget reads it",
                args: &[],
                flags: &[Flag::switch("refresh", "fetch quotes that have gone stale first")],
                example: "omacharts watchlist feed --refresh",
                json: true,
                writes: false,
                workspace: false,
            },
        ],
    },
    Noun {
        name: "section",
        about: "The named groups inside a watchlist",
        verbs: &[
            Verb {
                name: "list",
                about: "The sections of one watchlist",
                args: &[Arg::opt("LIST", SELECTOR)],
                flags: &[],
                example: "omacharts section list Default",
                json: true,
                writes: false,
                workspace: false,
            },
            Verb {
                name: "create",
                about: "Add a section to a watchlist",
                args: &[Arg::req("LIST", SELECTOR), Arg::req("NAME", "what to call it")],
                flags: &[],
                example: "omacharts section create Default Energy",
                json: true,
                writes: true,
                workspace: false,
            },
            Verb {
                name: "rename",
                about: "Give a section a different name",
                args: &[
                    Arg::req("LIST", SELECTOR),
                    Arg::req("SECTION", SELECTOR),
                    Arg::req("NAME", "the new name"),
                ],
                example: "omacharts section rename Default Energy Oil",
                flags: &[],
                json: true,
                writes: true,
                workspace: false,
            },
            Verb {
                name: "delete",
                about: "Remove a section and the symbols in it",
                args: &[Arg::req("LIST", SELECTOR), Arg::req("SECTION", SELECTOR)],
                flags: &[],
                example: "omacharts section delete Default Energy",
                json: true,
                writes: true,
                workspace: false,
            },
            Verb {
                name: "promote",
                about: "Turn a section into a watchlist of its own",
                args: &[Arg::req("LIST", SELECTOR), Arg::req("SECTION", SELECTOR)],
                flags: &[],
                example: "omacharts section promote Default Energy",
                json: true,
                writes: true,
                workspace: false,
            },
        ],
    },
    Noun {
        name: "chartbook",
        about: "Saved arrangements of charts",
        verbs: &[
            Verb {
                name: "list",
                about: "Every chartbook, and which one is open",
                args: &[],
                flags: &[],
                example: "omacharts chartbook list --json",
                json: true,
                writes: false,
                workspace: true,
            },
            Verb {
                name: "show",
                about: "One chartbook: its charts, their symbols and its watchlist",
                args: &[Arg::opt("BOOK", SELECTOR)],
                flags: &[],
                example: "omacharts chartbook show Macro",
                json: true,
                writes: false,
                workspace: true,
            },
            Verb {
                name: "create",
                about: "Make a chartbook holding one chart",
                args: &[Arg::req("NAME", "what to call it")],
                flags: &[
                    Flag::valued("watchlist", "LIST", "the watchlist it shows (default: the current one)"),
                    Flag::valued("symbol", "SYMBOL", "what its first chart opens on"),
                    Flag::switch("switch", "make it the open chartbook"),
                ],
                example: "omacharts chartbook create Semis --watchlist Semis --symbol NVDA --switch",
                json: true,
                writes: true,
                workspace: true,
            },
            Verb {
                name: "rename",
                about: "Give a chartbook a different name",
                args: &[Arg::req("BOOK", SELECTOR), Arg::req("NAME", "the new name")],
                flags: &[],
                example: "omacharts chartbook rename Macro Rates",
                json: true,
                writes: true,
                workspace: true,
            },
            Verb {
                name: "delete",
                about: "Remove a chartbook and its charts",
                args: &[Arg::req("BOOK", SELECTOR)],
                flags: &[],
                example: "omacharts chartbook delete Semis",
                json: true,
                writes: true,
                workspace: true,
            },
            Verb {
                name: "switch",
                about: "Open a chartbook",
                args: &[Arg::req("BOOK", SELECTOR)],
                flags: &[],
                example: "omacharts chartbook switch Macro",
                json: true,
                writes: true,
                workspace: true,
            },
            Verb {
                name: "watchlist",
                about: "Choose which watchlist a chartbook shows",
                args: &[Arg::req("BOOK", SELECTOR), Arg::req("LIST", SELECTOR)],
                flags: &[],
                example: "omacharts chartbook watchlist Semis Semis",
                json: true,
                writes: true,
                workspace: true,
            },
        ],
    },
    Noun {
        name: "chart",
        about: "The charts inside a chartbook",
        verbs: &[
            Verb {
                name: "list",
                about: "The charts in a chartbook, with the ids the other verbs take",
                args: &[],
                flags: &[Flag::valued("book", "BOOK", "which chartbook (default: the open one)")],
                example: "omacharts chart list --json",
                json: true,
                writes: false,
                workspace: true,
            },
            Verb {
                name: "split",
                about: "Divide a chart in two, copying what it shows",
                args: &[Arg::req("DIRECTION", "which way to divide it").of(SPLITS)],
                flags: &[
                    Flag::valued("book", "BOOK", "which chartbook (default: the open one)"),
                    Flag::valued("chart", "ID", "which chart (default: the focused one)"),
                ],
                example: "omacharts chart split horizontal",
                json: true,
                writes: true,
                workspace: true,
            },
            Verb {
                name: "close",
                about: "Remove a chart, giving its space to its neighbour",
                args: &[],
                flags: &[
                    Flag::valued("book", "BOOK", "which chartbook (default: the open one)"),
                    Flag::valued("chart", "ID", "which chart (default: the focused one)"),
                ],
                example: "omacharts chart close --chart 2",
                json: true,
                writes: true,
                workspace: true,
            },
            Verb {
                name: "focus",
                about: "Make a chart the one the keyboard and the menus act on",
                args: &[Arg::req("ID", "the chart's id, from `chart list`")],
                flags: &[Flag::valued("book", "BOOK", "which chartbook (default: the open one)")],
                example: "omacharts chart focus 2",
                json: true,
                writes: true,
                workspace: true,
            },
            Verb {
                name: "set",
                about: "Change what a chart shows",
                args: &[],
                flags: &[
                    Flag::valued("book", "BOOK", "which chartbook (default: the open one)"),
                    Flag::valued("chart", "ID", "which chart (default: the focused one)"),
                    Flag::valued("symbol", "SYMBOL", "the instrument to chart"),
                    Flag::valued("suffix", "S", "its venue suffix, for a listing abroad"),
                    Flag::valued("resolution", "TF", "such as 5m, 1h, 1D, 1W"),
                    Flag::valued("style", "STYLE", "how bars are drawn").of(STYLES),
                    Flag::valued("session", "SESSION", "which hours to include").of(SESSIONS),
                    Flag::valued("link", "GROUP", "link group 1-9, or `none` to unlink"),
                    Flag::valued("grid", "BOOL", "draw the grid").of(&["on", "off"]),
                ],
                example: "omacharts chart set --symbol NVDA --resolution 1h --style candles",
                json: true,
                writes: true,
                workspace: true,
            },
            Verb {
                name: "indicator",
                about: "Add, remove or list a chart's indicators",
                args: &[
                    Arg::req("ACTION", "what to do").of(&["list", "add", "remove"]),
                    Arg::opt("KIND", "the indicator, for add and remove"),
                ],
                flags: &[
                    Flag::valued("book", "BOOK", "which chartbook (default: the open one)"),
                    Flag::valued("chart", "ID", "which chart (default: the focused one)"),
                ],
                example: "omacharts chart indicator add rsi",
                json: true,
                writes: true,
                workspace: true,
            },
        ],
    },
    Noun {
        name: "config",
        about: "Stored preferences",
        verbs: &[
            Verb {
                name: "list",
                about: "Every setting that has been written, and its value",
                args: &[],
                flags: &[],
                example: "omacharts config list --json",
                json: true,
                writes: false,
                workspace: false,
            },
            Verb {
                name: "get",
                about: "One setting's value",
                args: &[Arg::req("KEY", "the setting's name, from `config list`")],
                flags: &[],
                example: "omacharts config get theme",
                json: true,
                writes: false,
                workspace: false,
            },
            Verb {
                name: "set",
                about: "Write a setting",
                args: &[Arg::req("KEY", "the setting's name"), Arg::req("VALUE", "what to set it to")],
                flags: &[],
                example: "omacharts config set theme omarchy",
                json: true,
                writes: true,
                workspace: false,
            },
        ],
    },
    Noun {
        name: "cache",
        about: "The cached market data",
        verbs: &[
            Verb {
                name: "status",
                about: "How much the cache holds, against its limit",
                args: &[],
                flags: &[],
                example: "omacharts cache status --json",
                json: true,
                writes: false,
                workspace: false,
            },
            Verb {
                name: "clear",
                about: "Throw away every cached series. Settings and watchlists stay",
                args: &[],
                flags: &[],
                example: "omacharts cache clear",
                json: true,
                writes: true,
                workspace: false,
            },
            Verb {
                name: "limit",
                about: "Read or set the size the cache is pruned back under",
                args: &[Arg::opt("SIZE", "such as 512MB or 2GB; omit to read it")],
                flags: &[],
                example: "omacharts cache limit 2GB",
                json: true,
                writes: true,
                workspace: false,
            },
        ],
    },
];

/// The values this table offers have to be the values the engine accepts.
///
/// Written out rather than built from the engine because a `const` cannot
/// call anything — so this is the test that keeps the two honest. Offering a
/// style that does not exist would tell an agent to try something that can
/// only ever fail.
/// Find a verb by the pair of names the parser matched.
pub fn verb(noun: &str, verb: &str) -> Option<&'static Verb> {
    SURFACE
        .iter()
        .find(|n| n.name == noun)?
        .verbs
        .iter()
        .find(|v| v.name == verb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use omacharts_engine::{BarStyle, Session};

    #[test]
    fn the_bar_styles_on_offer_are_the_ones_that_exist() {
        let engine: Vec<&str> = BarStyle::ALL.iter().map(|s| s.key()).collect();
        assert_eq!(STYLES, engine.as_slice());
    }

    #[test]
    fn the_sessions_on_offer_are_the_ones_that_exist() {
        let mut engine: Vec<&str> = Session::ALL.iter().map(|s| s.key()).collect();
        let mut offered = SESSIONS.to_vec();
        engine.sort_unstable();
        offered.sort_unstable();
        assert_eq!(offered, engine);
    }

    #[test]
    fn no_two_commands_share_a_name() {
        for noun in SURFACE {
            let mut seen: Vec<&str> = noun.verbs.iter().map(|v| v.name).collect();
            let before = seen.len();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), before, "{} has a duplicate verb", noun.name);
        }
    }
}
