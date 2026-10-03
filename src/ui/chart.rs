//! The candlestick chart.
//!
//! One `DrawingArea` and a cairo draw function. The view is two numbers — the
//! index of the leftmost visible bar and how many are visible — so panning and
//! zooming are arithmetic, not a re-layout, and a repaint touches only what is
//! on screen.
//!
//! Candles are drawn in two passes, up and down, each accumulating one cairo
//! path for the wicks and one for the bodies. Two strokes and two fills for a
//! screen of bars rather than four calls per candle, which is what keeps a
//! drag at the frame rate.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::cairo;
use gtk::prelude::*;
use omacharts_engine::{Bar, BarScheme, Instrument, Theme, Timeframe};

use crate::ui::colors;
use gtk::glib;

const PRICE_AXIS_W: f64 = 64.0;
const TIME_AXIS_H: f64 = 24.0;
const VOLUME_FRACTION: f64 = 0.18;
const PAD: f64 = 10.0;

/// Fewest bars we will zoom into, and the most we will draw at once.
const MIN_VISIBLE: usize = 12;
const MAX_VISIBLE: usize = 3000;

/// What the crosshair is over, handed to the window for the readout.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Hover {
    pub bar: Bar,
    pub index: usize,
}

struct State {
    bars: Vec<Bar>,
    theme: Theme,
    scheme: BarScheme,
    instrument: Option<Instrument>,
    timeframe: Timeframe,
    /// Leftmost visible bar.
    first: usize,
    /// How many bars are visible.
    visible: usize,
    pointer: Option<(f64, f64)>,
    /// Pinned to the right edge, so new bars keep the view at "now" until the
    /// user pans away.
    anchored: bool,
    drag_origin: Option<(usize, f64)>,
    stale: bool,
}

impl State {
    /// The visible slice, clamped to what we actually have.
    fn slice(&self) -> (usize, usize) {
        if self.bars.is_empty() {
            return (0, 0);
        }
        let visible = self.visible.clamp(MIN_VISIBLE, MAX_VISIBLE).min(self.bars.len());
        let first = if self.anchored {
            self.bars.len() - visible
        } else {
            self.first.min(self.bars.len() - visible)
        };
        (first, visible)
    }
}

pub struct ChartView {
    pub area: gtk::DrawingArea,
    state: Rc<RefCell<State>>,
    on_hover: Rc<RefCell<Option<Box<dyn Fn(Option<Hover>)>>>>,
}

impl ChartView {
    pub fn new(theme: Theme, scheme: BarScheme) -> ChartView {
        let area = gtk::DrawingArea::new();
        area.set_hexpand(true);
        area.set_vexpand(true);
        area.set_focusable(true);

        let state = Rc::new(RefCell::new(State {
            bars: Vec::new(),
            theme,
            scheme,
            instrument: None,
            timeframe: Timeframe::D1,
            first: 0,
            visible: 160,
            pointer: None,
            anchored: true,
            drag_origin: None,
            stale: false,
        }));
        let on_hover: Rc<RefCell<Option<Box<dyn Fn(Option<Hover>)>>>> = Rc::new(RefCell::new(None));

        let view = ChartView { area, state, on_hover };
        view.wire_drawing();
        view.wire_pointer();
        view.wire_zoom();
        view.wire_drag();
        view
    }

    pub fn set_hover_handler(&self, handler: impl Fn(Option<Hover>) + 'static) {
        *self.on_hover.borrow_mut() = Some(Box::new(handler));
    }

    /// Replace the series. Resets the view only when the instrument changed,
    /// so a background refresh of the same chart does not throw away a pan.
    pub fn set_series(&self, instrument: Instrument, timeframe: Timeframe, bars: Vec<Bar>) {
        let mut state = self.state.borrow_mut();
        let changed = state
            .instrument
            .as_ref()
            .map(|i| i.symbol != instrument.symbol || i.suffix != instrument.suffix)
            .unwrap_or(true)
            || state.timeframe != timeframe;
        state.bars = bars;
        state.instrument = Some(instrument);
        state.timeframe = timeframe;
        if changed {
            state.anchored = true;
            state.visible = 160;
        }
        drop(state);
        self.area.queue_draw();
    }

