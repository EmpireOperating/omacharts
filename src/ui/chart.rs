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
use omacharts_engine::indicators::{vwap, Output, Profile};
use omacharts_engine::{
    Bar, BarScheme, BarStyle, Direction, Indicator, Instrument, Theme, Timeframe,
};

use crate::ui::colors;
use gtk::glib;

const PRICE_AXIS_W: f64 = 64.0;
const TIME_AXIS_H: f64 = 24.0;
const PAD: f64 = 10.0;
/// Space between the price chart and a pane, and between two panes.
const PANE_GAP: f64 = 6.0;
/// The least of the chart price keeps, however many panes are stacked below.
const MIN_PRICE_SHARE: f64 = 0.45;

/// Fewest bars we will zoom into, and the most we will draw at once.
const MIN_VISIBLE: usize = 12;
const MAX_VISIBLE: usize = 3000;

/// How far the price scale may be stretched either way.
const MIN_PRICE_ZOOM: f64 = 0.05;
const MAX_PRICE_ZOOM: f64 = 40.0;

/// Pixels of drag for one doubling of either scale.
const DRAG_PER_DOUBLING: f64 = 180.0;

/// One indicator, computed and coloured, ready to draw.
///
/// The chart does no arithmetic: the window computes outputs through the
/// engine and resolves colours through the theme, and this is what arrives.
#[derive(Clone, PartialEq, Debug)]
pub struct Drawn {
    pub indicator: Indicator,
    pub output: Output,
    pub color: String,
}

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
    drag: Option<Drag>,
    indicators: Vec<Drawn>,
    bar_style: BarStyle,
    show_grid: bool,
    stale: bool,
    loading: bool,
    /// Multiplier on the auto-fitted price range. 1.0 shows exactly what the
    /// visible bars need; above that is zoomed in.
    price_zoom: f64,
    /// Vertical shift, as a fraction of the auto-fitted range.
    price_offset: f64,
    /// While true the price scale follows the data and the two values above
    /// are held at their neutral settings.
    price_auto: bool,
}

/// What a drag is doing, decided by where it started.
///
/// This is the whole of the direct-manipulation model: the chart body pans,
/// the price axis scales price, the time axis scales time. Same as every other
/// charting tool, because muscle memory is the feature.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Drag {
    Pan { first: usize, offset: f64 },
    PriceScale { zoom: f64 },
    TimeScale { visible: usize },
}

/// Which part of the widget a point is over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Region {
    Plot,
    PriceAxis,
    TimeAxis,
}

fn region_at(x: f64, y: f64, width: f64, height: f64) -> Region {
    if x >= width - PRICE_AXIS_W {
        Region::PriceAxis
    } else if y >= height - TIME_AXIS_H {
        Region::TimeAxis
    } else {
        Region::Plot
    }
}

impl State {
    /// The price range to draw, after the user's scaling.
    ///
    /// The auto fit is always the starting point, so taking manual control
    /// never makes the chart jump — it stretches around what was already on
    /// screen.
    fn price_window(&self, low: f64, high: f64) -> (f64, f64) {
        if self.price_auto {
            return (low, high);
        }
        let range = (high - low).max(f64::EPSILON);
        let middle = (low + high) / 2.0 + range * self.price_offset;
        let half = range / 2.0 / self.price_zoom.clamp(MIN_PRICE_ZOOM, MAX_PRICE_ZOOM);
        (middle - half, middle + half)
    }

    /// Shift the view by a number of bars. Positive moves forward in time.
    fn pan_by(&mut self, bars: f64) {
        let (first, visible) = self.slice();
        if self.bars.len() <= visible {
            return;
        }
        let max_first = self.bars.len() - visible;
        let next = (first as i64 + bars.round() as i64).clamp(0, max_first as i64) as usize;
        self.first = next;
        self.anchored = next >= max_first;
    }

    /// Zoom the time axis about `anchor`, a fraction across the plot.
    fn zoom_time(&mut self, factor: f64, anchor: f64) {
        let (first, visible) = self.slice();
        let next = ((visible as f64 * factor).round() as usize)
            .clamp(MIN_VISIBLE, MAX_VISIBLE)
            .min(self.bars.len().max(MIN_VISIBLE));
        // Anchored to the right edge, zooming reveals history and the last bar
        // stays put — which is what you want when looking at the live edge.
        if !self.anchored {
            let focus = first as f64 + anchor * visible as f64;
            self.first = (focus - anchor * next as f64).max(0.0) as usize;
        }
        self.visible = next;
    }

    /// Stretch or compress the price scale, taking it off automatic.
    fn scale_price(&mut self, factor: f64) {
        if self.price_auto {
            self.price_auto = false;
            self.price_zoom = 1.0;
            self.price_offset = 0.0;
        }
        self.price_zoom = (self.price_zoom * factor).clamp(MIN_PRICE_ZOOM, MAX_PRICE_ZOOM);
    }

