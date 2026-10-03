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
    /// Symbol and resolution, over the top left of the chart.
    pub readout: gtk::Label,
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

        let readout = gtk::Label::new(None);
        readout.add_css_class("readout-symbol");
        readout.set_valign(gtk::Align::Center);
        readout.set_can_target(false);

        let link = gtk::ToggleButton::new();
        link.add_css_class("flat");
        link.add_css_class("legend-gear");
        link.set_valign(gtk::Align::Center);
        link.set_active(linked);
        set_link_look(&link, linked);

        let gear = gtk::Button::from_icon_name("emblem-system-symbolic");
        gear.add_css_class("flat");
        gear.add_css_class("legend-gear");
        gear.set_tooltip_text(Some("Chart settings"));
        gear.set_valign(gtk::Align::Center);

        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.append(&readout);
        bar.append(&link);
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

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&view.area));
        overlay.add_overlay(&legend);

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
            readout,
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
    pub fn set_focused(&self, focused: bool) {
        if focused {
            self.root.add_css_class("focused");
        } else {
            self.root.remove_css_class("focused");
        }
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
        self.readout.set_text(&self.label());
    }
}

fn set_link_look(link: &gtk::ToggleButton, linked: bool) {
    link.set_icon_name(if linked {
        "insert-link-symbolic"
    } else {
        "link-broken-symbolic"
    });
    link.set_tooltip_text(Some(if linked { LINK_ON } else { LINK_OFF }));
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
    Split { horizontal: bool, first: Box<Node>, second: Box<Node> },
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
                first: Box::new(Node::Leaf(id)),
                second: Box::new(Node::Leaf(added)),
            },
            Node::Leaf(leaf) => Node::Leaf(*leaf),
            Node::Split { horizontal: h, first, second } => Node::Split {
                horizontal: *h,
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
            Node::Split { horizontal, first, second } => {
                match (first.remove(id), second.remove(id)) {
                    (Some(a), Some(b)) => Some(Node::Split {
                        horizontal: *horizontal,
                        first: Box::new(a),
                        second: Box::new(b),
                    }),
                    (Some(only), None) | (None, Some(only)) => Some(only),
                    (None, None) => None,
                }
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

    #[test]
    fn leaves_come_back_in_layout_order() {
        let layout = Node::leaf(1).split(1, 2, true).split(1, 3, false).split(2, 4, false);
        assert_eq!(layout.leaves(), vec![1, 3, 2, 4]);
    }
}
