//! One chart in the layout.
//!
//! The window used to be a chart with a legend painted over it. With several
//! charts side by side, everything that was "the chart's" moves in here: which
//! symbol it shows, at what resolution, with which indicators, in which
//! session, drawn how. A pane is the unit you focus, split, and close.
//!
//! What stays with the window is what is genuinely one per window — the
//! watchlist, the theme, the symbol index — and the question of which pane is
//! focused.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use omacharts_engine::{BarStyle, Indicator, Instrument, Session, Theme, Timeframe};

use crate::ui::chart::ChartView;

/// How a pane follows the watchlist.
///
/// Linked panes share one symbol: the watchlist drives them, and a symbol
/// picked on any of them moves the rest. Unlinked panes are parked — the
/// reason to split a chart in the first place is usually to leave one of them
/// showing something while you go looking at something else.
pub const LINK_ON: &str = "Linked to the watchlist";
pub const LINK_OFF: &str = "Not linked — this chart stays where it is";

pub struct ChartPane {
    pub id: u32,
    pub view: Rc<ChartView>,
    /// What goes in the layout: the chart, its legend, and the focus ring.
    pub root: gtk::Box,
    /// The symbol, over the top left of the chart. A button: clicking the
    /// name of the thing you are looking at to change it is the shortest
    /// route there is.
    pub symbol_button: gtk::Button,
    /// This chart's resolutions, since the resolution is this chart's. One
    /// strip in the header could only ever describe one of them.
    ///
    /// Shown on the focused chart only: four strips on screen is the same row
    /// of buttons four times, and only one of them is the one you are about to
    /// press. The others say their resolution in a word instead.
    pub strip: gtk::Box,
    pub buttons: RefCell<Vec<(Timeframe, gtk::ToggleButton)>>,
    pub timeframe_label: gtk::Label,
    /// One row per indicator, under the readout.
    pub indicator_legend: gtk::Box,
    pub gear: gtk::Button,
    pub link: gtk::ToggleButton,
    pub instrument: RefCell<Option<Instrument>>,
    pub timeframe: Cell<Timeframe>,
    pub indicators: RefCell<Vec<Indicator>>,
    pub bar_style: Cell<BarStyle>,
    pub session: Cell<Session>,
    pub show_grid: Cell<bool>,
    pub linked: Cell<bool>,
}

