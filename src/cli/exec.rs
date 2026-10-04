//! Carrying out a command.
//!
//! One arm per verb in [`super::spec`], and nothing here decides what the
//! surface is — the table does. Adding a command means adding it there and
//! here, and the test at the bottom of `parser` fails if only one of the two
//! happened.
//!
//! Every arm returns text rather than printing, because the same code answers
//! a terminal in this process and a terminal in somebody else's. See
//! [`super::Outcome`].

use serde_json::{json, Value};

use omacharts_engine::indicators::{LineStyle, Stroke};
use omacharts_engine::theme::{ColorChoice, SWATCH_NAMES};
use omacharts_engine::{BarStyle, Indicator, IndicatorKind, Reset, Session, Timeframe};

use super::charts::{self, Workspace};
use super::{parser, Fault, Live, Outcome, EXIT_USAGE};
use crate::store::{Section, Store, DEFAULT_WATCHLIST};

/// Parse and run.
pub fn dispatch(args: &[String], store: &Store, live: Option<&dyn Live>) -> Outcome {
    // `omacharts watchlist [--refresh]` was the whole command line once, and
    // it is what the bar widget installed on people's desktops still runs on a
    // timer. `watchlist` grew subcommands around it; this keeps the bare form
    // meaning what it has always meant rather than answering a widget with a
    // usage error.
    if args.get(1).map(String::as_str) == Some("watchlist")
        && args.iter().skip(2).all(|arg| arg == "--refresh")
    {
        return Outcome::ok(format!(
            "{}\n",
            super::watchlist_json(args.iter().any(|arg| arg == "--refresh"))
        ));
    }

    let parsed = match parser::command().try_get_matches_from(args) {
        Ok(parsed) => parsed,
        // Clap's own rendering, which is where "did you mean" and the usage
        // line come from. A help or version request is a success that happens
        // to arrive as an error.
        Err(error) => {
            let text = error.render().to_string();
            return match error.use_stderr() {
                false => Outcome::ok(text),
                true => Outcome { code: EXIT_USAGE, out: String::new(), err: text },
            };
        }
    };

    let Some((noun, rest)) = parsed.subcommand() else {
        return Outcome::ok(parser::command().render_help().to_string());
    };

    if noun == "help" {
        return help_for(rest.get_many::<String>("COMMAND").map(|c| c.cloned().collect()));
    }
    if noun == "surface" {
        return surface(rest);
    }

    let Some((verb, m)) = rest.subcommand() else {
        return Outcome::ok(parser::command().render_help().to_string());
    };
    let Some(spec) = super::spec::verb(noun, verb) else {
        return Outcome::failed(Fault::usage(format!("no such command: {noun} {verb}")));
    };

    // A command that reads the arrangement has to see what is on screen, not
    // what was saved a moment ago; one that writes it must not be overwritten
    // by the next save. Both are the same hand-off, so both happen here
    // rather than in every arm that could forget.
    if let Some(live) = live.filter(|_| spec.workspace) {
        live.flush_workspace();
    }

    let json = flag(m, "json");
    let result = match (noun, verb) {
        ("symbol", "search") => symbol_search(m, json),
        ("symbol", "show") => symbol_show(m, json),

        ("watchlist", "list") => watchlist_list(store, json),
        ("watchlist", "show") => watchlist_show(store, m, json),
        ("watchlist", "create") => watchlist_create(store, m, json),
        ("watchlist", "rename") => watchlist_rename(store, m, json),
        ("watchlist", "delete") => watchlist_delete(store, m, json),
        ("watchlist", "add") => watchlist_add(store, m, json, true),
        ("watchlist", "remove") => watchlist_add(store, m, json, false),
        ("watchlist", "feed") => Ok(super::watchlist_json(flag(m, "refresh"))),

        ("section", "list") => section_list(store, m, json),
        ("section", "create") => section_create(store, m, json),
        ("section", "rename") => section_rename(store, m, json),
        ("section", "delete") => section_delete(store, m, json),
        ("section", "promote") => section_promote(store, m, json),

        ("status", "show") => status(store, live.is_some(), json),
        ("chart", "crosshair") => crosshair(store, m, json),
        ("chartbook", _) | ("chart", _) => {
            // "The one I am looking at" needs something to be looking at. The
            // stored arrangement is what the window had when it last closed,
            // and an agent cannot tell that from what is on screen now — so a
            // command with no target is refused rather than answered from it.
            if live.is_none() && !names_a_target(noun, verb, m) {
                return Outcome::failed(Fault::no_window(match noun {
                    "chartbook" => "open chartbook",
                    _ => "focused chart",
                }));
            }
            charts_verb(store, noun, verb, m, json)
        }

        ("config", "list") => config_list(store, json),
        ("config", "get") => config_get(store, m, json),
        ("config", "set") => config_set(store, m, json),

        ("cache", "status") => cache_status(store, json),
        ("cache", "clear") => cache_clear(store, json),
        ("cache", "limit") => cache_limit(store, m, json),

        _ => Err(Fault::usage(format!("no such command: {noun} {verb}"))),
    };

    match result {
        Err(fault) => Outcome::failed(fault),
        Ok(out) => {
            // Only after it worked, and only what it touched. Rebuilding the
            // window because a setting changed would be the kind of thing
            // that is fine with five symbols and stutters with five hundred.
            if let Some(live) = live.filter(|_| spec.writes) {
                match spec.workspace {
                    true => live.reload_workspace(),
                    false => live.reload_watchlists(),
                }
            }
            Outcome::ok(out)
        }
    }
}

/// Did the command say which chartbook or chart it meant?
///
/// Verbs that take one as a required argument always have; the rest default
/// to what is on screen, and that default only exists while something is.
fn names_a_target(noun: &str, verb: &str, m: &clap::ArgMatches) -> bool {
    if arg(m, "BOOK").is_some() || arg(m, "book").is_some() {
        return true;
    }
    // Creating one invents its own target, and listing them all needs none.
    matches!((noun, verb), ("chartbook", "create") | ("chartbook", "list"))
}

fn help_for(path: Option<Vec<String>>) -> Outcome {
    let mut cmd = parser::command();
    let Some(path) = path.filter(|p| !p.is_empty()) else {
        return Outcome::ok(cmd.render_help().to_string());
    };
    for step in &path {
        let found = cmd.get_subcommands().find(|c| c.get_name() == step).cloned();
        match found {
            Some(next) => cmd = next,
            None => {
                return Outcome::failed(Fault::usage(format!(
                    "no such command: {}; try `omacharts --help`",
                    path.join(" ")
                )))
            }
        }
    }
    Outcome::ok(cmd.render_help().to_string())
}

fn surface(m: &clap::ArgMatches) -> Outcome {
    if let Some(shell) = arg(m, "completions") {
        return Outcome::ok(completions(shell));
    }
    if flag(m, "man") {
        return Outcome::ok(man_page());
    }
    Outcome::ok(format!("{}\n", parser::surface_json()))
}

// -- symbols --------------------------------------------------------------