    /// Back to fitting the data, at the live edge.
    fn reset_view(&mut self) {
        self.price_auto = true;
        self.price_zoom = 1.0;
        self.price_offset = 0.0;
        self.visible = 160;
        self.anchored = true;
    }

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
    on_context_menu: Rc<RefCell<Option<Box<dyn Fn(f64, f64)>>>>,
}

impl ChartView {
    pub fn new(theme: Theme, scheme: BarScheme) -> Rc<ChartView> {
        let area = gtk::DrawingArea::new();
        area.set_hexpand(true);
        area.set_vexpand(true);
        area.set_focusable(true);

        let state = Rc::new(RefCell::new(State {
            bars: Vec::new(),
            theme,
            scheme,
            instrument: None,
            timeframe: Timeframe::days(1),
            first: 0,
            visible: 160,
            pointer: None,
            anchored: true,
            indicators: Vec::new(),
            bar_style: BarStyle::default(),
            show_grid: true,
            drag: None,
            stale: false,
            loading: false,
            price_zoom: 1.0,
            price_offset: 0.0,
            price_auto: true,
        }));
        let on_hover: Rc<RefCell<Option<Box<dyn Fn(Option<Hover>)>>>> = Rc::new(RefCell::new(None));

        let view = Rc::new(ChartView {
            area,
            state,
            on_hover,
            on_context_menu: Rc::new(RefCell::new(None)),
        });
        view.wire_drawing();
        view.wire_pointer();
        view.wire_zoom();
        view.wire_drag();
        view.wire_axis_menu();
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

    /// Show or hide the gridlines. The axes and their labels stay: without
    /// them a chart is a shape with no scale.
    pub fn set_show_grid(&self, show: bool) {
        self.state.borrow_mut().show_grid = show;
        self.area.queue_draw();
    }

    pub fn set_bar_style(&self, style: BarStyle) {
        self.state.borrow_mut().bar_style = style;
        self.area.queue_draw();
    }

    /// What to do when the chart itself is right-clicked. The axis keeps its
    /// own menu.
    pub fn set_context_menu_handler(&self, handler: impl Fn(f64, f64) + 'static) {
        *self.on_context_menu.borrow_mut() = Some(Box::new(handler));
    }

    pub fn set_indicators(&self, indicators: Vec<Drawn>) {
        self.state.borrow_mut().indicators = indicators;
        self.area.queue_draw();
    }

    pub fn set_stale(&self, stale: bool) {
        self.state.borrow_mut().stale = stale;
        self.area.queue_draw();
    }

    /// Whether a fetch is outstanding for what is on screen.
    ///
    /// An empty chart that is still loading and an empty chart that came back
    /// empty are different things, and telling the user they are the same is
    /// how "no data" ends up meaning nothing.
    pub fn set_loading(&self, loading: bool) {
        self.state.borrow_mut().loading = loading;
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

    /// How many rows the volume profile with this id is actually drawing.
    ///
    /// The settings panel shows this when the count is automatic, so the
    /// number on screen is the number being drawn rather than a placeholder.
    pub fn profile_rows(&self, id: u32) -> Option<usize> {
        let state = self.state.borrow();
        state.indicators.iter().find(|drawn| drawn.indicator.id == id).and_then(|drawn| {
            match &drawn.output {
                Output::Profiles(profiles) => profiles.first().map(|p| p.rows.len()),
                _ => None,
            }
        })
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
        self.state.borrow_mut().zoom_time(factor, 0.5);
        self.area.queue_draw();
    }

    pub fn pan_bars(&self, delta: i64) {
        self.state.borrow_mut().pan_by(delta as f64);
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

    /// The wheel, with the conventions every charting tool shares.
    ///
    /// Plain wheel zooms time about the cursor. Shift pans. Ctrl scales price.
    /// Over an axis, the wheel scales that axis whatever the modifiers — the
    /// axis is the control.
    fn wire_zoom(&self) {
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let state = self.state.clone();
        let area = self.area.clone();
        scroll.connect_scroll(move |controller, dx, dy| {
            if dx == 0.0 && dy == 0.0 {
                return glib::Propagation::Proceed;
            }
            let modifiers = controller.current_event_state();
            let shift = modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK);
            let ctrl = modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK);

            let (width, height) = (area.width() as f64, area.height() as f64);
            let mut s = state.borrow_mut();
            let region = s
                .pointer
                .map(|(x, y)| region_at(x, y, width, height))
                .unwrap_or(Region::Plot);

            // A trackpad's horizontal axis always pans, whatever is held.
            if dx != 0.0 && dy == 0.0 {
                s.pan_by(dx * 2.0);
                drop(s);
                area.queue_draw();
                return glib::Propagation::Stop;
            }

            match (region, shift, ctrl) {
                (Region::PriceAxis, _, _) | (Region::Plot, false, true) => {
                    // Same sense as dragging the axis.
                    s.scale_price(2f64.powf(dy / 4.0));
                }
                (Region::Plot, true, _) => {
                    let visible = s.slice().1 as f64;
                    s.pan_by(dy * visible / 20.0);
                }
                _ => {
                    let plot_w = (width - PRICE_AXIS_W - PAD).max(1.0);
                    let anchor = s
                        .pointer
                        .map(|(x, _)| ((x - PAD) / plot_w).clamp(0.0, 1.0))
                        .unwrap_or(0.5);
                    s.zoom_time(if dy > 0.0 { 1.15 } else { 1.0 / 1.15 }, anchor);
                }
            }
            drop(s);
            area.queue_draw();
            glib::Propagation::Stop
        });
        self.area.add_controller(scroll);
    }

    fn wire_drag(&self) {
        let drag = gtk::GestureDrag::new();

        let state = self.state.clone();
        let area = self.area.clone();
        drag.connect_drag_begin(move |_, x, y| {
            let mut s = state.borrow_mut();
            let (first, visible) = s.slice();
            s.drag = Some(
                match region_at(x, y, area.width() as f64, area.height() as f64) {
                    Region::PriceAxis => {
                        // Touching the axis takes the scale off automatic, from
                        // exactly where it was, so nothing jumps.
                        if s.price_auto {
                            s.price_auto = false;
                            s.price_zoom = 1.0;
                            s.price_offset = 0.0;
                        }
                        Drag::PriceScale { zoom: s.price_zoom }
                    }
                    Region::TimeAxis => Drag::TimeScale { visible },
                    Region::Plot => Drag::Pan { first, offset: s.price_offset },
                },
            );
        });

        let state = self.state.clone();
        let area = self.area.clone();
        drag.connect_drag_update(move |_, offset_x, offset_y| {
            let mut s = state.borrow_mut();
            let Some(drag) = s.drag else { return };
            let plot_w = (area.width() as f64 - PRICE_AXIS_W - PAD).max(1.0);

            match drag {
                Drag::Pan { first, offset } => {
                    let (_, visible) = s.slice();
                    if s.bars.len() > visible {
                        let bar_w = plot_w / visible as f64;
                        // Dragging right reveals older bars.
                        let shift = -(offset_x / bar_w).round() as i64;
                        let max_first = s.bars.len() - visible;
                        let next = (first as i64 + shift).clamp(0, max_first as i64) as usize;
                        s.first = next;
                        s.anchored = next >= max_first;
                    }
                    // Vertical panning only means something once the scale is
                    // no longer fitting itself to the data. The content follows
                    // the hand: drag down and the bars come down with it, which
                    // means the window moves up the price axis.
                    if !s.price_auto {
                        let plot_h = (area.height() as f64 - TIME_AXIS_H - PAD).max(1.0);
                        s.price_offset = offset + offset_y / plot_h / s.price_zoom;
                    }
                }
                Drag::PriceScale { zoom } => {
                    // Grabbing the axis and pulling up stretches it: the
                    // numbers spread apart and less price fits on screen.
                    // Pulling down squeezes them together and shows more.
                    let factor = 2f64.powf(offset_y / DRAG_PER_DOUBLING);
                    s.price_zoom = (zoom * factor).clamp(MIN_PRICE_ZOOM, MAX_PRICE_ZOOM);
                }
                Drag::TimeScale { visible } => {
                    // Dragging left compresses: more time on screen.
                    let factor = 2f64.powf(-offset_x / DRAG_PER_DOUBLING);
                    let next = ((visible as f64 * factor).round() as usize)
                        .clamp(MIN_VISIBLE, MAX_VISIBLE);
                    s.visible = next;
                }
            }
            drop(s);
            area.queue_draw();
        });

        let state = self.state.clone();
        drag.connect_drag_end(move |_, _, _| {
            state.borrow_mut().drag = None;
        });

        self.area.add_controller(drag);

        // Double-clicking an axis puts it back on automatic, which is the way
        // out of any scale you have stretched into uselessness.
        let click = gtk::GestureClick::new();
        let state = self.state.clone();
        let area = self.area.clone();
        click.connect_pressed(move |_, presses, x, y| {
            if presses < 2 {
                return;
            }
            let region = region_at(x, y, area.width() as f64, area.height() as f64);
            let mut s = state.borrow_mut();
            match region {
                Region::PriceAxis => {
                    s.price_auto = true;
                    s.price_zoom = 1.0;
                    s.price_offset = 0.0;
                }
                Region::TimeAxis => {
                    s.visible = 160;
                    s.anchored = true;
                }
                // Double-clicking the chart itself does nothing, the same as
                // everywhere else. Resetting is Alt+R or the axis menu.
                Region::Plot => return,
            }
            drop(s);
            area.queue_draw();
        });
        self.area.add_controller(click);
    }

    /// Put the price scale back to fitting the data.
    pub fn reset_price_scale(&self) {
        let mut state = self.state.borrow_mut();
        state.price_auto = true;
        state.price_zoom = 1.0;
        state.price_offset = 0.0;
        drop(state);
        self.area.queue_draw();
    }

    /// Everything back to how the chart opens. Alt+R, and the axis menu.
    pub fn reset_view(&self) {
        self.state.borrow_mut().reset_view();
        self.area.queue_draw();
    }

    /// Offer the reset where people right-click for it.
    fn wire_axis_menu(self: &Rc<Self>) {
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_SECONDARY);
        let state = self.state.clone();
        let area = self.area.clone();
        let on_context_menu = self.on_context_menu.clone();
        click.connect_pressed(move |_, _, x, y| {
            let (width, height) = (area.width() as f64, area.height() as f64);
            if region_at(x, y, width, height) != Region::PriceAxis {
                // The chart's own menu belongs to whoever owns the chart.
                if let Some(handler) = on_context_menu.borrow().as_ref() {
                    handler(x, y);
                }
                return;
            }
            let items = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let popover = gtk::Popover::new();
            popover.set_child(Some(&items));
            popover.set_parent(&area);
            popover.set_has_arrow(false);
            popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));

            for (label, reset_all) in [("Auto scale price", false), ("Reset chart (Alt+R)", true)] {
                let button = gtk::Button::with_label(label);
                button.add_css_class("flat");
                if let Some(child) = button.child().and_downcast::<gtk::Label>() {
                    child.set_xalign(0.0);
                }
                let state = state.clone();
                let area = area.clone();
                let popover_weak = popover.downgrade();
                button.connect_clicked(move |_| {
                    let mut s = state.borrow_mut();
                    if reset_all {
                        s.reset_view();
                    } else {
                        s.price_auto = true;
                        s.price_zoom = 1.0;
                        s.price_offset = 0.0;
                    }
                    drop(s);
                    area.queue_draw();
                    if let Some(p) = popover_weak.upgrade() {
                        p.popdown();
                    }
                });
                items.append(&button);
            }
            popover.popup();
        });
        self.area.add_controller(click);
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
    // Volume, RSI and ATR each want a strip of their own, stacked under the
    // price in the order they are listed. Price keeps most of the chart
    // whatever is stacked below it: three panes sharing the window equally
    // would leave the candles — the thing you came for — as a sliver.
    let panes: Vec<(&Drawn, f64)> = state
        .indicators
        .iter()
        .filter(|drawn| drawn.indicator.visible)
        .filter_map(|drawn| drawn.output.pane_height().map(|share| (drawn, share)))
        .collect();
    let wanted: f64 = panes.iter().map(|(_, share)| share).sum();
    let squeeze = if wanted > 1.0 - MIN_PRICE_SHARE {
        (1.0 - MIN_PRICE_SHARE) / wanted
    } else {
        1.0
    };
    let pane_heights: Vec<f64> =
        panes.iter().map(|(_, share)| (total_h * share * squeeze).min(220.0)).collect();
    let panes_h: f64 = pane_heights.iter().sum::<f64>() + PANE_GAP * panes.len() as f64;
    let price_h = (total_h - panes_h).max(1.0);

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
    let (low, high) = state.price_window(low, high);

    let to_y = |price: f64| plot_y + price_h * (high - price) / (high - low);
    let bar_w = plot_w / visible as f64;

    // One answer for how precisely prices are written, used by the gridlines,
    // the last-price chip and the crosshair alike — a chart saying 1.1257 on
    // one label and 1.13 on another is describing two different prices.
    let step = nice_step(high - low, (price_h / 52.0).max(2.0) as usize);
    let kind = state.instrument.as_ref().map(|i| i.kind);
    let decimals = omacharts_engine::price_decimals(step, (low + high) / 2.0, kind);

    draw_price_grid(cr, state, plot_x, plot_w, plot_y, price_h, low, high, &to_y);
    draw_time_axis(cr, state, bars, plot_x, plot_w, height, bar_w, first);
    // Shaded things go under the candles; lines go over. A band drawn on top
    // of the bars hides the thing it is describing.
    draw_indicator_fills(cr, state, first, visible, plot_x, bar_w, &to_y);
    draw_candles(cr, state, bars, plot_x, bar_w, &to_y);
    draw_indicator_lines(cr, state, first, visible, plot_x, bar_w, &to_y);

    let mut pane_y = plot_y + price_h + PANE_GAP;
    for ((drawn, _), pane_h) in panes.iter().zip(&pane_heights) {
        match &drawn.output {
            Output::Volume { .. } if max_volume > 0.0 => {
                draw_volume(cr, state, bars, plot_x, bar_w, pane_y, *pane_h, max_volume);
            }
            Output::Pane(pane) => draw_pane(
                cr, state, pane, drawn, plot_x, plot_w, bar_w, pane_y, *pane_h, width, first,
                visible,
            ),
            _ => {}
        }
        pane_y += pane_h + PANE_GAP;
    }

    draw_last_price(cr, state, plot_x, plot_w, width, decimals, &to_y);

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
    let text = match (&state.instrument, state.loading) {
        (None, _) => "Press Ctrl+K to find a symbol",
        (Some(_), true) => "Loading…",
        (Some(_), false) => "No data for this symbol",
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
    let decimals = omacharts_engine::price_decimals(
        step,
        (low + high) / 2.0,
        state.instrument.as_ref().map(|i| i.kind),
    );

    cr.set_line_width(1.0);
    cr.select_font_face("sans-serif", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(11.0);

    let mut price = (low / step).ceil() * step;
    while price <= high {
        let y = to_y(price).round() + 0.5;
        if y > plot_y && y < plot_y + price_h {
            if state.show_grid {
                colors::set_source(cr, &state.theme.ui.grid);
                cr.move_to(plot_x, y);
                cr.line_to(plot_x + plot_w, y);
                let _ = cr.stroke();
            }

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
    // How far apart two labels land decides the format: a decade of daily bars
    // labelled "01 Jun" tells you nothing, and nine months of them all
    // labelled "Mar 2026" tells you less.
    let span = match (bars.first(), bars.last()) {
        (Some(first), Some(last)) => last.ts - first.ts,
        _ => 0,
    };
    let labels = (bars.len().div_ceil(stride)).max(1) as i64;
    let tick_seconds = span / labels;
    for (i, bar) in bars.iter().enumerate() {
        if i % stride != 0 {
            continue;
        }
        let x = plot_x + (i as f64 + 0.5) * bar_w;
        if x < plot_x + 18.0 || x > plot_x + plot_w - 18.0 {
            continue;
        }
        if state.show_grid {
            colors::set_source(cr, &state.theme.ui.grid);
            cr.move_to(x.round() + 0.5, PAD);
            cr.line_to(x.round() + 0.5, y);
            let _ = cr.stroke();
        }

        let label = format_axis_time(bar.ts, tick_seconds, state.timeframe.is_intraday());
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
    if state.bar_style == BarStyle::Ohlc {
        draw_ohlc(cr, state, bars, plot_x, bar_w, to_y);
        return;
    }
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

/// Open and close as ticks either side of a high-low line.
fn draw_ohlc(
    cr: &cairo::Context,
    state: &State,
    bars: &[Bar],
    plot_x: f64,
    bar_w: f64,
    to_y: &impl Fn(f64) -> f64,
) {
    let tick = (bar_w * 0.32).clamp(1.0, 10.0);
    cr.set_line_width(1.0);
    for rising in [false, true] {
        let direction = if rising { Direction::Up } else { Direction::Down };
        let mut any = false;
        for (i, bar) in bars.iter().enumerate() {
            if (bar.close >= bar.open) != rising {
                continue;
            }
            any = true;
            let x = (plot_x + (i as f64 + 0.5) * bar_w).round() + 0.5;
            cr.move_to(x, to_y(bar.high).round());
            cr.line_to(x, to_y(bar.low).round());
            if tick > 1.0 {
                let open = to_y(bar.open).round() + 0.5;
                cr.move_to(x - tick, open);
                cr.line_to(x, open);
                let close = to_y(bar.close).round() + 0.5;
                cr.move_to(x, close);
                cr.line_to(x + tick, close);
            }
        }
        if any {
            colors::set_source(cr, state.scheme.outline(direction));
            let _ = cr.stroke();
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

/// One indicator in its own strip: guides, then the line.
///
/// The strip carries its own name because the legend at the top cannot say
/// which of three stacked panes is which, and a pane you have to count down to
/// identify is a pane you misread.
#[allow(clippy::too_many_arguments)]
fn draw_pane(
    cr: &cairo::Context,
    state: &State,
    pane: &omacharts_engine::indicators::Pane,
    drawn: &Drawn,
    plot_x: f64,
    plot_w: f64,
    bar_w: f64,
    top: f64,
    height: f64,
    width: f64,
    first: usize,
    visible: usize,
) {
    let values = &pane.values[first.min(pane.values.len())..(first + visible).min(pane.values.len())];
    let (low, high) = match pane.bounds {
        Some(bounds) => bounds,
        // Fit what is on screen, with a little air: an ATR pressed against the
        // top and bottom of its strip has no shape to read.
        None => {
            let mut low = f64::MAX;
            let mut high = f64::MIN;
            for value in values.iter().flatten() {
                low = low.min(*value);
                high = high.max(*value);
            }
            if !low.is_finite() || !high.is_finite() {
                return;
            }
            if (high - low).abs() < f64::EPSILON {
                (low - 1.0, high + 1.0)
            } else {
                let air = (high - low) * 0.12;
                (low - air, high + air)
            }
        }
    };
    let to_y = |value: f64| top + height * (high - value) / (high - low);

    // The band between the guides, so overbought and oversold read as regions
    // rather than two lines you have to remember the meaning of.
    if let Some((from, to)) = pane.band {
        cr.rectangle(plot_x, to_y(to), plot_w, (to_y(from) - to_y(to)).abs());
        colors::set_source_alpha(cr, &state.theme.ui.grid, 0.35);
        let _ = cr.fill();
    }

    cr.save().ok();
    cr.set_line_width(1.0);
    colors::set_source_alpha(cr, &state.theme.ui.grid, 0.9);
    for guide in &pane.guides {
        let y = to_y(*guide).round() + 0.5;
        cr.move_to(plot_x, y);
        cr.line_to(plot_x + plot_w, y);
    }
    let _ = cr.stroke();
    cr.restore().ok();

    // The scale, written where the price axis is written. Fixed scales label
    // their guides; a fitted one labels its extremes, which is the only way to
    // know what the line is worth.
    cr.select_font_face("sans-serif", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(10.0);
    colors::set_source_alpha(cr, &state.theme.ui.text_muted, 0.75);
    let marks: Vec<(f64, String)> = if pane.bounds.is_some() {
        pane.guides.iter().map(|g| (*g, format!("{g:.0}"))).collect()
    } else {
        let decimals = decimals_for(nice_step(high - low, 3));
        vec![(low, format!("{low:.decimals$}")), (high, format!("{high:.decimals$}"))]
    };
    for (value, text) in marks {
        let y = to_y(value);
        if y < top || y > top + height {
            continue;
        }
        cr.move_to(plot_x + plot_w + 6.0, (y + 3.0).min(top + height));
        let _ = cr.show_text(&text);
    }
    let _ = width;

    // Its name, top left of its own strip.
    cr.set_font_size(10.0);
    colors::set_source_alpha(cr, &state.theme.ui.text_muted, 0.8);
    cr.move_to(plot_x + 2.0, top + 11.0);
    let _ = cr.show_text(&drawn.indicator.label_for(state.timeframe));

    // The line last, over its own furniture.
    let stroke = drawn.indicator.stroke;
    if stroke.is_hidden() {
        return;
    }
    cr.save().ok();
    cr.set_line_width(stroke.width);
    cr.set_dash(&stroke.style.dashes(stroke.width), 0.0);
    colors::set_source(cr, &drawn.color);
    let mut pen_down = false;
    for (i, value) in values.iter().enumerate() {
        let Some(value) = value else {
            pen_down = false;
            continue;
        };
        let x = plot_x + (i as f64 + 0.5) * bar_w;
        let y = to_y(*value).clamp(top, top + height);
        if pen_down {
            cr.line_to(x, y);
        } else {
            cr.move_to(x, y);
            pen_down = true;
        }
    }
    let _ = cr.stroke();
    cr.restore().ok();
}

#[allow(clippy::too_many_arguments)]
fn draw_last_price(
    cr: &cairo::Context,
    state: &State,
    plot_x: f64,
    plot_w: f64,
    width: f64,
    decimals: usize,
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

    label_on_axis(
        cr,
        state,
        &format!("{:.*}", decimals, last.close),
        plot_x + plot_w,
        y,
        width,
        colour,
    );
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
        let decimals = omacharts_engine::price_decimals(
            step,
            price,
            state.instrument.as_ref().map(|i| i.kind),
        );
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

/// Band shading and volume profiles, beneath the bars.
fn draw_indicator_fills(
    cr: &cairo::Context,
    state: &State,
    first: usize,
    visible: usize,
    plot_x: f64,
    bar_w: f64,
    to_y: &impl Fn(f64) -> f64,
) {
    for drawn in state.indicators.iter().filter(|d| d.indicator.visible) {
        match &drawn.output {
            Output::Bands(bands) => {
                // One shaded region, between the first band's own edges: the
                // area price spends most of its time in, drawn once. Shading
                // every band stacks into a wash that says less than the lines.
                for band in bands.bands.iter() {
                    if !band.enabled || !band.fill {
                        continue;
                    }
                    let colour = band_colour(band, drawn, &state.theme);
                    colors::set_source_alpha(cr, &colour, vwap::FILL_ALPHA);
                    fill_between(
                        cr, &band.upper, &band.lower, first, visible, plot_x, bar_w, to_y,
                    );
                }
            }
            Output::Profiles(profiles) => {
                draw_profiles(cr, state, drawn, profiles, first, visible, plot_x, bar_w, to_y);
            }
            // Drawn as its own pane, after the price plot.
            // Both draw in their own strip, where the price scale does not
            // reach, so nothing to do over the candles.
            Output::Volume { .. } | Output::Pane(_) | Output::Line(_) => {}
        }
    }
}

/// Indicator lines, over the bars.
fn draw_indicator_lines(
    cr: &cairo::Context,
    state: &State,
    first: usize,
    visible: usize,
    plot_x: f64,
    bar_w: f64,
    to_y: &impl Fn(f64) -> f64,
) {
    for drawn in state.indicators.iter().filter(|d| d.indicator.visible) {
        match &drawn.output {
            Output::Line(values) => {
                let stroke = drawn.indicator.stroke;
                if stroke.is_hidden() {
                    continue;
                }
                cr.save().ok();
                cr.set_dash(&stroke.style.dashes(stroke.width), 0.0);
                colors::set_source(cr, &drawn.color);
                cr.set_line_width(stroke.width);
                stroke_series(cr, values, first, visible, plot_x, bar_w, to_y);
                cr.restore().ok();
            }
            Output::Bands(bands) => {
                cr.save().ok();
                for band in bands.bands.iter() {
                    // A width of zero means no line, which is how a shaded
                    // band gets a clean edge without painting one in the
                    // background colour.
                    if !band.enabled || band.stroke.is_hidden() {
                        continue;
                    }
                    cr.set_dash(&band.stroke.style.dashes(band.stroke.width), 0.0);
                    cr.set_line_width(band.stroke.width);
                    colors::set_source(cr, &band_colour(band, drawn, &state.theme));
                    stroke_series(cr, &band.upper, first, visible, plot_x, bar_w, to_y);
                    stroke_series(cr, &band.lower, first, visible, plot_x, bar_w, to_y);
                }
                cr.restore().ok();

                let stroke = drawn.indicator.stroke;
                if !stroke.is_hidden() {
                    cr.save().ok();
                    cr.set_dash(&stroke.style.dashes(stroke.width), 0.0);
                    colors::set_source(cr, &drawn.color);
                    cr.set_line_width(stroke.width);
                    stroke_series(cr, &bands.vwap, first, visible, plot_x, bar_w, to_y);
                    cr.restore().ok();
                }
            }
            Output::Profiles(_) | Output::Volume { .. } | Output::Pane(_) => {}
        }
    }
}

/// A band's own colour, or the indicator's if it has none.
fn band_colour(band: &vwap::BandSeries, drawn: &Drawn, theme: &Theme) -> String {
    band.color
        .as_ref()
        .map(|choice| choice.resolve(theme))
        .unwrap_or_else(|| drawn.color.clone())
}

/// One histogram per period, anchored where its period begins.
#[allow(clippy::too_many_arguments)]
fn draw_profiles(
    cr: &cairo::Context,
    state: &State,
    drawn: &Drawn,
    profiles: &[Profile],
    first: usize,
    visible: usize,
    plot_x: f64,
    bar_w: f64,
    to_y: &impl Fn(f64) -> f64,
) {
    let last = first + visible;
    for profile in profiles {
        if profile.last_bar < first || profile.first_bar >= last {
            continue;
        }
        let left = plot_x + (profile.first_bar.max(first) - first) as f64 * bar_w;
        // A profile may use at most this much of its own period's width, so it
        // describes the bars rather than burying them.
        let span = ((profile.last_bar.min(last - 1) + 1 - profile.first_bar.max(first)) as f64
            * bar_w)
            .max(bar_w)
            * 0.4;

        let (area_low, area_high) = profile.value_area_bounds();
        for row in &profile.rows {
            if row.volume <= 0.0 || profile.max_volume <= 0.0 {
                continue;
            }
            let width = span * (row.volume / profile.max_volume);
            let top = to_y(row.high);
            let height = (to_y(row.low) - top).max(1.0);
            let inside = row.low >= area_low && row.high <= area_high;
            colors::set_source_alpha(cr, &drawn.color, if inside { 0.30 } else { 0.14 });
            cr.rectangle(left, top, width, height.max(1.0));
            let _ = cr.fill();
        }

        // The point of control, across the period it belongs to.
        let y = to_y(profile.poc_price()).round() + 0.5;
        colors::set_source_alpha(cr, &drawn.color, 0.85);
        cr.set_line_width(1.0);
        cr.move_to(left, y);
        cr.line_to(left + span, y);
        let _ = cr.stroke();
        let _ = state;
    }
}

/// Stroke a series, breaking the path wherever it has no value.
#[allow(clippy::too_many_arguments)]
fn stroke_series(
    cr: &cairo::Context,
    values: &[Option<f64>],
    first: usize,
    visible: usize,
    plot_x: f64,
    bar_w: f64,
    to_y: &impl Fn(f64) -> f64,
) {
    let mut drawing = false;
    for i in 0..visible {
        let x = plot_x + (i as f64 + 0.5) * bar_w;
        match values.get(first + i).copied().flatten() {
            Some(value) => {
                let y = to_y(value);
                if drawing {
                    cr.line_to(x, y);
                } else {
                    cr.move_to(x, y);
                    drawing = true;
                }
            }
            // A gap is a gap. Joining across it would draw a line through
            // prices that were never there.
            None => drawing = false,
        }
    }
    let _ = cr.stroke();
}

/// Fill the region between two series.
#[allow(clippy::too_many_arguments)]
fn fill_between(
    cr: &cairo::Context,
    upper: &[Option<f64>],
    lower: &[Option<f64>],
    first: usize,
    visible: usize,
    plot_x: f64,
    bar_w: f64,
    to_y: &impl Fn(f64) -> f64,
) {
    let mut run: Vec<(f64, f64, f64)> = Vec::new();
    let flush = |run: &mut Vec<(f64, f64, f64)>| {
        if run.len() < 2 {
            run.clear();
            return;
        }
        cr.move_to(run[0].0, run[0].1);
        for (x, y, _) in run.iter().skip(1) {
            cr.line_to(*x, *y);
        }
        for (x, _, y2) in run.iter().rev() {
            cr.line_to(*x, *y2);
        }
        cr.close_path();
        let _ = cr.fill();
        run.clear();
    };

    for i in 0..visible {
        let x = plot_x + (i as f64 + 0.5) * bar_w;
        match (
            upper.get(first + i).copied().flatten(),
            lower.get(first + i).copied().flatten(),
        ) {
            (Some(a), Some(b)) => run.push((x, to_y(a), to_y(b))),
            _ => flush(&mut run),
        }
    }
    flush(&mut run);
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

/// Label an axis tick at the detail the gap between ticks justifies.
///
/// The gap, not the whole span: twenty ticks across nine months are about a
/// fortnight apart, and labelling each of them "Mar 2026" prints the same
/// thing three times running. What decides the format is how far apart two
/// neighbouring labels are, because that is what has to tell them apart.
fn format_axis_time(ts: i64, tick_seconds: i64, intraday: bool) -> String {
    use chrono::{Local, TimeZone};
    let Some(dt) = Local.timestamp_opt(ts, 0).single() else {
        return String::new();
    };
    const DAY: i64 = 86_400;
    if intraday {
        return match tick_seconds {
            t if t >= DAY => dt.format("%d %b %H:%M").to_string(),
            t if t >= 4 * 3_600 => dt.format("%a %H:%M").to_string(),
            _ => dt.format("%H:%M").to_string(),
        };
    }
    match tick_seconds {
        t if t >= 300 * DAY => dt.format("%Y").to_string(),
        t if t >= 25 * DAY => dt.format("%b %Y").to_string(),
        _ => dt.format("%d %b").to_string(),
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

        // Years apart: years only. This is the case that read "01 Jun" for
        // every tick before the spacing was taken into account.
        let decade = format_axis_time(ts, 400 * DAY, false);
        assert_eq!(decade.len(), 4, "{decade}");
        assert!(decade.chars().all(|c| c.is_ascii_digit()), "{decade}");

        // A year or two: month and year.
        assert!(format_axis_time(ts, 30 * DAY, false).contains("20"), "months get a year");
        // A few days apart: day and month, no year.
        assert!(!format_axis_time(ts, 5 * DAY, false).contains("20"));
        // Intraday always carries a clock, however far apart the ticks.
        assert!(format_axis_time(ts, 3600, true).contains(':'));
        assert!(format_axis_time(ts, 30 * DAY, true).contains(':'));
    }

    #[test]
    fn intraday_ticks_a_few_days_apart_do_not_repeat_a_date() {
        // A week of 15-minute bars: several ticks land inside each day, so a
        // date-only label printed the same thing over and over.
        const DAY: i64 = 86_400;
        let tick = 6 * 3600;
        let labels: Vec<String> = (0..6)
            .map(|i| format_axis_time(1_700_000_000 + i * tick, tick, true))
            .collect();
        let mut unique = labels.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), labels.len(), "repeated labels: {labels:?}");
    }

    #[test]
    fn months_of_daily_bars_do_not_repeat_the_same_month() {
        // Nine months of daily bars labelled every fortnight: the case that
        // printed "Mar 2026" three times in a row.
        const DAY: i64 = 86_400;
        let tick = 14 * DAY;
        let labels: Vec<String> = (0..18)
            .map(|i| format_axis_time(1_767_225_600 + i * tick, tick, false))
            .collect();
        let mut unique = labels.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), labels.len(), "repeated labels: {labels:?}");
    }

    #[test]
    fn two_ticks_a_decade_apart_get_different_labels() {
        const DAY: i64 = 86_400;
        let tick = 400 * DAY;
        let a = format_axis_time(1_200_000_000, tick, false);
        let b = format_axis_time(1_700_000_000, tick, false);
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
