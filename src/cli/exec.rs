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

use omacharts_engine::{BarStyle, Indicator, IndicatorKind, Session, Timeframe};

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

        ("chartbook", _) | ("chart", _) => charts_verb(store, noun, verb, m, json),

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
        "indicator" => return chart_indicator(m, as_json, workspace, at, pane),
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
    let listed = chart["indicators"].as_array().cloned().unwrap_or_default();

    if action == "list" {
        let names: Vec<String> = listed
            .iter()
            .filter_map(|i| i["kind"].as_str().map(str::to_string))
            .collect();
        return match as_json {
            true => Ok(format!("{}\n", json!({"chart": pane, "indicators": names}))),
            false => Ok(names.join("\n") + "\n"),
        };
    }

    let wanted = m
        .get_one::<String>("KIND")
        .ok_or_else(|| Fault::usage(format!("`indicator {action}` needs an indicator to {action}")))?
        .to_lowercase();
    let kind = IndicatorKind::ALL
        .into_iter()
        .find(|k| k.key() == wanted || k.short_name().to_lowercase() == wanted)
        .ok_or_else(|| {
            Fault::usage(format!(
                "{wanted:?} is not an indicator; try {}",
                IndicatorKind::ALL.map(|k| k.key()).join(", ")
            ))
        })?;

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
            let added = serde_json::to_value(Indicator::new(next, kind))
                .map_err(|e| Fault::new(super::EXIT_ERROR, e.to_string()))?;
            indicators.push(added);
            format!("added {} to chart {pane}", kind.short_name())
        }
        "remove" => {
            let before = indicators.len();
            indicators.retain(|i| i["kind"].as_str() != Some(kind.key()));
            if indicators.len() == before {
                return Err(Fault::not_found(format!(
                    "chart {pane} has no {}",
                    kind.short_name()
                )));
            }
            format!("removed {} from chart {pane}", kind.short_name())
        }
        other => return Err(Fault::usage(format!("{other:?} is not list, add or remove"))),
    };
    said(as_json, json!({"chart": pane, "message": text}), text)
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
        let split = run("chart split horizontal", &store);
        assert_eq!(split.code, 0, "{}", split.err);

        let listed = run("chart list --json", &store);
        let parsed: serde_json::Value = serde_json::from_str(&listed.out).unwrap();
        assert_eq!(parsed["charts"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn the_last_chart_in_a_chartbook_cannot_be_closed() {
        let store = Store::memory().unwrap();
        run("chartbook create Macro --switch", &store);
        assert_eq!(run("chart close", &store).code, super::super::EXIT_REFUSED);
    }

    #[test]
    fn a_charts_symbol_and_resolution_can_be_set_together() {
        let store = Store::memory().unwrap();
        run("chartbook create Macro --switch", &store);
        let set = run("chart set --symbol NVDA --resolution 1h --style ohlc", &store);
        assert_eq!(set.code, 0, "{}", set.err);

        let listed = run("chart list --json", &store);
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
        let out = run("chart set --symbol NVDA --resolution nonsense", &store);
        assert_eq!(out.code, super::super::EXIT_USAGE);

        let listed = run("chart list --json", &store);
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

    #[test]
    fn sizes_are_read_the_way_people_write_them() {
        assert_eq!(parse_bytes("1GB"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_bytes("512 MB"), Some(512 * 1024 * 1024));
        assert_eq!(parse_bytes("nonsense"), None);
        assert_eq!(parse_bytes("0"), None, "a cache of nothing is not a limit");
    }
}
