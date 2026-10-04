//! The widgets.

pub mod chart;
pub mod chart_settings;
pub mod colors;
pub mod dialogs;
pub mod pane;
pub mod palette;
pub mod preferences;
pub mod screenshot;
pub mod search;
pub mod shortcuts;
pub mod watchlist;
pub mod window;

pub use chart::ChartView;
pub use window::Window;

/// Whether widgets can be built on this thread.
///
/// Two bargains in one. There may be no display, in which case there is
/// nothing to check — that is the usual answer on CI. And libtest gives every
/// test its own thread even at `--test-threads=1`, so the first test to reach
/// GTK claims it and `gtk::init` from the next one panics with "attempted to
/// initialize GTK from two different threads". Asking first turns that into a
/// skip, which is what the display-less case already does.
#[cfg(test)]
pub fn gtk_ready() -> bool {
    gtk::is_initialized_main_thread() || (!gtk::is_initialized() && gtk::init().is_ok())
}