impl ChartPane {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: u32,
        theme: Theme,
        scheme: omacharts_engine::theme::BarScheme,
        timeframe: Timeframe,
        indicators: Vec<Indicator>,
        bar_style: BarStyle,
        session: Session,
        show_grid: bool,
        linked: bool,
    ) -> Rc<ChartPane> {
        let view = ChartView::new(theme, scheme);

        let symbol_button = gtk::Button::new();
        symbol_button.add_css_class("flat");
        symbol_button.add_css_class("legend-symbol");
        // The same weight the plain label had: it is still the title of the
        // chart, it just happens to be clickable.
        symbol_button.add_css_class("readout-symbol");
        symbol_button.set_valign(gtk::Align::Center);
        symbol_button.set_tooltip_text(Some("Find a symbol (Ctrl+K)"));

        let strip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        strip.add_css_class("linked");
        strip.add_css_class("timeframe-strip");
        strip.set_valign(gtk::Align::Center);
        strip.set_visible(false);

        let timeframe_label = gtk::Label::new(None);
        timeframe_label.add_css_class("readout-symbol");
        timeframe_label.set_valign(gtk::Align::Center);
        timeframe_label.set_can_target(false);

        // Drawn rather than named. Adwaita's "insert-link" is a chain with a
        // downward arrow under it — it means *insert* a link, and the arrow
        // read as a dropdown nobody could open. There is no plain chain in the
        // theme, so here is one: two capsules and the bar that joins them,
        // painted in whatever colour the button currently has, which is what
        // makes it follow the theme and dim with the rest of the legend.
        let link = gtk::ToggleButton::new();
        link.set_child(Some(&chain_icon()));
        link.add_css_class("flat");
        link.add_css_class("legend-link");
        link.set_valign(gtk::Align::Center);
        link.set_active(linked);
        set_link_look(&link, linked);

        let gear = gtk::Button::from_icon_name("emblem-system-symbolic");
        gear.add_css_class("flat");
        gear.add_css_class("legend-gear");
        gear.set_tooltip_text(Some("Chart settings"));
        gear.set_valign(gtk::Align::Center);

        // The link sits with the symbol, because that is what it is about:
        // whether this chart follows the rail's symbol or keeps its own.
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        bar.append(&symbol_button);
        bar.append(&link);
        bar.append(&timeframe_label);
        bar.append(&gear);

        let indicator_legend = gtk::Box::new(gtk::Orientation::Vertical, 0);
        indicator_legend.set_halign(gtk::Align::Start);

        let legend = gtk::Box::new(gtk::Orientation::Vertical, 0);
        legend.set_halign(gtk::Align::Start);
        legend.set_valign(gtk::Align::Start);
        legend.set_margin_start(12);
        legend.set_margin_top(6);
        legend.append(&bar);
        legend.append(&indicator_legend);

        // The strip sits across the top of its own chart rather than in the
        // legend: centred it reads as this chart's own toolbar, where in the
        // corner it was one more thing in a stack of names.
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        top.set_halign(gtk::Align::Center);
        top.set_valign(gtk::Align::Start);
        top.set_margin_top(6);
        top.append(&strip);

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&view.area));
        overlay.add_overlay(&legend);
        overlay.add_overlay(&top);

        // A box rather than the overlay itself, so the focus ring is drawn on
        // something that is not also the drawing surface.
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("chart-pane");
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.append(&overlay);

        Rc::new(ChartPane {
            id,
            view,
            root,
            symbol_button,
            strip,
            buttons: RefCell::new(Vec::new()),
            timeframe_label,
            indicator_legend,
            gear,
            link,
            instrument: RefCell::new(None),
            timeframe: Cell::new(timeframe),
            indicators: RefCell::new(indicators),
            bar_style: Cell::new(bar_style),
            session: Cell::new(session),
            show_grid: Cell::new(show_grid),
            linked: Cell::new(linked),
        })
    }

    /// Mark this one as the pane that keys and menus act on.
    ///
    /// A ring rather than anything louder: with four charts on screen the
    /// focused one has to be obvious at a glance and invisible the moment you
    /// stop looking for it.
    /// Point this chart's strip at the resolution it is showing.
    pub fn sync_strip(&self) {
        let current = self.timeframe.get();
        for (listed, button) in self.buttons.borrow().iter() {
            button.set_active(*listed == current);
        }
    }

    pub fn set_focused(&self, focused: bool) {
        if focused {
            self.root.add_css_class("focused");
        } else {
            self.root.remove_css_class("focused");
        }
        self.strip.set_visible(focused);
        self.timeframe_label.set_visible(!focused);
    }

    pub fn set_linked(&self, linked: bool) {
        self.linked.set(linked);
        if self.link.is_active() != linked {
            self.link.set_active(linked);
        }
        set_link_look(&self.link, linked);
    }

    pub fn label(&self) -> String {
        match self.instrument.borrow().as_ref() {
            Some(instrument) => format!(
                "{}  ·  {}",
                instrument.display_symbol(),
                self.timeframe.get().label()
            ),
            None => String::new(),
        }
    }

    pub fn write_readout(&self) {
        let symbol = self
            .instrument
            .borrow()
            .as_ref()
            .map(|i| i.display_symbol())
            .unwrap_or_default();
        self.symbol_button.set_label(&symbol);
        self.timeframe_label.set_text(&format!("·  {}", self.timeframe.get().label()));
    }
}

/// A chain: two rounded capsules side by side, joined across the middle.
fn chain_icon() -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_content_width(16);
    area.set_content_height(16);
    area.set_draw_func(|area, cr, width, height| {
        let colour = area.color();
        cr.set_source_rgba(
            colour.red() as f64,
            colour.green() as f64,
            colour.blue() as f64,
            colour.alpha() as f64,
        );
        let (w, h) = (width as f64, height as f64);
        let mid = h / 2.0;
        let thickness = (h / 5.5).max(1.5);
        cr.set_line_width(thickness);
        cr.set_line_cap(gtk::cairo::LineCap::Round);

        // Two links, each a short stroke, with the gap between them bridged by
        // a thinner bar — the shape you read as a chain at sixteen pixels.
        let inset = w * 0.12;
        let gap = w * 0.09;
        cr.move_to(inset, mid);
        cr.line_to(w / 2.0 - gap, mid);
        cr.move_to(w / 2.0 + gap, mid);
        cr.line_to(w - inset, mid);
        let _ = cr.stroke();

        cr.set_line_width(thickness * 0.62);
        cr.move_to(w / 2.0 - gap * 1.6, mid);
        cr.line_to(w / 2.0 + gap * 1.6, mid);
        let _ = cr.stroke();
    });
    area
}

