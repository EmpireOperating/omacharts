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
    // context costs around 350ms before anything of ours runs — measured
    // here at 561ms to a window with it, 226ms with the GL renderer, against
    // 8ms of our own work. The chart is drawn with cairo into a DrawingArea
    // either way, so the renderer only composites the result; paying a third
    // of a second for that is a bad trade on an app whose whole point is
    // feeling instant.
    //
    // GSK_RENDERER=cairo is faster still (124ms) at the cost of software
    // compositing. Anything already set is left alone.
    if std::env::var_os("GSK_RENDERER").is_none() {
        // SAFETY: single-threaded, before GTK or any other thread starts.
        unsafe { std::env::set_var("GSK_RENDERER", "ngl") };
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
            // A database we cannot open is fatal, but an in-memory one at
            // least puts a usable window on screen rather than nothing at all.
            let store =
                Rc::new(Store::open().or_else(|_| Store::memory()).expect("open database"));
            *window.borrow_mut() = Some(Window::build(app, store));
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
        glib::ExitCode::SUCCESS
    });

    app.run()
}
