//! omacharts.

use std::rc::Rc;

use adw::prelude::*;
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
        Some(other) => {
            eprintln!("omacharts: unknown command {other:?}\n\n{}", omacharts::cli::USAGE);
            return glib::ExitCode::FAILURE;
        }
    }

    let app = adw::Application::builder().application_id(APP_ID).build();

    app.connect_activate(|app| {
        // A database we cannot open is fatal, but an in-memory one at least
        // puts a usable window on screen rather than nothing at all.
        let store = Rc::new(Store::open().or_else(|_| Store::memory()).expect("open database"));
        let window = Window::build(app, store);
        window.window.present();
    });

    app.run()
}