/// The legend stays out of the way, so an unlinked chart's toggle is nearly
/// invisible. A linked one is not: following the rail is the state worth being
/// able to see across four charts without looking for it.
fn set_link_look(link: &gtk::ToggleButton, linked: bool) {
    link.set_tooltip_text(Some(if linked { LINK_ON } else { LINK_OFF }));
    // Two states that cannot be confused, from one icon: Adwaita has a chain
    // and no broken chain, so the difference has to be carried by weight
    // rather than by a second glyph. A linked chart says so plainly; an
    // unlinked one keeps a faint handle you can find when you want it.
    link.set_opacity(if linked { 1.0 } else { 0.28 });
}

/// How the panes are arranged.
///
/// A binary tree, the way a tiling window manager keeps one: splitting divides
/// the focused pane in two, and closing a pane hands its space back to its
/// sibling by collapsing the split above them. Nothing else moves.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Node {
    Leaf(u32),
    Split {
        horizontal: bool,
        /// Where the divider sits, as a share of the split's length. Stored
        /// with the shape because it is part of the arrangement: a layout that
        /// comes back with every divider centred is not the layout you left.
        #[serde(default = "half")]
        ratio: f64,
        first: Box<Node>,
        second: Box<Node>,
    },
}

fn half() -> f64 {
    0.5
}

impl Node {
    pub fn leaf(id: u32) -> Node {
        Node::Leaf(id)
    }

    /// Replace the leaf `id` with a split holding it and `added`.
    pub fn split(&self, id: u32, added: u32, horizontal: bool) -> Node {
        match self {
            Node::Leaf(leaf) if *leaf == id => Node::Split {
                horizontal,
                ratio: 0.5,
                first: Box::new(Node::Leaf(id)),
                second: Box::new(Node::Leaf(added)),
            },
            Node::Leaf(leaf) => Node::Leaf(*leaf),
            Node::Split { horizontal: h, ratio, first, second } => Node::Split {
                horizontal: *h,
                ratio: *ratio,
                first: Box::new(first.split(id, added, horizontal)),
                second: Box::new(second.split(id, added, horizontal)),
            },
        }
    }

    /// Take `id` out, collapsing the split that held it into its sibling.
    /// `None` when `id` was the only leaf, which the caller refuses to do.
    pub fn remove(&self, id: u32) -> Option<Node> {
        match self {
            Node::Leaf(leaf) => (*leaf != id).then_some(Node::Leaf(*leaf)),
            Node::Split { horizontal, ratio, first, second } => {
                match (first.remove(id), second.remove(id)) {
                    (Some(a), Some(b)) => Some(Node::Split {
                        horizontal: *horizontal,
                        ratio: *ratio,
                        first: Box::new(a),
                        second: Box::new(b),
                    }),
                    (Some(only), None) | (None, Some(only)) => Some(only),
                    (None, None) => None,
                }
            }
        }
    }

    /// Move the divider of the split at `path`, where each step says which
    /// half to descend into.
    pub fn with_ratio(&self, path: &[bool], ratio: f64) -> Node {
        match self {
            Node::Leaf(id) => Node::Leaf(*id),
            Node::Split { horizontal, ratio: current, first, second } => {
                let (ratio, first, second) = match path.split_first() {
                    None => (ratio.clamp(0.05, 0.95), first.clone(), second.clone()),
                    Some((true, rest)) => (
                        *current,
                        Box::new(first.with_ratio(rest, ratio)),
                        second.clone(),
                    ),
                    Some((false, rest)) => (
                        *current,
                        first.clone(),
                        Box::new(second.with_ratio(rest, ratio)),
                    ),
                };
                Node::Split { horizontal: *horizontal, ratio, first, second }
            }
        }
    }