fn symbol_search(m: &clap::ArgMatches, as_json: bool) -> Result<String, Fault> {
    let query = arg(m, "QUERY").expect("required");
    let limit: usize = m
        .get_one::<String>("limit")
        .map(|l| l.parse().unwrap_or(10))
        .unwrap_or(10);
    let index = crate::inventory::everything();
    let hits: Vec<&omacharts_engine::Instrument> =
        index.search(query, limit).iter().filter_map(|h| index.get(h.index)).collect();
    if hits.is_empty() {
        return Err(Fault::not_found(format!("nothing matches {query:?}")));
    }
    if as_json {
        return Ok(wrap_list("symbols", hits.iter().map(|i| instrument_json(i)).collect()));
    }
    Ok(hits
        .iter()
        .map(|i| {
            format!(
                "{:<10} {:<34} {:<8} {}",
                i.display_symbol(),
                truncate(&i.name, 34),
                i.kind.label(),
                i.exchange_label().unwrap_or("")
            )
            .trim_end()
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n")
}

fn symbol_show(m: &clap::ArgMatches, as_json: bool) -> Result<String, Fault> {
    let symbol = arg(m, "SYMBOL").expect("required").to_uppercase();
    let suffix = arg(m, "SUFFIX").map(|s| s.to_uppercase());
    let index = crate::inventory::everything();
    let found = index.find(&symbol, suffix.as_deref()).ok_or_else(|| {
        Fault::not_found(format!(
            "no instrument called {}; try `omacharts symbol search {symbol}`",
            spell(&symbol, suffix.as_deref())
        ))
    })?;
    if as_json {
        return Ok(format!("{}\n", instrument_json(found)));
    }
    Ok(format!(
        "{}\n{}\nkind      {}\nexchange  {}\ncurrency  {}\n",
        found.display_symbol(),
        found.name,
        found.kind.label(),
        found.exchange_label().unwrap_or("unknown"),
        found.currency.as_deref().unwrap_or("unknown"),
    ))
}

fn instrument_json(i: &omacharts_engine::Instrument) -> String {
    json!({
        "symbol": i.symbol,
        "suffix": i.suffix,
        "display": i.display_symbol(),
        "name": i.name,
        "kind": i.kind.label(),
        "exchange": i.exchange_label(),
        "currency": i.currency,
    })
    .to_string()
}

// -- watchlists -----------------------------------------------------------

/// Resolve `a name, or id:N` against the watchlists.
fn find_list(store: &Store, selector: &str) -> Result<(i64, String), Fault> {
    let lists = store.watchlists();
    if let Some(rest) = selector.strip_prefix("id:") {
        let id: i64 = rest
            .parse()
            .map_err(|_| Fault::usage(format!("{selector:?} is not a watchlist id")))?;
        return lists
            .iter()
            .find(|(i, _)| *i == id)
            .cloned()
            .ok_or_else(|| Fault::not_found(format!("no watchlist with id {id}")));
    }
    let wanted = selector.to_lowercase();
    let hits: Vec<&(i64, String)> =
        lists.iter().filter(|(_, name)| name.to_lowercase() == wanted).collect();
    match hits.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(Fault::not_found(format!(
            "no watchlist called {selector:?}; try `omacharts watchlist list`"
        ))),
        many => Err(Fault::ambiguous(format!(
            "{} watchlists are called {selector:?}; use {}",
            many.len(),
            many.iter().map(|(i, _)| format!("id:{i}")).collect::<Vec<_>>().join(" or ")
        ))),
    }
}

/// The watchlist a command acts on when none was named.
fn list_or_default(store: &Store, m: &clap::ArgMatches) -> Result<(i64, String), Fault> {
    match arg(m, "LIST") {
        Some(selector) => find_list(store, selector),
        None => find_list(store, &format!("id:{DEFAULT_WATCHLIST}")),
    }
}

fn watchlist_list(store: &Store, as_json: bool) -> Result<String, Fault> {
    let lists = store.watchlists();
    if as_json {
        let rows = lists
            .iter()
            .map(|(id, name)| {
                let counted: usize =
                    store.watchlist_sections(*id).iter().map(|s| s.entries.len()).sum();
                json!({
                    "id": id,
                    "name": name,
                    "symbols": counted,
                    "isDefault": *id == DEFAULT_WATCHLIST,
                })
                .to_string()
            })
            .collect();
        return Ok(wrap_list("watchlists", rows));
    }
    Ok(lists
        .iter()
        .map(|(id, name)| {
            let counted: usize =
                store.watchlist_sections(*id).iter().map(|s| s.entries.len()).sum();
            let tag = if *id == DEFAULT_WATCHLIST { "  (default)" } else { "" };
            format!("{id:<4} {name:<24} {counted:>4} symbols{tag}")
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n")
}

fn watchlist_show(store: &Store, m: &clap::ArgMatches, as_json: bool) -> Result<String, Fault> {
    let (id, name) = list_or_default(store, m)?;
    let sections = store.watchlist_sections(id);
    if as_json {
        return Ok(format!(
            "{}\n",
            json!({ "id": id, "name": name, "sections": sections_json(&sections) })
        ));
    }
    let mut out = format!("{name}\n");
    for section in &sections {
        let title = if section.root { "(no section)" } else { &section.name };
        out.push_str(&format!("\n  {title}\n"));
        for entry in &section.entries {
            out.push_str(&format!("    {}\n", spell(&entry.symbol, entry.suffix.as_deref())));
        }
    }
    Ok(out)
}

fn sections_json(sections: &[Section]) -> Value {
    Value::Array(
        sections
            .iter()
            .map(|s| {
                json!({
                    "id": s.id,
                    "name": s.name,
                    "root": s.root,
                    "collapsed": s.collapsed,
                    "symbols": s.entries.iter().map(|e| json!({
                        "symbol": e.symbol,
                        "suffix": e.suffix,
                        "display": spell(&e.symbol, e.suffix.as_deref()),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

fn watchlist_create(store: &Store, m: &clap::ArgMatches, as_json: bool) -> Result<String, Fault> {
    let name = arg(m, "NAME").expect("required");
    let id = store
        .add_watchlist(name)
        .ok_or_else(|| Fault::new(super::EXIT_ERROR, "could not create the watchlist".into()))?;
    said(as_json, json!({"id": id, "name": name}), format!("created watchlist {name:?} (id {id})"))
}

fn watchlist_rename(store: &Store, m: &clap::ArgMatches, as_json: bool) -> Result<String, Fault> {
    let (id, was) = find_list(store, arg(m, "LIST").expect("required"))?;
    let name = arg(m, "NAME").expect("required");
    store.rename_watchlist(id, name);
    said(as_json, json!({"id": id, "name": name}), format!("renamed {was:?} to {name:?}"))
}

fn watchlist_delete(store: &Store, m: &clap::ArgMatches, as_json: bool) -> Result<String, Fault> {
    let (id, name) = find_list(store, arg(m, "LIST").expect("required"))?;
    if id == DEFAULT_WATCHLIST {
        return Err(Fault::refused(format!(
            "{name:?} is the watchlist the bar widget shows and cannot be deleted; rename it instead"
        )));
    }
    store.remove_watchlist(id);
    said(as_json, json!({"id": id, "name": name}), format!("deleted watchlist {name:?}"))
}

/// Add symbols, or take them out. One function because they differ in a verb
/// and nothing else, and two would drift.
fn watchlist_add(
    store: &Store,
    m: &clap::ArgMatches,
    as_json: bool,
    adding: bool,
) -> Result<String, Fault> {
    let (id, name) = find_list(store, arg(m, "LIST").expect("required"))?;
    let suffix = arg(m, "suffix").map(|s| s.to_uppercase());
    let symbols: Vec<String> = m
        .get_many::<String>("SYMBOL")
        .expect("required")
        .map(|s| s.to_uppercase())
        .collect();

    let section = match arg(m, "section") {
        None => store.root_section(id),
        Some(wanted) => find_section(store, id, wanted)?.id,
    };

    let index = crate::inventory::everything();
    let mut done: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    for symbol in &symbols {
        if index.find(symbol, suffix.as_deref()).is_none() {
            unknown.push(spell(symbol, suffix.as_deref()));
            continue;
        }
        match adding {
            true => store.add_to_section(section, symbol, suffix.as_deref()),
            false => store.remove_from_section(section, symbol, suffix.as_deref()),
        }
        done.push(spell(symbol, suffix.as_deref()));
    }

    if done.is_empty() {
        return Err(Fault::not_found(format!(
            "none of those are instruments this knows: {}",
            unknown.join(", ")
        )));
    }
    let verb = if adding { "added" } else { "removed" };
    let preposition = if adding { "to" } else { "from" };
    let mut text = format!("{verb} {} {preposition} {name:?}: {}", done.len(), done.join(" "));
    if !unknown.is_empty() {
        text.push_str(&format!("\nskipped, not instruments: {}", unknown.join(" ")));
    }
    said(
        as_json,
        json!({"watchlist": name, "changed": done, "skipped": unknown}),
        text,
    )
}

// -- sections -------------------------------------------------------------

fn find_section(store: &Store, list: i64, selector: &str) -> Result<Section, Fault> {
    let sections = store.watchlist_sections(list);
    if let Some(rest) = selector.strip_prefix("id:") {
        let id: i64 = rest
            .parse()
            .map_err(|_| Fault::usage(format!("{selector:?} is not a section id")))?;
        return sections
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(|| Fault::not_found(format!("no section with id {id}")));
    }
    let wanted = selector.to_lowercase();
    let hits: Vec<Section> =
        sections.into_iter().filter(|s| s.name.to_lowercase() == wanted).collect();
    match hits.len() {
        1 => Ok(hits.into_iter().next().expect("one")),
        0 => Err(Fault::not_found(format!("no section called {selector:?}"))),
        n => Err(Fault::ambiguous(format!(
            "{n} sections are called {selector:?}; use {}",
            hits.iter().map(|s| format!("id:{}", s.id)).collect::<Vec<_>>().join(" or ")
        ))),
    }
}

fn section_list(store: &Store, m: &clap::ArgMatches, as_json: bool) -> Result<String, Fault> {
    let (id, name) = list_or_default(store, m)?;
    let sections = store.watchlist_sections(id);
    if as_json {
        return Ok(format!("{}\n", json!({"watchlist": name, "sections": sections_json(&sections)})));
    }
    Ok(sections
        .iter()
        .map(|s| {
            let title = if s.root { "(no section)".to_string() } else { s.name.clone() };
            format!("{:<5} {:<24} {:>3} symbols", s.id, title, s.entries.len())
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n")
}

fn section_create(store: &Store, m: &clap::ArgMatches, as_json: bool) -> Result<String, Fault> {
    let (list, list_name) = find_list(store, arg(m, "LIST").expect("required"))?;
    let name = arg(m, "NAME").expect("required");
    let id = store
        .add_section(list, name)
        .ok_or_else(|| Fault::new(super::EXIT_ERROR, "could not create the section".into()))?;
    said(
        as_json,
        json!({"id": id, "name": name, "watchlist": list_name}),
        format!("created section {name:?} in {list_name:?}"),
    )
}

fn section_rename(store: &Store, m: &clap::ArgMatches, as_json: bool) -> Result<String, Fault> {
    let (list, _) = find_list(store, arg(m, "LIST").expect("required"))?;
    let section = find_section(store, list, arg(m, "SECTION").expect("required"))?;
    let name = arg(m, "NAME").expect("required");
    store.rename_section(section.id, name);
    said(
        as_json,
        json!({"id": section.id, "name": name}),
        format!("renamed section {:?} to {name:?}", section.name),
    )
}

fn section_delete(store: &Store, m: &clap::ArgMatches, as_json: bool) -> Result<String, Fault> {
    let (list, _) = find_list(store, arg(m, "LIST").expect("required"))?;
    let section = find_section(store, list, arg(m, "SECTION").expect("required"))?;
    if section.root {
        return Err(Fault::refused(
            "that is where symbols outside a section live, and cannot be removed".into(),
        ));
    }
    store.remove_section(section.id);
    said(
        as_json,
        json!({"id": section.id, "name": section.name, "symbols": section.entries.len()}),
        format!("deleted section {:?} and its {} symbols", section.name, section.entries.len()),
    )
}

/// Turn a section into a watchlist of its own.
///
/// The symbols are copied across before the section goes, so a run that dies
/// in the middle leaves them in both places and never in neither.
fn section_promote(store: &Store, m: &clap::ArgMatches, as_json: bool) -> Result<String, Fault> {
    let (list, _) = find_list(store, arg(m, "LIST").expect("required"))?;
    let section = find_section(store, list, arg(m, "SECTION").expect("required"))?;
    if section.root {
        return Err(Fault::refused(
            "symbols outside a section have no name to give a watchlist".into(),
        ));
    }
    let id = store.add_watchlist(&section.name).ok_or_else(|| {
        Fault::new(super::EXIT_ERROR, "could not create the watchlist".into())
    })?;
    let root = store.root_section(id);
    for entry in &section.entries {
        store.add_to_section(root, &entry.symbol, entry.suffix.as_deref());
    }
    store.remove_section(section.id);
    said(
        as_json,
        json!({"id": id, "name": section.name, "symbols": section.entries.len()}),
        format!(
            "turned section {:?} into a watchlist with its {} symbols",
            section.name,
            section.entries.len()
        ),
    )
}

// -- chartbooks and charts ------------------------------------------------

fn charts_verb(
    store: &Store,
    noun: &str,
    verb: &str,
    m: &clap::ArgMatches,
    as_json: bool,
) -> Result<String, Fault> {
    let mut workspace = Workspace::load(store);
    let selector = arg(m, "BOOK").or_else(|| arg(m, "book"));

    let text = match (noun, verb) {
        ("chartbook", "list") => {
            let rows: Vec<String> = (0..workspace.books().len())
                .map(|i| {
                    let book = &workspace.books()[i];
                    json!({
                        "id": i,
                        "name": workspace.name_of(i),
                        "charts": charts::panes(book).len(),
                        "watchlist": book["watchlist"],
                        "active": i == workspace.active(),
                    })
                    .to_string()
                })
                .collect();
            return Ok(match as_json {
                true => wrap_list("chartbooks", rows),
                false => (0..workspace.books().len())
                    .map(|i| {
                        format!(
                            "{}{:<4} {:<24} {:>2} charts",
                            if i == workspace.active() { "* " } else { "  " },
                            format!("id:{i}"),
                            workspace.name_of(i),
                            charts::panes(&workspace.books()[i]).len()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n",
            });
        }
        ("chartbook", "show") | ("chart", "list") => {
            let at = workspace.resolve(selector.map(String::as_str))?;
            let book = workspace.book(at)?;
            let rows: Vec<String> = charts::panes(book).iter().map(pane_json).collect();
            return Ok(match as_json {
                true => format!(
                    "{{\"chartbook\":{},\"watchlist\":{},\"charts\":[{}]}}\n",
                    super::json_string(&workspace.name_of(at)),
                    book["watchlist"],
                    rows.join(",")
                ),
                false => {
                    let focused = book["focused"].as_u64().unwrap_or(0);
                    let mut out = format!("{}\n", workspace.name_of(at));
                    for pane in charts::panes(book) {
                        let id = pane["id"].as_u64().unwrap_or(0);
                        out.push_str(&format!(
                            "{} {:<4} {:<12} {:<6} {}\n",
                            if id == focused { "*" } else { " " },
                            id,
                            spell(
                                pane["symbol"].as_str().unwrap_or("?"),
                                pane["suffix"].as_str()
                            ),
                            pane["timeframe"].as_str().unwrap_or(""),
                            pane["bar_style"].as_str().unwrap_or(""),
                        ));
                    }
                    out
                }
            });
        }
        ("chartbook", "create") => {
            let name = arg(m, "NAME").expect("required");
            let symbol = m
                .get_one::<String>("symbol")
                .map(|s| s.to_uppercase())
                .unwrap_or_else(|| "SPY".to_string());
            let watchlist = match arg(m, "watchlist") {
                Some(selector) => Some(find_list(store, selector)?.0),
                None => None,
            };
            let id = workspace.next_pane();
            let book = json!({
                "name": name,
                "layout": {"leaf": id},
                "focused": id,
                "panes": [charts::new_pane(id, &symbol, None)],
                "watchlist": watchlist,
            });
            let at = workspace.push(book);
            if flag(m, "switch") {
                workspace.set_active(at);
            }
            format!("created chartbook {name:?} (id:{at}) showing {symbol}")
        }
        ("chartbook", "rename") => {
            let at = workspace.find(arg(m, "BOOK").expect("required"))?;
            let was = workspace.name_of(at);
            let name = arg(m, "NAME").expect("required");
            workspace.book_mut(at)["name"] = json!(name);
            format!("renamed chartbook {was:?} to {name:?}")
        }
        ("chartbook", "delete") => {
            let at = workspace.find(arg(m, "BOOK").expect("required"))?;
            if workspace.books().len() < 2 {
                return Err(Fault::refused(
                    "this is the only chartbook; there has to be one".into(),
                ));
            }
            let name = workspace.name_of(at);
            let charts = charts::panes(workspace.book(at)?).len();
            workspace.remove(at);
            format!("deleted chartbook {name:?} and its {charts} charts")
        }
        ("chartbook", "switch") => {
            let at = workspace.find(arg(m, "BOOK").expect("required"))?;
            workspace.set_active(at);
            format!("switched to chartbook {:?}", workspace.name_of(at))
        }
        ("chartbook", "watchlist") => {
            let at = workspace.find(arg(m, "BOOK").expect("required"))?;
            let (id, name) = find_list(store, arg(m, "LIST").expect("required"))?;
            workspace.book_mut(at)["watchlist"] = json!(id);
            format!("chartbook {:?} now shows watchlist {name:?}", workspace.name_of(at))
        }
        ("chart", _) => return chart_verb(store, verb, m, as_json, &mut workspace),
        _ => return Err(Fault::usage(format!("no such command: {noun} {verb}"))),
    };

    workspace.save(store);
    said(as_json, json!({"message": text}), text)
}

fn chart_verb(
    store: &Store,
    verb: &str,
    m: &clap::ArgMatches,
    as_json: bool,
    workspace: &mut Workspace,
) -> Result<String, Fault> {
    let at = workspace.resolve(arg(m, "book").map(String::as_str))?;
    let wanted = arg(m, "chart").map(String::as_str);
    let book = workspace.book(at)?.clone();
    let pane = charts::resolve_pane(&book, wanted)?;

    let text = match verb {
        "split" => {
            let horizontal =
                arg(m, "DIRECTION").map(|d| d == "horizontal").unwrap_or(true);
            let added = workspace.next_pane();
            let from = charts::panes(&book)
                .iter()
                .find(|p| p["id"].as_u64() == Some(pane as u64))
                .cloned()
                .unwrap_or_else(|| charts::new_pane(added, "SPY", None));
            let target = workspace.book_mut(at);
            target["layout"] = charts::split_leaf(&target["layout"], pane, added, horizontal);
            target["panes"]
                .as_array_mut()
                .expect("panes is an array")
                .push(charts::copy_pane(&from, added));
            target["focused"] = json!(added);
            format!(
                "split chart {pane} {}; the new chart is {added}",
                if horizontal { "horizontally" } else { "vertically" }
            )
        }
        "close" => {
            let Some(pruned) = charts::remove_leaf(&book["layout"], pane) else {
                return Err(Fault::refused(
                    "this is the only chart in the chartbook; there has to be one".into(),
                ));
            };
            let kept = charts::leaves(&pruned);
            let target = workspace.book_mut(at);
            target["layout"] = pruned;
            target["panes"]
                .as_array_mut()
                .expect("panes is an array")
                .retain(|p| p["id"].as_u64().is_some_and(|id| kept.contains(&(id as u32))));
            target["focused"] = json!(kept.first().copied().unwrap_or(0));
            format!("closed chart {pane}")
        }
        "focus" => {
            let id: u32 = m
                .get_one::<String>("ID")
                .expect("required")
                .parse()
                .map_err(|_| Fault::usage("a chart id is a number".into()))?;
            charts::resolve_pane(&book, Some(&id.to_string()))?;
            workspace.book_mut(at)["focused"] = json!(id);
            format!("focused chart {id}")
        }
        "set" => return chart_set(store, m, as_json, workspace, at, pane),
        "indicator" => return chart_indicator(store, m, as_json, workspace, at, pane),
        _ => return Err(Fault::usage(format!("no such command: chart {verb}"))),
    };

    workspace.save(store);
    said(as_json, json!({"message": text, "chart": pane}), text)
}

fn chart_set(
    store: &Store,
    m: &clap::ArgMatches,
    as_json: bool,
    workspace: &mut Workspace,
    at: usize,
    pane: u32,
) -> Result<String, Fault> {
    let suffix = arg(m, "suffix").map(|s| s.to_uppercase());
    let mut changed: Vec<String> = Vec::new();

    // Everything is checked before anything is written, so a command with one
    // bad value does not leave the chart half-changed.
    let symbol = match arg(m, "symbol") {
        None => None,
        Some(text) => {
            let symbol = text.to_uppercase();
            let index = crate::inventory::everything();
            if index.find(&symbol, suffix.as_deref()).is_none() {
                return Err(Fault::not_found(format!(
                    "no instrument called {}; try `omacharts symbol search {symbol}`",
                    spell(&symbol, suffix.as_deref())
                )));
            }
            Some(symbol)
        }
    };
    let timeframe = match arg(m, "resolution") {
        None => None,
        Some(text) => Some(Timeframe::parse(text).ok_or_else(|| {
            Fault::usage(format!("{text:?} is not a resolution; try 5m, 1h, 1D or 1W"))
        })?),
    };
    let style = match arg(m, "style") {
        None => None,
        Some(text) => Some(
            BarStyle::from_key(text)
                .ok_or_else(|| Fault::usage(format!("{text:?} is not a bar style")))?,
        ),
    };
    let session = match arg(m, "session") {
        None => None,
        Some(text) => Some(
            Session::from_key(text)
                .ok_or_else(|| Fault::usage(format!("{text:?} is not a session")))?,
        ),
    };
    let link = match arg(m, "link") {
        None => None,
        Some(text) if text.eq_ignore_ascii_case("none") => Some(0u8),
        Some(text) => {
            let group: u8 = text
                .parse()
                .ok()
                .filter(|g| (1..=9).contains(g))
                .ok_or_else(|| Fault::usage(format!("{text:?} is not a link group; use 1-9 or none")))?;
            Some(group)
        }
    };

    let target = workspace.book_mut(at);
    let Some(chart) = charts::pane_mut(target, pane) else {
        return Err(Fault::not_found(format!("no chart {pane}")));
    };
    if let Some(symbol) = symbol {
        chart["symbol"] = json!(symbol);
        chart["suffix"] = json!(suffix);
        changed.push(format!("symbol {}", spell(&symbol, suffix.as_deref())));
    }
    if let Some(timeframe) = timeframe {
        chart["timeframe"] = json!(timeframe.key());
        changed.push(format!("resolution {}", timeframe.key()));
    }
    if let Some(style) = style {
        chart["bar_style"] = json!(style.key());
        changed.push(format!("style {}", style.key()));
    }
    if let Some(session) = session {
        chart["session"] = json!(session.key());
        changed.push(format!("session {}", session.key()));
    }
    if let Some(link) = link {
        chart["linked"] = json!(link);
        changed.push(match link {
            0 => "unlinked".to_string(),
            group => format!("link group {group}"),
        });
    }
    if let Some(grid) = arg(m, "grid") {
        chart["show_grid"] = json!(grid == "on");
        changed.push(format!("grid {grid}"));
    }

    if changed.is_empty() {
        return Err(Fault::usage(
            "nothing to change; pass --symbol, --resolution, --style, --session, --link or --grid"
                .into(),
        ));
    }
    workspace.save(store);
    let text = format!("chart {pane}: {}", changed.join(", "));
    said(as_json, json!({"chart": pane, "changed": changed}), text)
}

fn chart_indicator(
    store: &Store,
    m: &clap::ArgMatches,
    as_json: bool,
    workspace: &mut Workspace,
    at: usize,
    pane: u32,
) -> Result<String, Fault> {
    let action = arg(m, "ACTION").expect("required").as_str();
    let book = workspace.book(at)?.clone();
    let chart = charts::panes(&book)
        .iter()
        .find(|p| p["id"].as_u64() == Some(pane as u64))
        .cloned()
        .unwrap_or_else(|| json!({}));

    if action == "list" {
        let listed = chart["indicators"].as_array().cloned().unwrap_or_default();
        let rows: Vec<String> = listed
            .iter()
            .map(|i| {
                json!({
                    "id": i["id"],
                    "kind": i["kind"],
                    "params": i["params"],
                    "color": i["color"],
                    "stroke": i["stroke"],
                    "visible": i["visible"],
                })
                .to_string()
            })
            .collect();
        return match as_json {
            true => Ok(format!("{{\"chart\":{pane},\"indicators\":[{}]}}\n", rows.join(","))),
            false => Ok(listed
                .iter()
                .map(describe_indicator)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n"),
        };
    }

    let wanted = arg(m, "KIND")
        .ok_or_else(|| Fault::usage(format!("`indicator {action}` needs an indicator to {action}")))?
        .to_lowercase();
    let kind = IndicatorKind::ALL
        .into_iter()
        .find(|k| k.key() == wanted)
        .ok_or_else(|| {
            Fault::usage(format!(
                "{wanted:?} is not an indicator; try {}",
                IndicatorKind::ALL.map(|k| k.key()).join(", ")
            ))
        })?;

    // Everything is read and checked before anything is written, so a command
    // carrying one bad value leaves the chart exactly as it was.
    let edits = Edits::read(m)?;

    let target = workspace.book_mut(at);
    let Some(chart) = charts::pane_mut(target, pane) else {
        return Err(Fault::not_found(format!("no chart {pane}")));
    };
    let indicators = chart["indicators"].as_array_mut().ok_or_else(|| {
        Fault::new(super::EXIT_ERROR, "this chart's indicators are not a list".into())
    })?;

    let text = match action {
        "add" => {
            let next = indicators
                .iter()
                .filter_map(|i| i["id"].as_u64())
                .max()
                .map(|id| id as u32 + 1)
                .unwrap_or(1);
            let mut added = serde_json::to_value(Indicator::new(next, kind))
                .map_err(|e| Fault::new(super::EXIT_ERROR, e.to_string()))?;
            edits.apply(&mut added, kind)?;
            let described = describe_indicator(&added);
            indicators.push(added);
            format!("added {described}")
        }
        "remove" => {
            let before = indicators.len();
            indicators.retain(|i| !is_kind(i, kind));
            if indicators.len() == before {
                return Err(Fault::not_found(format!(
                    "this chart has no {}",
                    kind.short_name()
                )));
            }
            format!("removed {}", kind.short_name())
        }
        "set" => {
            let wanted_id = match arg(m, "id") {
                None => None,
                Some(text) => Some(
                    text.parse::<u64>()
                        .map_err(|_| Fault::usage(format!("{text:?} is not an indicator id")))?,
                ),
            };
            let matching: Vec<usize> = indicators
                .iter()
                .enumerate()
                .filter(|(_, i)| is_kind(i, kind))
                .filter(|(_, i)| wanted_id.is_none_or(|id| i["id"].as_u64() == Some(id)))
                .map(|(at, _)| at)
                .collect();
            let which = match matching.as_slice() {
                [one] => *one,
                [] => {
                    return Err(Fault::not_found(format!(
                        "this chart has no {}",
                        kind.short_name()
                    )))
                }
                many => {
                    return Err(Fault::ambiguous(format!(
                        "this chart has {} of them; say which with --id {}",
                        many.len(),
                        many.iter()
                            .filter_map(|at| indicators[*at]["id"].as_u64())
                            .map(|id| id.to_string())
                            .collect::<Vec<_>>()
                            .join(" or ")
                    )))
                }
            };
            if edits.is_empty() {
                return Err(Fault::usage(
                    "nothing to change; pass --period, --anchor, --rows, --color and so on \
                     (see `omacharts chart indicator --help`)"
                        .into(),
                ));
            }
            edits.apply(&mut indicators[which], kind)?;
            format!("set {}", describe_indicator(&indicators[which]))
        }
        other => return Err(Fault::usage(format!("{other:?} is not list, add, remove or set"))),
    };

    workspace.save(store);

    // What it actually hit, not just that it worked. A command that named no
    // chart has to say which one it found, or nobody can catch it reaching
    // the wrong one.
    let where_at = format!(
        "{text} on {} in {:?}",
        chart_label(workspace.book(at)?, pane),
        workspace.name_of(at)
    );
    said(as_json, json!({"chart": pane, "message": where_at}), where_at)
}

/// Does this stored indicator have that kind?
///
/// `Params` is tagged by kind and the kind is also its own field, and a value
/// written by an older build may carry only one of them.
fn is_kind(stored: &Value, kind: IndicatorKind) -> bool {
    stored["kind"].as_str() == Some(kind.key())
        || stored["params"]["kind"].as_str() == Some(kind.key())
}

/// How a chart is named back to somebody who did not name it.
fn chart_label(book: &Value, pane: u32) -> String {
    let Some(found) = charts::panes(book).iter().find(|p| p["id"].as_u64() == Some(pane as u64))
    else {
        return format!("chart {pane}");
    };
    let at = charts::position_of(book, pane).map(|at| format!("pos:{at} ")).unwrap_or_default();
    format!(
        "{at}{} {}",
        spell(found["symbol"].as_str().unwrap_or("?"), found["suffix"].as_str()),
        found["timeframe"].as_str().unwrap_or("")
    )
}

fn describe_indicator(stored: &Value) -> String {
    let kind = stored["kind"].as_str().unwrap_or("?");
    let mut text = IndicatorKind::ALL
        .into_iter()
        .find(|k| k.key() == kind)
        .map(|k| k.short_name().to_string())
        .unwrap_or_else(|| kind.to_string());
    if let Some(period) = stored["params"]["period"].as_u64() {
        text.push_str(&format!("({period})"));
    }
    if let Some(reset) = stored["params"]["reset"].as_str() {
        text.push_str(&format!(" · {reset}"));
    }
    text
}

/// Every parameter a command offered, read and checked before one is written.
struct Edits {
    period: Option<u64>,
    anchor: Option<Reset>,
    rows: Option<Option<u64>>,
    value_area: Option<f64>,
    poc_color: Option<ColorChoice>,
    color: Option<ColorChoice>,
    width: Option<f64>,
    style: Option<LineStyle>,
    height: Option<f64>,
    overbought: Option<f64>,
    oversold: Option<f64>,
    bands: Option<Vec<usize>>,
    band_alpha: Option<f64>,
    visible: Option<bool>,
}

impl Edits {
    fn read(m: &clap::ArgMatches) -> Result<Edits, Fault> {
        Ok(Edits {
            period: number(m, "period")?.map(|n| n as u64),
            anchor: match arg(m, "anchor") {
                None => None,
                Some(text) => Some(Reset::from_key(text).ok_or_else(|| {
                    Fault::usage(format!("{text:?} is not an anchor; try session or week"))
                })?),
            },
            rows: match arg(m, "rows") {
                None => None,
                Some(text) if text.eq_ignore_ascii_case("auto") => Some(None),
                Some(text) => Some(Some(text.parse::<u64>().map_err(|_| {
                    Fault::usage(format!("{text:?} is not a row count; use a number or `auto`"))
                })?)),
            },
            value_area: fraction(m, "value-area", 0.0, 1.0)?,
            poc_color: colour(m, "poc-color")?,
            color: colour(m, "color")?,
            width: number(m, "width")?,
            style: match arg(m, "style") {
                None => None,
                Some(text) => Some(
                    LineStyle::ALL
                        .into_iter()
                        .find(|s| serde_json::to_value(s).ok().as_ref().and_then(Value::as_str) == Some(text.as_str()))
                        .ok_or_else(|| Fault::usage(format!("{text:?} is not a line style")))?,
                ),
            },
            height: fraction(m, "height", 0.0, 1.0)?,
            overbought: number(m, "overbought")?,
            oversold: number(m, "oversold")?,
            bands: match arg(m, "bands") {
                None => None,
                Some(text) if text.eq_ignore_ascii_case("none") => Some(Vec::new()),
                Some(text) => Some(
                    text.split(',')
                        .map(|part| {
                            part.trim().parse::<usize>().ok().filter(|n| (1..=3).contains(n)).map(|n| n - 1)
                        })
                        .collect::<Option<Vec<usize>>>()
                        .ok_or_else(|| {
                            Fault::usage(format!("{text:?} is not a band list; try 1,2 or none"))
                        })?,
                ),
            },
            band_alpha: fraction(m, "band-alpha", 0.02, 0.6)?,
            visible: arg(m, "visible").map(|v| v == "on"),
        })
    }

    fn is_empty(&self) -> bool {
        self.period.is_none()
            && self.anchor.is_none()
            && self.rows.is_none()
            && self.value_area.is_none()
            && self.poc_color.is_none()
            && self.color.is_none()
            && self.width.is_none()
            && self.style.is_none()
            && self.height.is_none()
            && self.overbought.is_none()
            && self.oversold.is_none()
            && self.bands.is_none()
            && self.band_alpha.is_none()
            && self.visible.is_none()
    }

    /// Write what was given, and refuse what this indicator has no use for.
    ///
    /// Silently ignoring `--period` on a volume profile would read as the
    /// command having worked, which is the one thing a caller that cannot see
    /// the screen must never be told.
    fn apply(&self, stored: &mut Value, kind: IndicatorKind) -> Result<(), Fault> {
        let params = &mut stored["params"];
        let refuse = |what: &str| {
            Fault::usage(format!("{} has no {what}", kind.short_name()))
        };

        if let Some(period) = self.period {
            match params.get("period").is_some() {
                true => params["period"] = json!(period.max(1)),
                false => return Err(refuse("period")),
            }
        }
        if let Some(anchor) = self.anchor {
            match params.get("reset").is_some() {
                true => params["reset"] = json!(anchor.key()),
                false => return Err(refuse("anchor")),
            }
        }
        if let Some(rows) = self.rows {
            match kind == IndicatorKind::VolumeProfile {
                true => params["rows"] = json!(rows),
                false => return Err(refuse("rows")),
            }
        }
        if let Some(share) = self.value_area {
            match kind == IndicatorKind::VolumeProfile {
                true => params["value_area"] = json!(share),
                false => return Err(refuse("value area")),
            }
        }
        if let Some(colour) = &self.poc_color {
            match kind == IndicatorKind::VolumeProfile {
                true => params["poc_color"] = to_value(colour)?,
                false => return Err(refuse("point of control")),
            }
        }
        if let Some(level) = self.overbought {
            match params.get("overbought").is_some() {
                true => params["overbought"] = json!(level),
                false => return Err(refuse("overbought level")),
            }
        }
        if let Some(level) = self.oversold {
            match params.get("oversold").is_some() {
                true => params["oversold"] = json!(level),
                false => return Err(refuse("oversold level")),
            }
        }
        if let Some(height) = self.height {
            match params.get("height").is_some() {
                true => params["height"] = json!(height),
                false => return Err(refuse("pane height")),
            }
        }
        if self.bands.is_some() || self.band_alpha.is_some() {
            if kind != IndicatorKind::Vwap {
                return Err(refuse("bands"));
            }
            let Some(bands) = params["bands"].as_array_mut() else {
                return Err(refuse("bands"));
            };
            for (at, band) in bands.iter_mut().enumerate() {
                if let Some(wanted) = &self.bands {
                    band["enabled"] = json!(wanted.contains(&at));
                }
                if let Some(alpha) = self.band_alpha {
                    band["fill_alpha"] = json!(alpha);
                }
            }
        }

        if let Some(colour) = &self.color {
            stored["color"] = to_value(colour)?;
        }
        if let Some(width) = self.width {
            stored["stroke"]["width"] = json!(width.max(0.0));
        }
        if let Some(style) = self.style {
            stored["stroke"]["style"] = to_value(&style)?;
        }
        if stored["stroke"].is_null() {
            stored["stroke"] = to_value(&Stroke::default())?;
        }
        if let Some(visible) = self.visible {
            stored["visible"] = json!(visible);
        }
        Ok(())
    }
}

fn to_value<T: serde::Serialize>(value: &T) -> Result<Value, Fault> {
    serde_json::to_value(value).map_err(|e| Fault::new(super::EXIT_ERROR, e.to_string()))
}

fn number(m: &clap::ArgMatches, id: &str) -> Result<Option<f64>, Fault> {
    match arg(m, id) {
        None => Ok(None),
        Some(text) => text
            .parse::<f64>()
            .map(Some)
            .map_err(|_| Fault::usage(format!("--{id} takes a number, not {text:?}"))),
    }
}

fn fraction(m: &clap::ArgMatches, id: &str, low: f64, high: f64) -> Result<Option<f64>, Fault> {
    match number(m, id)? {
        None => Ok(None),
        Some(value) if (low..=high).contains(&value) => Ok(Some(value)),
        Some(value) => Err(Fault::usage(format!(
            "--{id} is {value}, which is outside {low} to {high}"
        ))),
    }
}

/// A palette name, or a hex the theme will never touch.
///
/// Both, because the app keeps them apart: a swatch is re-resolved whenever
/// the theme changes and a hex is not, so taking only hex would opt every
/// scripted indicator out of following the desktop.
fn colour(m: &clap::ArgMatches, id: &str) -> Result<Option<ColorChoice>, Fault> {
    let Some(text) = arg(m, id) else { return Ok(None) };
    if let Some(hex) = text.strip_prefix('#') {
        let valid = matches!(hex.len(), 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit());
        return match valid {
            true => Ok(Some(ColorChoice::Fixed { hex: format!("#{}", hex.to_lowercase()) })),
            false => Err(Fault::usage(format!("{text:?} is not a colour; try #rrggbb"))),
        };
    }
    SWATCH_NAMES
        .iter()
        .find(|name| name.eq_ignore_ascii_case(text))
        .map(|name| Some(ColorChoice::swatch(name)))
        .ok_or_else(|| {
            Fault::usage(format!(
                "{text:?} is not a palette colour; try {} — or #rrggbb for one the theme will not change",
                SWATCH_NAMES.join(", ")
            ))
        })
}

fn pane_json(pane: &Value) -> String {
    json!({
        "id": pane["id"],
        "symbol": pane["symbol"],
        "suffix": pane["suffix"],
        "display": spell(pane["symbol"].as_str().unwrap_or(""), pane["suffix"].as_str()),
        "resolution": pane["timeframe"],
        "style": pane["bar_style"],
        "session": pane["session"],
        "grid": pane["show_grid"],
        "link": pane["linked"],
        "indicators": pane["indicators"]
            .as_array()
            .map(|all| all.iter().filter_map(|i| i["kind"].as_str()).collect::<Vec<_>>())
            .unwrap_or_default(),
    })
    .to_string()
}

// -- what is open ---------------------------------------------------------

/// The live picture, in one call.
///
/// One command rather than `chartbook current` and `chart current`, so the
/// answer cannot disagree with itself because something moved between two
/// reads. Everything it names is spelled the way the other verbs accept it,
/// so a snapshot composes into the next command without translation.
fn status(store: &Store, window_open: bool, as_json: bool) -> Result<String, Fault> {
    let workspace = Workspace::load(store);
    let at = workspace.active().min(workspace.books().len().saturating_sub(1));
    let book = workspace.books().get(at).cloned().unwrap_or_else(|| json!({}));
    let focused = book["focused"].as_u64().unwrap_or(0) as u32;
    let order = charts::leaves(&book["layout"]);
    let position = order.iter().position(|id| *id == focused);

    let charts_json: Vec<String> = order
        .iter()
        .enumerate()
        .filter_map(|(i, id)| {
            let pane = charts::panes(&book).iter().find(|p| p["id"].as_u64() == Some(*id as u64))?;
            Some(format!(
                "{{\"position\":{i},\"target\":\"pos:{i}\",\"focused\":{},\"chart\":{}}}",
                Some(*id) == Some(focused),
                pane_json(pane)
            ))
        })
        .collect();

    if as_json {
        return Ok(format!(
            "{{\"windowOpen\":{window_open},\"chartbook\":{},\"target\":{},\"watchlist\":{},\
             \"focusedPosition\":{},\"charts\":[{}]}}\n",
            super::json_string(&workspace.name_of(at)),
            super::json_string(&format!("id:{at}")),
            book["watchlist"],
            match position {
                Some(at) => at.to_string(),
                None => "null".to_string(),
            },
            charts_json.join(",")
        ));
    }

    let mut out = match window_open {
        true => String::new(),
        false => "nothing is open; this is what was stored when the window last closed\n\n"
            .to_string(),
    };
    out.push_str(&format!("chartbook  {} (id:{at})\n", workspace.name_of(at)));
    for (i, id) in order.iter().enumerate() {
        let Some(pane) = charts::panes(&book).iter().find(|p| p["id"].as_u64() == Some(*id as u64))
        else {
            continue;
        };
        out.push_str(&format!(
            "{} pos:{i}  {:<10} {:<5} {}\n",
            if *id == focused { "*" } else { " " },
            spell(pane["symbol"].as_str().unwrap_or("?"), pane["suffix"].as_str()),
            pane["timeframe"].as_str().unwrap_or(""),
            match pane["linked"].as_u64().unwrap_or(0) {
                0 => "unlinked".to_string(),
                group => format!("link {group}"),
            },
        ));
    }
    Ok(out)
}

/// Whether a pointer on one chart draws a line on the ones linked to it.
///
/// One setting for the whole window rather than one per chart, which is why
/// it reads and writes a setting rather than a pane.
fn crosshair(store: &Store, m: &clap::ArgMatches, as_json: bool) -> Result<String, Fault> {
    const KEY: &str = "sync_crosshair";
    let Some(state) = arg(m, "STATE") else {
        let on = store.setting_bool(KEY, true);
        return match as_json {
            true => Ok(format!("{}\n", json!({"crosshairSync": on}))),
            false => Ok(format!("{}\n", if on { "on" } else { "off" })),
        };
    };
    let on = state == "on";
    store.set_setting_bool(KEY, on);
    said(
        as_json,
        json!({"crosshairSync": on}),
        format!("crosshair syncing across linked charts is {state}"),
    )
}

// -- settings and cache ---------------------------------------------------

fn config_list(store: &Store, as_json: bool) -> Result<String, Fault> {
    let settings = store.settings();
    if as_json {
        let rows = settings
            .iter()
            .map(|(k, v)| json!({"key": k, "value": v}).to_string())
            .collect();
        return Ok(wrap_list("settings", rows));
    }
    Ok(settings
        .iter()
        .map(|(k, v)| format!("{k:<24} {}", truncate(v, 60)))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n")
}

fn config_get(store: &Store, m: &clap::ArgMatches, as_json: bool) -> Result<String, Fault> {
    let key = arg(m, "KEY").expect("required");
    let value = store
        .setting(key)
        .ok_or_else(|| Fault::not_found(format!("{key:?} has never been set")))?;
    match as_json {
        true => Ok(format!("{}\n", json!({"key": key, "value": value}))),
        false => Ok(format!("{value}\n")),
    }
}

fn config_set(store: &Store, m: &clap::ArgMatches, as_json: bool) -> Result<String, Fault> {
    let key = arg(m, "KEY").expect("required");
    let value = arg(m, "VALUE").expect("required");
    store.set_setting(key, value);
    said(as_json, json!({"key": key, "value": value}), format!("set {key} to {value}"))
}

fn cache_status(store: &Store, as_json: bool) -> Result<String, Fault> {
    let (used, limit, series) = (store.used_bytes(), store.cache_limit(), store.cached_series());
    let share = if limit > 0 { used as f64 / limit as f64 * 100.0 } else { 0.0 };
    match as_json {
        true => Ok(format!(
            "{}\n",
            json!({"series": series, "usedBytes": used, "limitBytes": limit, "percent": share.round()})
        )),
        false => Ok(format!(
            "{series} series · {} of {} · {}%\n",
            bytes(used),
            bytes(limit),
            share.round()
        )),
    }
}

fn cache_clear(store: &Store, as_json: bool) -> Result<String, Fault> {
    let series = store.cached_series();
    store
        .clear_market_data()
        .map_err(|e| Fault::new(super::EXIT_ERROR, e.to_string()))?;
    said(as_json, json!({"cleared": series}), format!("cleared {series} cached series"))
}

fn cache_limit(store: &Store, m: &clap::ArgMatches, as_json: bool) -> Result<String, Fault> {
    let Some(text) = arg(m, "SIZE") else {
        let limit = store.cache_limit();
        return match as_json {
            true => Ok(format!("{}\n", json!({"limitBytes": limit}))),
            false => Ok(format!("{}\n", bytes(limit))),
        };
    };
    let parsed = parse_bytes(text)
        .ok_or_else(|| Fault::usage(format!("{text:?} is not a size; try 512MB or 2GB")))?;
    store.set_cache_limit(parsed);
    said(
        as_json,
        json!({"limitBytes": parsed}),
        format!("the cache will be pruned back under {}", bytes(parsed)),
    )
}

// -- shared -------------------------------------------------------------

/// Say what happened, in whichever form was asked for.
///
/// A mutation that printed nothing would leave a caller that cannot look at
/// the screen with no way to tell a success from a no-op but to go and ask.
fn said(as_json: bool, value: Value, text: String) -> Result<String, Fault> {
    match as_json {
        true => Ok(format!("{value}\n")),
        false => Ok(format!("{text}\n")),
    }
}

fn wrap_list(name: &str, rows: Vec<String>) -> String {
    format!("{{\"{name}\":[{}]}}\n", rows.join(","))
}

/// Read an argument that this verb may not have.
///
/// Clap panics when asked for an id the subcommand never defined, and the
/// arms below are shared between verbs that take a chartbook and verbs that
/// do not. Asking politely is the difference between a missing flag and a
/// crash.
fn arg<'a>(m: &'a clap::ArgMatches, id: &str) -> Option<&'a String> {
    m.try_get_one::<String>(id).ok().flatten()
}

fn flag(m: &clap::ArgMatches, id: &str) -> bool {
    m.try_get_one::<bool>(id).ok().flatten().copied().unwrap_or(false)
}

/// How an instrument is written where a person reads it.
pub fn spell(symbol: &str, suffix: Option<&str>) -> String {
    match suffix.filter(|s| !s.is_empty()) {
        Some(suffix) => format!("{symbol}.{suffix}"),
        None => symbol.to_string(),
    }
}

fn truncate(text: &str, width: usize) -> String {
    match text.chars().count() > width {
        false => text.to_string(),
        true => text.chars().take(width - 1).collect::<String>() + "…",
    }
}

fn bytes(count: i64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let n = count as f64;
    if n >= GB {
        format!("{:.1} GB", n / GB)
    } else {
        format!("{:.0} MB", n / MB)
    }
}

fn parse_bytes(text: &str) -> Option<i64> {
    let cleaned = text.trim().to_uppercase().replace(' ', "");
    let (number, scale) = if let Some(rest) = cleaned.strip_suffix("GB") {
        (rest, 1024.0 * 1024.0 * 1024.0)
    } else if let Some(rest) = cleaned.strip_suffix("MB") {
        (rest, 1024.0 * 1024.0)
    } else if let Some(rest) = cleaned.strip_suffix('B') {
        (rest, 1.0)
    } else {
        (cleaned.as_str(), 1.0)
    };
    let value: f64 = number.parse().ok()?;
    (value > 0.0).then_some((value * scale) as i64)
}

fn completions(shell: &str) -> String {
    super::completions::script(shell)
}

fn man_page() -> String {
    super::completions::man()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(line: &str, store: &Store) -> Outcome {
        let args: Vec<String> =
            std::iter::once("omacharts".to_string()).chain(line.split_whitespace().map(String::from)).collect();
        dispatch(&args, store, None)
    }

    #[test]
    fn a_watchlist_can_be_made_filled_and_read_back() {
        let store = Store::memory().unwrap();
        assert_eq!(run("watchlist create Semis", &store).code, 0);
        let added = run("watchlist add Semis NVDA AMD AVGO", &store);
        assert_eq!(added.code, 0, "{}", added.err);
        assert!(added.out.contains("added 3"), "{}", added.out);

        let shown = run("watchlist show Semis --json", &store);
        let parsed: serde_json::Value = serde_json::from_str(&shown.out).unwrap();
        let symbols = parsed["sections"][0]["symbols"].as_array().unwrap();
        assert_eq!(symbols.len(), 3);
    }

    /// An agent reads the code, not the sentence. A name that does not exist
    /// has to be distinguishable from a command that was spelled wrongly.
    #[test]
    fn naming_something_that_does_not_exist_says_so_in_the_exit_code() {
        let store = Store::memory().unwrap();
        assert_eq!(run("watchlist show Nope", &store).code, super::super::EXIT_NOT_FOUND);
        assert_eq!(run("watchlist frobnicate", &store).code, super::super::EXIT_USAGE);
    }

    #[test]
    fn the_watchlist_the_bar_widget_shows_cannot_be_deleted() {
        let store = Store::memory().unwrap();
        let refused = run("watchlist delete id:1", &store);
        assert_eq!(refused.code, super::super::EXIT_REFUSED);
        assert!(refused.err.contains("rename"), "{}", refused.err);
    }

    #[test]
    fn adding_the_same_symbol_twice_is_not_an_error() {
        let store = Store::memory().unwrap();
        run("watchlist create Semis", &store);
        assert_eq!(run("watchlist add Semis NVDA", &store).code, 0);
        assert_eq!(run("watchlist add Semis NVDA", &store).code, 0);
        let shown = run("watchlist show Semis --json", &store);
        let parsed: serde_json::Value = serde_json::from_str(&shown.out).unwrap();
        assert_eq!(parsed["sections"][0]["symbols"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_ticker_nobody_lists_is_refused_rather_than_stored() {
        let store = Store::memory().unwrap();
        run("watchlist create Semis", &store);
        let out = run("watchlist add Semis NOTATICKER", &store);
        assert_eq!(out.code, super::super::EXIT_NOT_FOUND);
    }

    #[test]
    fn a_section_can_become_a_watchlist_carrying_its_symbols() {
        let store = Store::memory().unwrap();
        run("section create id:1 Energy", &store);
        run("watchlist add id:1 CL --section Energy", &store);
        let out = run("section promote id:1 Energy", &store);
        assert_eq!(out.code, 0, "{}", out.err);

        let listed = run("watchlist list --json", &store);
        assert!(listed.out.contains("Energy"), "{}", listed.out);
        assert!(!run("section list id:1 --json", &store).out.contains("Energy"));
    }

    #[test]
    fn a_chartbook_can_be_created_split_and_read_back() {
        let store = Store::memory().unwrap();
        assert_eq!(run("chartbook create Macro --symbol SPY --switch", &store).code, 0);
        let split = run("chart split horizontal --book Macro", &store);
        assert_eq!(split.code, 0, "{}", split.err);

        let listed = run("chart list --book Macro --json", &store);
        let parsed: serde_json::Value = serde_json::from_str(&listed.out).unwrap();
        assert_eq!(parsed["charts"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn the_last_chart_in_a_chartbook_cannot_be_closed() {
        let store = Store::memory().unwrap();
        run("chartbook create Macro --switch", &store);
        assert_eq!(run("chart close --book Macro", &store).code, super::super::EXIT_REFUSED);
    }

    #[test]
    fn a_charts_symbol_and_resolution_can_be_set_together() {
        let store = Store::memory().unwrap();
        run("chartbook create Macro --switch", &store);
        let set = run("chart set --book Macro --symbol NVDA --resolution 1h --style ohlc", &store);
        assert_eq!(set.code, 0, "{}", set.err);

        let listed = run("chart list --book Macro --json", &store);
        let parsed: serde_json::Value = serde_json::from_str(&listed.out).unwrap();
        assert_eq!(parsed["charts"][0]["symbol"], "NVDA");
        assert_eq!(parsed["charts"][0]["resolution"], "1h");
    }

    /// A command that half-worked is worse than one that refused: the caller
    /// has no way to tell which half.
    #[test]
    fn one_bad_value_leaves_the_chart_exactly_as_it_was() {
        let store = Store::memory().unwrap();
        run("chartbook create Macro --symbol SPY --switch", &store);
        let out = run("chart set --book Macro --symbol NVDA --resolution nonsense", &store);
        assert_eq!(out.code, super::super::EXIT_USAGE);

        let listed = run("chart list --book Macro --json", &store);
        let parsed: serde_json::Value = serde_json::from_str(&listed.out).unwrap();
        assert_eq!(parsed["charts"][0]["symbol"], "SPY", "the symbol must not have moved");
    }

    /// The widget on somebody's bar is running the old spelling right now,
    /// and it updates on a timer. Growing subcommands under `watchlist` must
    /// not answer it with a usage error.
    #[test]
    fn the_bar_widgets_own_command_still_means_what_it_did() {
        let store = Store::memory().unwrap();
        // Without `--refresh`, which would go to the network: this is about
        // the shape of the answer, not about fetching one.
        let out = run("watchlist", &store);
        assert_eq!(out.code, 0, "{}", out.err);
        let parsed: serde_json::Value = serde_json::from_str(&out.out).unwrap();
        assert!(parsed["sections"].is_array(), "the bare form must answer with the feed");
    }

    /// The whole point of the implicit default is "the chart in front of me".
    /// With nothing in front of anybody, answering from what was stored when
    /// the window last closed would be indistinguishable from answering
    /// correctly — and an agent would act on it.
    #[test]
    fn asking_for_the_focused_chart_with_nothing_open_says_so() {
        let store = Store::memory().unwrap();
        run("chartbook create Macro --switch", &store);
        let out = run("chart indicator add rsi", &store);
        assert_eq!(out.code, super::super::EXIT_NO_WINDOW);
        assert!(out.err.contains("name one, or start Omacharts"), "{}", out.err);
    }

    #[test]
    fn an_indicator_carries_the_parameters_it_was_given() {
        let store = Store::memory().unwrap();
        run("chartbook create Macro --switch", &store);
        let out = run(
            "chart indicator add sma --book Macro --period 200 --color Amber --style dashed --width 2",
            &store,
        );
        assert_eq!(out.code, 0, "{}", out.err);

        let listed = run("chart indicator list --book Macro --json", &store);
        let parsed: serde_json::Value = serde_json::from_str(&listed.out).unwrap();
        let sma = &parsed["indicators"][0];
        assert_eq!(sma["params"]["period"], 200);
        assert_eq!(sma["color"]["name"], "Amber", "a swatch, so it follows the theme");
        assert_eq!(sma["stroke"]["style"], "dashed");
        assert_eq!(sma["stroke"]["width"], 2.0);
    }

    /// A hex is the colour somebody picked and is never re-resolved; a name is
    /// the theme's and is. Both have to arrive as what they are.
    #[test]
    fn a_colour_is_either_the_themes_or_the_users_and_the_two_stay_apart() {
        let store = Store::memory().unwrap();
        run("chartbook create Macro --switch", &store);
        run("chart indicator add ema --book Macro --color #ff8800", &store);

        let listed = run("chart indicator list --book Macro --json", &store);
        let parsed: serde_json::Value = serde_json::from_str(&listed.out).unwrap();
        assert_eq!(parsed["indicators"][0]["color"]["hex"], "#ff8800");

        let bad = run("chart indicator add rsi --book Macro --color Chartreuse", &store);
        assert_eq!(bad.code, super::super::EXIT_USAGE);
        assert!(bad.err.contains("Blue"), "the error has to name the palette: {}", bad.err);
    }

    #[test]
    fn the_volume_profile_takes_auto_rows_and_its_own_two_colours() {
        let store = Store::memory().unwrap();
        run("chartbook create Macro --switch", &store);
        let out = run(
            "chart indicator add volume_profile --book Macro --rows auto --anchor week \
             --color Violet --poc-color Amber",
            &store,
        );
        assert_eq!(out.code, 0, "{}", out.err);

        let listed = run("chart indicator list --book Macro --json", &store);
        let parsed: serde_json::Value = serde_json::from_str(&listed.out).unwrap();
        let vp = &parsed["indicators"][0];
        assert!(vp["params"]["rows"].is_null(), "auto is stored as no number at all");
        assert_eq!(vp["params"]["reset"], "week");
        assert_eq!(vp["params"]["poc_color"]["name"], "Amber");
    }

    /// Quietly ignoring a parameter the indicator has no use for would read
    /// as the command having worked.
    #[test]
    fn a_parameter_an_indicator_has_no_use_for_is_refused() {
        let store = Store::memory().unwrap();
        run("chartbook create Macro --switch", &store);
        let out = run("chart indicator add volume --book Macro --period 50", &store);
        assert_eq!(out.code, super::super::EXIT_USAGE);
        assert!(out.err.contains("no period"), "{}", out.err);
    }

    #[test]
    fn an_indicator_already_on_a_chart_can_be_reconfigured() {
        let store = Store::memory().unwrap();
        run("chartbook create Macro --switch", &store);
        run("chart indicator add rsi --book Macro", &store);
        let out = run("chart indicator set rsi --book Macro --period 21 --overbought 80", &store);
        assert_eq!(out.code, 0, "{}", out.err);

        let listed = run("chart indicator list --book Macro --json", &store);
        let parsed: serde_json::Value = serde_json::from_str(&listed.out).unwrap();
        assert_eq!(parsed["indicators"][0]["params"]["period"], 21);
        assert_eq!(parsed["indicators"][0]["params"]["overbought"], 80.0);
    }

    #[test]
    fn vwap_bands_can_be_turned_on_and_shaded() {
        let store = Store::memory().unwrap();
        run("chartbook create Macro --switch", &store);
        run("chart indicator add vwap --book Macro --anchor month --bands 1,2 --band-alpha 0.3", &store);

        let listed = run("chart indicator list --book Macro --json", &store);
        let parsed: serde_json::Value = serde_json::from_str(&listed.out).unwrap();
        let bands = parsed["indicators"][0]["params"]["bands"].as_array().unwrap();
        assert_eq!(bands[0]["enabled"], true);
        assert_eq!(bands[1]["enabled"], true);
        assert_eq!(bands[2]["enabled"], false);
        assert_eq!(bands[0]["fill_alpha"], 0.3);
    }

    /// A command that resolved its own target has to say which one it found,
    /// or nobody can catch it reaching the wrong chart.
    #[test]
    fn a_command_names_the_chart_it_acted_on() {
        let store = Store::memory().unwrap();
        run("chartbook create Macro --symbol AAPL --switch", &store);
        let out = run("chart indicator add rsi --book Macro", &store);
        assert!(out.out.contains("AAPL"), "{}", out.out);
        assert!(out.out.contains("Macro"), "{}", out.out);
    }

    #[test]
    fn status_says_whether_anything_is_actually_open() {
        let store = Store::memory().unwrap();
        run("chartbook create Macro --symbol AAPL --switch", &store);
        let out = run("status show --json", &store);
        let parsed: serde_json::Value = serde_json::from_str(&out.out).unwrap();
        assert_eq!(parsed["windowOpen"], false, "nothing is running in a test");
        assert_eq!(parsed["chartbook"], "Macro");
        assert_eq!(parsed["charts"][0]["target"], "pos:0", "spelled as the other verbs take it");
    }

    #[test]
    fn crosshair_syncing_can_be_read_and_set() {
        let store = Store::memory().unwrap();
        assert_eq!(run("chart crosshair off", &store).code, 0);
        assert_eq!(run("chart crosshair", &store).out.trim(), "off");
        assert_eq!(run("chart crosshair on", &store).code, 0);
        assert_eq!(run("chart crosshair", &store).out.trim(), "on");
    }

    #[test]
    fn sizes_are_read_the_way_people_write_them() {
        assert_eq!(parse_bytes("1GB"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_bytes("512 MB"), Some(512 * 1024 * 1024));
        assert_eq!(parse_bytes("nonsense"), None);
        assert_eq!(parse_bytes("0"), None, "a cache of nothing is not a limit");
    }
}