    pub fn set_stale(&self, stale: bool) {
        self.state.borrow_mut().stale = stale;
        self.area.queue_draw();
    }

    pub fn restyle(&self, theme: Theme, scheme: BarScheme) {
        {
            let mut state = self.state.borrow_mut();
            state.theme = theme;
            state.scheme = scheme;
        }
        self.area.queue_draw();
    }

    pub fn timeframe(&self) -> Timeframe {
        self.state.borrow().timeframe
    }

    pub fn bar_count(&self) -> usize {
        self.state.borrow().bars.len()
    }

    /// Jump back to the right edge and follow new bars again.
    pub fn go_to_latest(&self) {
        self.state.borrow_mut().anchored = true;
        self.area.queue_draw();
    }

    pub fn zoom(&self, factor: f64) {
        let mut state = self.state.borrow_mut();
        let (first, visible) = state.slice();
        let next = ((visible as f64 * factor).round() as usize).clamp(MIN_VISIBLE, MAX_VISIBLE);
        // Zooming while anchored keeps the right edge pinned, which is what
        // "show me more history" means.
        if !state.anchored {
            let centre = first + visible / 2;
            state.first = centre.saturating_sub(next / 2);
        }
        state.visible = next;
        drop(state);
        self.area.queue_draw();
    }

    pub fn pan_bars(&self, delta: i64) {
        let mut state = self.state.borrow_mut();
        let (first, visible) = state.slice();
        if state.bars.len() <= visible {
            return;
        }
        let max_first = state.bars.len() - visible;
        let next = (first as i64 + delta).clamp(0, max_first as i64) as usize;
        state.first = next;
        state.anchored = next >= max_first;
        drop(state);
        self.area.queue_draw();
    }

    fn wire_drawing(&self) {
        let state = self.state.clone();
        self.area.set_draw_func(move |_, cr, width, height| {
            draw(cr, width as f64, height as f64, &state.borrow());
        });
    }

    fn wire_pointer(&self) {
        let motion = gtk::EventControllerMotion::new();
        let state = self.state.clone();
        let area = self.area.clone();
        let on_hover = self.on_hover.clone();
        motion.connect_motion(move |_, x, y| {
            state.borrow_mut().pointer = Some((x, y));
            notify_hover(&state, &on_hover, &area);
            area.queue_draw();
        });

        let state = self.state.clone();
        let area = self.area.clone();
        let on_hover = self.on_hover.clone();
        motion.connect_leave(move |_| {
            state.borrow_mut().pointer = None;
            if let Some(handler) = on_hover.borrow().as_ref() {
                handler(None);
            }
            area.queue_draw();
        });
        self.area.add_controller(motion);
    }

    fn wire_zoom(&self) {
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let state = self.state.clone();
        let area = self.area.clone();
        scroll.connect_scroll(move |_, _dx, dy| {
            if dy == 0.0 {
                return glib::Propagation::Proceed;
            }
            let mut s = state.borrow_mut();
            let (first, visible) = s.slice();
            let factor = if dy > 0.0 { 1.15 } else { 1.0 / 1.15 };
            let next = ((visible as f64 * factor).round() as usize)
                .clamp(MIN_VISIBLE, MAX_VISIBLE)
                .min(s.bars.len().max(MIN_VISIBLE));

            // Zoom about the pointer: the bar under the cursor stays under it.
            if !s.anchored {
                let plot_w = (area.width() as f64 - PRICE_AXIS_W - PAD).max(1.0);
                let frac = s
                    .pointer
                    .map(|(x, _)| ((x - PAD) / plot_w).clamp(0.0, 1.0))
                    .unwrap_or(0.5);
                let focus = first as f64 + frac * visible as f64;
                s.first = (focus - frac * next as f64).max(0.0) as usize;
            }
            s.visible = next;
            drop(s);
            area.queue_draw();
            glib::Propagation::Stop
        });
        self.area.add_controller(scroll);
    }

