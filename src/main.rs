//! omacharts.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use omacharts::store::Store;
use omacharts::ui::Window;

const APP_ID: &str = "com.jorgemanrubia.Omacharts";

fn main() -> glib::ExitCode {
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
