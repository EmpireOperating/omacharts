//! omacharts.

use std::rc::Rc;

use std::cell::RefCell;

use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use omacharts::store::Store;
use omacharts::ui::Window;

const APP_ID: &str = "com.jorgemanrubia.Omacharts";

fn main() -> glib::ExitCode {
    // A handful of subcommands, matched by hand. Reaching for an argument
    // parser to tell "watchlist" from nothing would be more dependency than
    // decision.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {}
        Some("watchlist") => {
            let refresh = args.iter().any(|a| a == "--refresh");
            println!("{}", omacharts::cli::watchlist_json(refresh));
            return glib::ExitCode::SUCCESS;
        }
        Some("--help" | "-h" | "help") => {
            println!("{}", omacharts::cli::USAGE);
            return glib::ExitCode::SUCCESS;
        }
        // An unknown option is a mistake worth reporting. Anything else is a
        // symbol, and goes to the app — this matcher exists to peel off the
        // headless commands, not to vet arguments GTK will handle.
        Some(other) if other.starts_with('-') => {
            eprintln!("omacharts: unknown option {other:?}\n\n{}", omacharts::cli::USAGE);
            return glib::ExitCode::FAILURE;
        }
        Some(_) => {}
    }

    // GTK4 picks the Vulkan renderer by default, and bringing up a Vulkan
    // context costs most of half a second before anything of ours runs —
    // measured here, window on screen: vulkan 575ms, gl 191ms, cairo 148ms,
    // against 8ms of our own work. The chart is drawn with cairo into a
    // DrawingArea either way, so the renderer only composites the result;
    // paying a third of a second for that is a bad trade on an app whose
    // whole point is feeling instant.
    //
    // "gl", not "ngl": the renderer was renamed and the old name now draws a
    // warning on every launch. It is still honoured — the timings above are
    // identical either way — but a warning nobody can act on is noise.
    //
    // Software compositing (cairo) is faster still to the first frame and
    // pays for it while panning, which is the one thing this app does
    // constantly. Anything already set is left alone.
    if std::env::var_os("GSK_RENDERER").is_none() {
        // SAFETY: single-threaded, before GTK or any other thread starts.
        unsafe { std::env::set_var("GSK_RENDERER", "gl") };
    }

    // The command line is handled rather than ignored, because a second
    // launch is how anything outside the app asks for a symbol: the bar widget
    // runs `omacharts NVDA`, and GTK hands that to the instance already
    // running instead of starting another.
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    let window: RefCell<Option<Rc<Window>>> = RefCell::new(None);
    app.connect_command_line(move |app, command_line| {
        if window.borrow().is_none() {
            let store = match Store::open() {
                Ok(store) => store,
                // Written by a newer Omacharts than this one. The file is
                // intact and it is theirs, so this is the one failure that
                // must not be worked around: carrying on with an empty
                // database would put a pristine watchlist on screen over the
                // top of years of their own, and look exactly like having
                // lost it.
                Err(error @ omacharts::migrations::Error::FromTheFuture { .. }) => {
                    eprintln!("omacharts: {error}");
                    return glib::ExitCode::FAILURE;
                }
                // Anything else — unreadable, unwritable, corrupt — and an
                // in-memory database at least puts a usable window on screen
                // rather than nothing at all.
                Err(error) => {
                    eprintln!("omacharts: {error}; carrying on without saving anything");
                    Store::memory().expect("open database")
                }
            };
            // Trimming the cache is housekeeping, and housekeeping goes
            // behind the first frame with the rest of it: a cache that has
            // been over its limit for six hours can be over it a second
            // longer. Read before the store moves into the window.
            let sweep = store
                .path()
                .filter(|_| store.cache_sweep_due())
                .map(|path| (path.to_path_buf(), store.cache_limit()));

            *window.borrow_mut() = Some(Window::build(app, Rc::new(store)));

            if let Some((path, limit)) = sweep {
                glib::idle_add_local_once(move || {
                    omacharts::cache::sweep_in_background(path, limit, |swept| {
                        if swept.dropped > 0 {
                            eprintln!(
                                "omacharts: cache over its limit, dropped {} series",
                                swept.dropped
                            );
                        }
                    });
                });
            }
        }

        let Some(window) = window.borrow().clone() else {
            return glib::ExitCode::FAILURE;
        };

        let args: Vec<String> = command_line
            .arguments()
            .into_iter()
            .skip(1)
            .map(|arg| arg.to_string_lossy().into_owned())
            .filter(|arg| !arg.starts_with('-'))
            .collect();
        if let Some(symbol) = args.first() {
            window.show_named(&symbol.to_uppercase(), args.get(1).map(String::as_str));
        }

        window.window.present();

        // After the window is on screen, never before it: keeping an installed
        // bar widget current is housekeeping, and housekeeping does not get to
        // sit in front of the first frame.
        glib::idle_add_local_once(|| {
            if let Some(home) = glib::home_dir().to_str().map(std::path::PathBuf::from) {
                if let Err(error) = omacharts::bar_plugin::refresh(&home) {
                    eprintln!("omacharts: bar widget not updated: {error}");
                }
            }
        });

        glib::ExitCode::SUCCESS
    });

    app.run()
}