    fn wire_drag(&self) {
        let drag = gtk::GestureDrag::new();

        let state = self.state.clone();
        drag.connect_drag_begin(move |_, _, _| {
            let mut s = state.borrow_mut();
            let (first, _) = s.slice();
            s.drag_origin = Some((first, 0.0));
        });

        let state = self.state.clone();
        let area = self.area.clone();
        drag.connect_drag_update(move |_, offset_x, _| {
            let mut s = state.borrow_mut();
            let Some((origin_first, _)) = s.drag_origin else {
                return;
            };
            let (_, visible) = s.slice();
            if s.bars.len() <= visible {
                return;
            }
            let plot_w = (area.width() as f64 - PRICE_AXIS_W - PAD).max(1.0);
            let bar_w = plot_w / visible as f64;
            // Dragging right reveals older bars.
            let shift = -(offset_x / bar_w).round() as i64;
            let max_first = s.bars.len() - visible;
            let next = (origin_first as i64 + shift).clamp(0, max_first as i64) as usize;
            s.first = next;
            s.anchored = next >= max_first;
            drop(s);
            area.queue_draw();
        });

        let state = self.state.clone();
        drag.connect_drag_end(move |_, _, _| {
            state.borrow_mut().drag_origin = None;
        });

        self.area.add_controller(drag);
    }
}