    /// Every pane, left to right and top to bottom — which is the order the
    /// keyboard walks them in.
    pub fn leaves(&self) -> Vec<u32> {
        match self {
            Node::Leaf(id) => vec![*id],
            Node::Split { first, second, .. } => {
                let mut out = first.leaves();
                out.extend(second.leaves());
                out
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitting_replaces_the_leaf_with_a_pair() {
        let layout = Node::leaf(1).split(1, 2, true);
        assert_eq!(layout.leaves(), vec![1, 2]);
        assert!(matches!(layout, Node::Split { horizontal: true, .. }));
    }

    #[test]
    fn splitting_only_touches_the_pane_asked_for() {
        let layout = Node::leaf(1).split(1, 2, true).split(2, 3, false);
        assert_eq!(layout.leaves(), vec![1, 2, 3]);
    }

    #[test]
    fn removing_gives_the_space_to_the_sibling() {
        // Two panes side by side: closing one leaves the other alone at the
        // top of the tree, holding everything.
        let layout = Node::leaf(1).split(1, 2, true);
        assert_eq!(layout.remove(2), Some(Node::Leaf(1)));
        assert_eq!(layout.remove(1), Some(Node::Leaf(2)));
    }

    #[test]
    fn removing_collapses_only_the_split_that_held_it() {
        let layout = Node::leaf(1).split(1, 2, true).split(2, 3, false);
        let left = layout.remove(3).unwrap();
        assert_eq!(left.leaves(), vec![1, 2]);
        assert!(matches!(left, Node::Split { horizontal: true, .. }));
    }

    #[test]
    fn the_last_pane_cannot_be_removed() {
        assert_eq!(Node::leaf(1).remove(1), None);
    }

    /// Four panes from two splits of a split, the way tmux does it: every
    /// leaf is splittable, including ones that came from a split.
    #[test]
    fn splitting_a_split_pane_gives_four() {
        let layout = Node::leaf(1)
            .split(1, 2, true)
            .split(1, 3, false)
            .split(2, 4, false);
        assert_eq!(layout.leaves().len(), 4);
        // And the shape is a pair of columns, each divided in two.
        let Node::Split { horizontal, first, second, .. } = &layout else { panic!("{layout:?}") };
        assert!(*horizontal);
        assert!(matches!(**first, Node::Split { horizontal: false, .. }));
        assert!(matches!(**second, Node::Split { horizontal: false, .. }));
    }

    /// Closing one of four leaves three, and only the split that held it
    /// collapses — the other column keeps its own division.
    #[test]
    fn closing_one_of_four_leaves_the_rest_alone() {
        let layout = Node::leaf(1).split(1, 2, true).split(1, 3, false).split(2, 4, false);
        let left = layout.remove(3).unwrap();
        assert_eq!(left.leaves(), vec![1, 2, 4]);
        let Node::Split { first, second, .. } = &left else { panic!("{left:?}") };
        assert_eq!(**first, Node::Leaf(1));
        assert!(matches!(**second, Node::Split { .. }));
    }

    /// A dragged divider is part of the arrangement, and has to survive both
    /// a restart and anything else happening to the tree.
    #[test]
    fn a_dragged_divider_is_remembered() {
        let layout = Node::leaf(1).split(1, 2, true).with_ratio(&[], 0.7);
        let Node::Split { ratio, .. } = &layout else { panic!("{layout:?}") };
        assert!((ratio - 0.7).abs() < 1e-9, "{ratio}");

        // Splitting one half leaves the outer divider where it was.
        let deeper = layout.split(2, 3, false);
        let Node::Split { ratio, .. } = &deeper else { panic!("{deeper:?}") };
        assert!((ratio - 0.7).abs() < 1e-9, "outer divider moved: {ratio}");

        // And so does closing a pane in the other half.
        let closed = deeper.remove(3).unwrap();
        let Node::Split { ratio, .. } = &closed else { panic!("{closed:?}") };
        assert!((ratio - 0.7).abs() < 1e-9, "outer divider moved on close: {ratio}");
    }

    #[test]
    fn an_inner_divider_moves_without_disturbing_the_outer_one() {
        let layout = Node::leaf(1)
            .split(1, 2, true)
            .with_ratio(&[], 0.3)
            .split(2, 3, false)
            .with_ratio(&[false], 0.8);
        let Node::Split { ratio, second, .. } = &layout else { panic!() };
        assert!((ratio - 0.3).abs() < 1e-9, "outer: {ratio}");
        let Node::Split { ratio, .. } = &**second else { panic!() };
        assert!((ratio - 0.8).abs() < 1e-9, "inner: {ratio}");
    }

    #[test]
    fn a_divider_cannot_be_pushed_off_the_end() {
        let layout = Node::leaf(1).split(1, 2, true).with_ratio(&[], 9.0);
        let Node::Split { ratio, .. } = &layout else { panic!() };
        assert!(*ratio <= 0.95 && *ratio >= 0.05, "{ratio}");
    }

    #[test]
    fn leaves_come_back_in_layout_order() {
        let layout = Node::leaf(1).split(1, 2, true).split(1, 3, false).split(2, 4, false);
        assert_eq!(layout.leaves(), vec![1, 3, 2, 4]);
    }
}