fn notify_hover(
    state: &Rc<RefCell<State>>,
    on_hover: &Rc<RefCell<Option<Box<dyn Fn(Option<Hover>)>>>>,
    area: &gtk::DrawingArea,
) {
    let hover = {
        let s = state.borrow();
        let (first, visible) = s.slice();
        match (s.pointer, visible) {
            (Some((x, _)), v) if v > 0 => {
                let plot_w = (area.width() as f64 - PRICE_AXIS_W - PAD).max(1.0);
                let frac = (x - PAD) / plot_w;
                if (0.0..=1.0).contains(&frac) {
                    let index = (first as f64 + frac * visible as f64).floor() as usize;
                    s.bars.get(index.min(s.bars.len().saturating_sub(1)))
                        .map(|bar| Hover { bar: *bar, index })
                } else {
                    None
                }
            }
            _ => None,
        }
    };
    if let Some(handler) = on_hover.borrow().as_ref() {
        handler(hover);
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

fn draw(cr: &cairo::Context, width: f64, height: f64, state: &State) {
    let ui = &state.theme.ui;

    colors::set_source(cr, &ui.background);
    let _ = cr.paint();

    let (first, visible) = state.slice();
    if visible == 0 {
        draw_placeholder(cr, width, height, state);
        return;
    }
    let bars = &state.bars[first..first + visible];

    let plot_x = PAD;
    let plot_w = (width - PRICE_AXIS_W - PAD).max(1.0);
    let plot_y = PAD;
    let total_h = (height - TIME_AXIS_H - PAD).max(1.0);
    let volume_h = (total_h * VOLUME_FRACTION).min(120.0);
    let price_h = (total_h - volume_h - 6.0).max(1.0);

    // Price scale over what is visible, padded so candles never touch the
    // edges.
    let (mut low, mut high) = (f64::MAX, f64::MIN);
    let mut max_volume: f64 = 0.0;
    for b in bars {
        low = low.min(b.low);
        high = high.max(b.high);
        max_volume = max_volume.max(b.volume);
    }
    if !low.is_finite() || !high.is_finite() {
        draw_placeholder(cr, width, height, state);
        return;
    }
    if (high - low).abs() < f64::EPSILON {
        high += 1.0;
        low -= 1.0;
    }
    let span = high - low;
    low -= span * 0.04;
    high += span * 0.04;

    let to_y = |price: f64| plot_y + price_h * (high - price) / (high - low);
    let bar_w = plot_w / visible as f64;

    draw_price_grid(cr, state, plot_x, plot_w, plot_y, price_h, low, high, &to_y);
    draw_time_axis(cr, state, bars, plot_x, plot_w, height, bar_w, first);
    draw_candles(cr, state, bars, plot_x, bar_w, &to_y);

    if max_volume > 0.0 {
        draw_volume(cr, state, bars, plot_x, bar_w, plot_y + price_h + 6.0, volume_h, max_volume);
    }

    draw_last_price(cr, state, plot_x, plot_w, width, &to_y);

    if let Some((px, py)) = state.pointer {
        draw_crosshair(
            cr, state, px, py, plot_x, plot_w, plot_y, price_h, width, height, bar_w, first, low,
            high, &to_y,
        );
    }

    if state.stale {
        draw_stale_marker(cr, state, width);
    }
}

fn draw_placeholder(cr: &cairo::Context, width: f64, height: f64, state: &State) {
    let text = if state.instrument.is_none() {
        "Press Ctrl+K to find a symbol"
    } else {
        "No data yet"
    };
    colors::set_source_alpha(cr, &state.theme.ui.text_muted, 0.8);
    cr.select_font_face("sans-serif", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(13.0);
    if let Ok(extents) = cr.text_extents(text) {
        cr.move_to((width - extents.width()) / 2.0, height / 2.0);
        let _ = cr.show_text(text);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_price_grid(
    cr: &cairo::Context,
    state: &State,
    plot_x: f64,
    plot_w: f64,
    plot_y: f64,
    price_h: f64,
    low: f64,
    high: f64,
    to_y: &impl Fn(f64) -> f64,
) {
    let step = nice_step(high - low, (price_h / 52.0).max(2.0) as usize);
    if step <= 0.0 {
        return;
    }
    let decimals = decimals_for(step);

    cr.set_line_width(1.0);
    cr.select_font_face("sans-serif", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(11.0);

    let mut price = (low / step).ceil() * step;
    while price <= high {
        let y = to_y(price).round() + 0.5;
        if y > plot_y && y < plot_y + price_h {
            colors::set_source(cr, &state.theme.ui.grid);
            cr.move_to(plot_x, y);
            cr.line_to(plot_x + plot_w, y);
            let _ = cr.stroke();

            colors::set_source_alpha(cr, &state.theme.ui.text_muted, 0.9);
            cr.move_to(plot_x + plot_w + 6.0, y + 3.5);
            let _ = cr.show_text(&format!("{price:.decimals$}"));
        }
        price += step;
    }

    // The axis itself.
    colors::set_source(cr, &state.theme.ui.axis);
    let x = (plot_x + plot_w).round() + 0.5;
    cr.move_to(x, plot_y);
    cr.line_to(x, plot_y + price_h);
    let _ = cr.stroke();
}

#[allow(clippy::too_many_arguments)]
fn draw_time_axis(
    cr: &cairo::Context,
    state: &State,
    bars: &[Bar],
    plot_x: f64,
    plot_w: f64,
    height: f64,
    bar_w: f64,
    _first: usize,
) {
    let y = height - TIME_AXIS_H;
    colors::set_source(cr, &state.theme.ui.axis);
    cr.set_line_width(1.0);
    cr.move_to(plot_x, y.round() + 0.5);
    cr.line_to(plot_x + plot_w, y.round() + 0.5);
    let _ = cr.stroke();

    cr.select_font_face("sans-serif", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(11.0);

    // About one label per 90px, on a whole number of bars so labels do not
    // jitter as the view scrolls.
    let target = (plot_w / 90.0).max(2.0) as usize;
    let stride = (bars.len() / target).max(1);
    // How much time is on screen decides the format. A decade of daily bars
    // labelled "01 Jun" tells you nothing.
    let span = match (bars.first(), bars.last()) {
        (Some(first), Some(last)) => last.ts - first.ts,
        _ => 0,
    };
    for (i, bar) in bars.iter().enumerate() {
        if i % stride != 0 {
            continue;
        }
        let x = plot_x + (i as f64 + 0.5) * bar_w;
        if x < plot_x + 18.0 || x > plot_x + plot_w - 18.0 {
            continue;
        }
        colors::set_source(cr, &state.theme.ui.grid);
        cr.move_to(x.round() + 0.5, PAD);
        cr.line_to(x.round() + 0.5, y);
        let _ = cr.stroke();

        let label = format_axis_time(bar.ts, span);
        colors::set_source_alpha(cr, &state.theme.ui.text_muted, 0.9);
        if let Ok(extents) = cr.text_extents(&label) {
            cr.move_to(x - extents.width() / 2.0, height - 7.0);
            let _ = cr.show_text(&label);
        }
    }
}

/// Up and down candles in two passes, each building one path for wicks and one
/// for bodies.
fn draw_candles(
    cr: &cairo::Context,
    state: &State,
    bars: &[Bar],
    plot_x: f64,
    bar_w: f64,
    to_y: &impl Fn(f64) -> f64,
) {
    let scheme = &state.scheme;
    let body_w = (bar_w * 0.68).clamp(1.0, 24.0);
    // Below about three pixels a candle is a line; outlining it just muddies
    // the colour.
    let hairline = bar_w < 3.0;

    for rising in [false, true] {
        let (outline, fill) = if rising {
            (&scheme.up, &scheme.up_fill)
        } else {
            (&scheme.down, &scheme.down_fill)
        };

        // Wicks.
        cr.set_line_width(1.0);
        colors::set_source(cr, outline);
        let mut any = false;
        for (i, bar) in bars.iter().enumerate() {
            if (bar.close >= bar.open) != rising {
                continue;
            }
            any = true;
            let x = (plot_x + (i as f64 + 0.5) * bar_w).round() + 0.5;
            cr.move_to(x, to_y(bar.high).round());
            cr.line_to(x, to_y(bar.low).round());
        }
        if any {
            let _ = cr.stroke();
        }
        if !any {
            continue;
        }

        if hairline {
            // At this density the body is the wick.
            continue;
        }

        // Bodies, as one path.
        for (i, bar) in bars.iter().enumerate() {
            if (bar.close >= bar.open) != rising {
                continue;
            }
            let x = plot_x + (i as f64 + 0.5) * bar_w - body_w / 2.0;
            let top = to_y(bar.open.max(bar.close)).round();
            let bottom = to_y(bar.open.min(bar.close)).round();
            cr.rectangle(x.round(), top, body_w.round(), (bottom - top).max(1.0));
        }
        if colors::is_transparent(fill) {
            // Hollow: stroke the outline and leave the background showing.
            colors::set_source(cr, outline);
            let _ = cr.stroke();
        } else {
            colors::set_source(cr, fill);
            let _ = cr.fill();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_volume(
    cr: &cairo::Context,
    state: &State,
    bars: &[Bar],
    plot_x: f64,
    bar_w: f64,
    top: f64,
    height: f64,
    max_volume: f64,
) {
    let body_w = (bar_w * 0.68).clamp(1.0, 24.0);
    for rising in [false, true] {
        let colour = if rising { &state.scheme.volume_up } else { &state.scheme.volume_down };
        let mut any = false;
        for (i, bar) in bars.iter().enumerate() {
            if (bar.close >= bar.open) != rising || bar.volume <= 0.0 {
                continue;
            }
            any = true;
            let h = (bar.volume / max_volume * height).max(1.0);
            let x = plot_x + (i as f64 + 0.5) * bar_w - body_w / 2.0;
            cr.rectangle(x.round(), (top + height - h).round(), body_w.round(), h.round());
        }
        if any {
            colors::set_source_alpha(cr, colour, 0.85);
            let _ = cr.fill();
        }
    }
}

fn draw_last_price(
    cr: &cairo::Context,
    state: &State,
    plot_x: f64,
    plot_w: f64,
    width: f64,
    to_y: &impl Fn(f64) -> f64,
) {
    let Some(last) = state.bars.last() else { return };
    let y = to_y(last.close).round() + 0.5;
    if y < PAD {
        return;
    }
    let rising = last.close >= last.open;
    let colour = if rising { &state.scheme.up } else { &state.scheme.down };

    cr.save().ok();
    cr.set_dash(&[3.0, 3.0], 0.0);
    cr.set_line_width(1.0);
    colors::set_source_alpha(cr, colour, 0.7);
    cr.move_to(plot_x, y);
    cr.line_to(plot_x + plot_w, y);
    let _ = cr.stroke();
    cr.restore().ok();

    label_on_axis(cr, state, &format!("{:.2}", last.close), plot_x + plot_w, y, width, colour);
}

#[allow(clippy::too_many_arguments)]
fn draw_crosshair(
    cr: &cairo::Context,
    state: &State,
    px: f64,
    py: f64,
    plot_x: f64,
    plot_w: f64,
    plot_y: f64,
    price_h: f64,
    width: f64,
    height: f64,
    bar_w: f64,
    first: usize,
    low: f64,
    high: f64,
    to_y: &impl Fn(f64) -> f64,
) {
    if px < plot_x || px > plot_x + plot_w || py < plot_y || py > height - TIME_AXIS_H {
        return;
    }
    let crosshair = &state.theme.ui.crosshair;

    // Snap to the centre of the bar under the pointer.
    let index_in_view = ((px - plot_x) / bar_w).floor();
    let snapped_x = (plot_x + (index_in_view + 0.5) * bar_w).round() + 0.5;

    cr.save().ok();
    cr.set_dash(&[2.0, 3.0], 0.0);
    cr.set_line_width(1.0);
    colors::set_source_alpha(cr, crosshair, 0.55);
    cr.move_to(snapped_x, plot_y);
    cr.line_to(snapped_x, height - TIME_AXIS_H);
    cr.move_to(plot_x, py.round() + 0.5);
    cr.line_to(plot_x + plot_w, py.round() + 0.5);
    let _ = cr.stroke();
    cr.restore().ok();

    // Price under the pointer, only while it is over the price pane.
    if py <= plot_y + price_h {
        let price = high - (py - plot_y) / price_h * (high - low);
        let step = nice_step(high - low, (price_h / 52.0).max(2.0) as usize);
        let decimals = decimals_for(step);
        label_on_axis(
            cr,
            state,
            &format!("{price:.decimals$}"),
            plot_x + plot_w,
            py.round() + 0.5,
            width,
            crosshair,
        );
    }
    let _ = to_y;

    // Time under the pointer.
    if let Some(bar) = state.bars.get(first + index_in_view.max(0.0) as usize) {
        let label = format_time_full(bar.ts, state.timeframe);
        cr.select_font_face("sans-serif", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
        cr.set_font_size(11.0);
        if let Ok(extents) = cr.text_extents(&label) {
            let w = extents.width() + 10.0;
            let x = (snapped_x - w / 2.0).clamp(plot_x, plot_x + plot_w - w);
            let y = height - TIME_AXIS_H + 2.0;
            colors::set_source(cr, crosshair);
            cr.rectangle(x, y, w, TIME_AXIS_H - 4.0);
            let _ = cr.fill();
            colors::set_source(cr, colors::readable_on(crosshair));
            cr.move_to(x + 5.0, y + TIME_AXIS_H - 10.0);
            let _ = cr.show_text(&label);
        }
    }
}

/// A filled chip on the price axis.
fn label_on_axis(
    cr: &cairo::Context,
    state: &State,
    text: &str,
    axis_x: f64,
    y: f64,
    width: f64,
    colour: &str,
) {
    cr.select_font_face("sans-serif", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(11.0);
    let Ok(extents) = cr.text_extents(text) else { return };
    let h = 16.0;
    let w = (extents.width() + 10.0).min(width - axis_x - 2.0);
    colors::set_source(cr, colour);
    cr.rectangle(axis_x + 1.0, y - h / 2.0, w, h);
    let _ = cr.fill();
    colors::set_source(cr, colors::readable_on(colour));
    cr.move_to(axis_x + 6.0, y + 3.5);
    let _ = cr.show_text(text);
    let _ = state;
}

fn draw_stale_marker(cr: &cairo::Context, state: &State, width: f64) {
    let text = "stale";
    cr.select_font_face("sans-serif", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(10.0);
    let Ok(extents) = cr.text_extents(text) else { return };
    let w = extents.width() + 12.0;
    let x = width - PRICE_AXIS_W - w - PAD;
    colors::set_source_alpha(cr, &state.theme.ui.text_muted, 0.22);
    cr.rectangle(x, PAD, w, 16.0);
    let _ = cr.fill();
    colors::set_source_alpha(cr, &state.theme.ui.text_muted, 0.95);
    cr.move_to(x + 6.0, PAD + 11.5);
    let _ = cr.show_text(text);
}

// ---------------------------------------------------------------------------
// Scales and formatting
// ---------------------------------------------------------------------------

/// A round step — 1, 2, 2.5 or 5 times a power of ten — giving roughly
/// `target` gridlines across `range`.
pub fn nice_step(range: f64, target: usize) -> f64 {
    if range <= 0.0 || target == 0 {
        return 0.0;
    }
    let rough = range / target as f64;
    let magnitude = 10f64.powf(rough.log10().floor());
    let normalised = rough / magnitude;
    let step = if normalised <= 1.0 {
        1.0
    } else if normalised <= 2.0 {
        2.0
    } else if normalised <= 2.5 {
        2.5
    } else if normalised <= 5.0 {
        5.0
    } else {
        10.0
    };
    step * magnitude
}

/// Enough decimals to tell two adjacent gridlines apart, and no more.
pub fn decimals_for(step: f64) -> usize {
    if step <= 0.0 {
        return 2;
    }
    let places = -step.log10().floor();
    places.clamp(0.0, 6.0) as usize
}

/// Label an axis tick at a detail the visible span justifies.
fn format_axis_time(ts: i64, span_seconds: i64) -> String {
    use chrono::{Local, TimeZone};
    let Some(dt) = Local.timestamp_opt(ts, 0).single() else {
        return String::new();
    };
    const DAY: i64 = 86_400;
    match span_seconds {
        s if s > 1460 * DAY => dt.format("%Y").to_string(),
        s if s > 160 * DAY => dt.format("%b %Y").to_string(),
        s if s > 4 * DAY => dt.format("%d %b").to_string(),
        s if s > DAY => dt.format("%a %H:%M").to_string(),
        _ => dt.format("%H:%M").to_string(),
    }
}

/// The crosshair label always carries the year — it is the one place you look
/// to know exactly which bar you are on.
fn format_time_full(ts: i64, timeframe: Timeframe) -> String {
    use chrono::{Local, TimeZone};
    let Some(dt) = Local.timestamp_opt(ts, 0).single() else {
        return String::new();
    };
    if timeframe.is_intraday() {
        dt.format("%d %b %Y %H:%M").to_string()
    } else {
        dt.format("%a %d %b %Y").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_are_round_numbers() {
        assert_eq!(nice_step(100.0, 5), 20.0);
        assert_eq!(nice_step(10.0, 5), 2.0);
        assert_eq!(nice_step(1.0, 4), 0.25);
        assert_eq!(nice_step(0.0, 5), 0.0);
        assert_eq!(nice_step(100.0, 0), 0.0);
    }

    #[test]
    fn steps_scale_across_magnitudes() {
        // A chart of an index in the thousands and one of a penny stock both
        // need readable gridlines.
        for range in [0.01, 0.5, 7.0, 320.0, 48000.0] {
            let step = nice_step(range, 6);
            assert!(step > 0.0, "{range}");
            let lines = range / step;
            assert!((2.0..=15.0).contains(&lines), "{range} -> {lines} lines");
        }
    }

    #[test]
    fn axis_labels_match_the_span_on_screen() {
        const DAY: i64 = 86_400;
        // A fixed instant so the assertions do not drift with the clock.
        let ts = 1_700_000_000;

        // Decades: years only. This is the case that read "01 Jun" for every
        // tick before the span was taken into account.
        let decade = format_axis_time(ts, 4000 * DAY);
        assert_eq!(decade.len(), 4, "{decade}");
        assert!(decade.chars().all(|c| c.is_ascii_digit()), "{decade}");

        // A year or two: month and year.
        assert!(format_axis_time(ts, 400 * DAY).contains("20"));
        // A few weeks: day and month, no year.
        assert!(!format_axis_time(ts, 30 * DAY).contains("20"));
        // Intraday: a clock.
        assert!(format_axis_time(ts, 3600).contains(':'));
    }

    #[test]
    fn two_ticks_a_decade_apart_get_different_labels() {
        const DAY: i64 = 86_400;
        let span = 4000 * DAY;
        let a = format_axis_time(1_200_000_000, span);
        let b = format_axis_time(1_700_000_000, span);
        assert_ne!(a, b);
    }

    #[test]
    fn decimals_follow_the_step() {
        assert_eq!(decimals_for(100.0), 0);
        assert_eq!(decimals_for(1.0), 0);
        assert_eq!(decimals_for(0.5), 1);
        assert_eq!(decimals_for(0.01), 2);
        assert_eq!(decimals_for(0.0001), 4);
    }
}
