//! Link groups: which charts and watchlists follow each other's symbol.
//!
//! One group was enough while a chart could only follow the rail or not, and
//! that is what a `bool` bought. Nine groups buys the thing people actually do
//! with several charts open: a macro group tracking one symbol while a trade
//! group tracks another, and a watchlist pointed at whichever of them it is
//! driving today.
//!
//! The group is identity; the colour is only how you recognise it. That
//! matters here because the colour comes from the active theme and changes
//! when the theme does, while "these two charts are group 3" has to stay true
//! across a theme switch, a restart, and a palette the user edited at
//! midnight. So nothing stores a colour — [`LinkGroup::colour`] resolves one on
//! demand, the same bargain [`ColorChoice`](crate::theme::ColorChoice) strikes
//! for indicators.

use std::fmt;

use serde::de::{Unexpected, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::theme::{held_to, ContrastBand, Theme};

/// How many groups there are to join. Nine because that is how many fit on a
/// row of number keys, and because a tenth would be a group nobody can name.
pub const GROUP_COUNT: u8 = 9;

/// The swatch each coloured group wears, groups 2 through 9 in order.
///
/// The six of [`SWATCH_SEQUENCE`](crate::theme::SWATCH_SEQUENCE) come first,
/// so somebody picking groups in order gets the hues that were chosen to stay
/// apart from each other, and only then the two the sequence withholds. Green
/// and Rose are the candle direction colours: a chart badged in either reads,
/// for a moment, as a chart that is up or down. That is a fine thing to choose on purpose and a bad
/// thing to be handed, so they are last rather than absent.
const GROUP_SWATCHES: [&str; 8] =
    ["Blue", "Amber", "Violet", "Teal", "Orange", "Cyan", "Green", "Rose"];

/// What the first group's neutral mark is held to against the chart.
///
/// A floor and no real ceiling, unlike the grid's band. The grid needs an
/// upper limit because furniture that shouts becomes a lattice over the
/// prices; a link badge is the opposite — it is a state you asked to see, and
/// there is no such thing as reading it too easily. The floor is WCAG's
/// minimum for a graphic that carries meaning, which is what this is.
pub const LINK_CONTRAST: ContrastBand = ContrastBand::new(3.0, 21.0);

/// How much colour the first group's mark is allowed to keep.
///
/// Almost none, which is the point twice over. It has to *read* as neutral,
/// or it is a tenth colour rather than the absence of one — and a theme whose
/// muted text is a lavender grey or a warm taupe hands it a hue close enough
/// to one of the swatches to be mistaken for that group. Catppuccin, Nord and
/// Rosé Pine each did exactly that; pulling the chroma almost to zero is what
/// clears every pair in every shipped theme.
const NEUTRAL_CHROMA: f64 = 0.006;

/// Whether something follows a shared symbol, and which pool it shares it
/// with.
///
/// `None` is its own state rather than a tenth group, because "linked to
/// nobody" and "linked to group 1 alone" look identical until a second chart
/// joins, and only one of them should quietly acquire a follower when it does.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum LinkGroup {
    #[default]
    None,
    /// 1 through [`GROUP_COUNT`]. Construct through [`LinkGroup::numbered`],
    /// which is the only thing that can promise that.
    Group(u8),
}

/// Every option a picker offers, in the order it should offer them: the way
/// out first, then the groups.
pub const ALL: [LinkGroup; 1 + GROUP_COUNT as usize] = [
    LinkGroup::None,
    LinkGroup::Group(1),
    LinkGroup::Group(2),
    LinkGroup::Group(3),
    LinkGroup::Group(4),
    LinkGroup::Group(5),
    LinkGroup::Group(6),
    LinkGroup::Group(7),
    LinkGroup::Group(8),
    LinkGroup::Group(9),
];

impl LinkGroup {
    /// A group by number, or `None` for anything outside 1..=9.
    ///
    /// Lenient rather than fallible on purpose: this is reached from stored
    /// state, and a number from a version that knew more groups than this one
    /// should cost a chart its link, not cost the user their workspace.
    pub fn numbered(n: u8) -> LinkGroup {
        match n {
            1..=GROUP_COUNT => LinkGroup::Group(n),
            _ => LinkGroup::None,
        }
    }

    pub fn number(self) -> Option<u8> {
        match self {
            LinkGroup::None => None,
            LinkGroup::Group(n) => Some(n),
        }
    }

    pub fn is_linked(self) -> bool {
        matches!(self, LinkGroup::Group(_))
    }

    /// The palette name this group wears, if it wears one. The first group
    /// does not: it is the neutral one, and neutral is not in the palette.
    pub fn swatch(self) -> Option<&'static str> {
        match self {
            LinkGroup::Group(n) if n >= 2 => GROUP_SWATCHES.get(usize::from(n) - 2).copied(),
            _ => None,
        }
    }

    /// The colour to badge this group with, against `theme`.
    ///
    /// `None` when there is nothing to badge. The first group resolves to the
    /// theme's own muted text, held legible against the chart — which is what
    /// makes it read as "on, but not claiming a colour", and keeps a chart
    /// that was simply linked before looking exactly as it did.
    ///
    /// A theme missing a swatch falls back to its accent, the same way a
    /// [`ColorChoice`](crate::theme::ColorChoice) does, because a group with
    /// no colour at all would be a group you cannot see you are in.
    pub fn colour(self, theme: &Theme) -> Option<String> {
        match self {
            LinkGroup::None => None,
            LinkGroup::Group(1) => Some(held_to(
                &theme.ui.text_muted,
                &theme.ui.background,
                LINK_CONTRAST,
                Some(NEUTRAL_CHROMA),
            )),
            LinkGroup::Group(_) => Some(
                self.swatch()
                    .and_then(|name| theme.swatch(name))
                    .map(|swatch| swatch.hex.clone())
                    .unwrap_or_else(|| theme.ui.accent.clone()),
            ),
        }
    }

    /// What a menu row calls this.
    ///
    /// Numbered rather than named by colour, because the number is the part
    /// that is true tomorrow. The colour is the theme's answer today and a
    /// different one under the next theme, so a popover offering "Amber"
    /// would be describing the swatch rather than the group — and two people
    /// comparing screens could be in the same group under different names.
    pub fn label(self) -> &'static str {
        match self {
            LinkGroup::None => "Not linked",
            LinkGroup::Group(n) => LABELS.get(usize::from(n) - 1).copied().unwrap_or("Not linked"),
        }
    }
}

const LABELS: [&str; GROUP_COUNT as usize] = [
    "Group 1", "Group 2", "Group 3", "Group 4", "Group 5", "Group 6", "Group 7", "Group 8",
    "Group 9",
];

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

/// Written as a number, with 0 for unlinked.
impl Serialize for LinkGroup {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.number().unwrap_or(0))
    }
}

/// Read from a number — or from the boolean a chart used to store.
///
/// Every workspace written before groups existed says `"linked": true`, and
/// there is no migration step anywhere that could rewrite them: the file is
/// the user's live layout, read once at startup. So the old spelling is simply
/// still a spelling this understands, and `true` means the first group, which
/// is the one that looks and behaves like the only link there used to be.
impl<'de> Deserialize<'de> for LinkGroup {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<LinkGroup, D::Error> {
        deserializer.deserialize_any(LinkGroupVisitor)
    }
}

struct LinkGroupVisitor;

impl Visitor<'_> for LinkGroupVisitor {
    type Value = LinkGroup;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a link group from 0 to 9, or the boolean a single link was stored as")
    }

    fn visit_bool<E: serde::de::Error>(self, linked: bool) -> Result<LinkGroup, E> {
        Ok(if linked { LinkGroup::Group(1) } else { LinkGroup::None })
    }

    fn visit_u64<E: serde::de::Error>(self, n: u64) -> Result<LinkGroup, E> {
        Ok(LinkGroup::numbered(u8::try_from(n).unwrap_or(0)))
    }

    fn visit_i64<E: serde::de::Error>(self, n: i64) -> Result<LinkGroup, E> {
        Ok(LinkGroup::numbered(u8::try_from(n).unwrap_or(0)))
    }

    fn visit_f64<E: serde::de::Error>(self, n: f64) -> Result<LinkGroup, E> {
        // JSON has one number type, and a round-tripped 3 can arrive as 3.0.
        if n.fract() != 0.0 {
            return Err(E::invalid_value(Unexpected::Float(n), &self));
        }
        Ok(LinkGroup::numbered(n as u8))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{builtin_themes, delta_e, SWATCH_NAMES, SWATCH_SEQUENCE};

    #[test]
    fn a_chart_linked_before_groups_existed_joins_the_first_one() {
        // The old spelling, straight out of a workspace nobody will rewrite.
        assert_eq!(
            serde_json::from_str::<LinkGroup>("true").unwrap(),
            LinkGroup::Group(1)
        );
        assert_eq!(serde_json::from_str::<LinkGroup>("false").unwrap(), LinkGroup::None);
    }

    #[test]
    fn every_group_survives_being_written_down() {
        for group in ALL {
            let json = serde_json::to_string(&group).unwrap();
            assert_eq!(serde_json::from_str::<LinkGroup>(&json).unwrap(), group, "{json}");
        }
    }

    #[test]
    fn a_group_this_version_does_not_have_costs_a_link_and_nothing_else() {
        // A workspace from a version with more groups, or a hand-edited one.
        // Losing the link is recoverable in one click; refusing to parse would
        // take the whole layout down with it.
        assert_eq!(serde_json::from_str::<LinkGroup>("42").unwrap(), LinkGroup::None);
        assert_eq!(serde_json::from_str::<LinkGroup>("-1").unwrap(), LinkGroup::None);
        assert_eq!(serde_json::from_str::<LinkGroup>("0").unwrap(), LinkGroup::None);
    }

    #[test]
    fn the_unambiguous_hues_are_offered_before_the_candle_colours() {
        for (n, name) in SWATCH_SEQUENCE.iter().enumerate() {
            assert_eq!(GROUP_SWATCHES[n], *name, "the sequence should come first");
        }
        let tail = &GROUP_SWATCHES[SWATCH_SEQUENCE.len()..];
        assert_eq!(tail, ["Green", "Rose"], "the direction colours belong last");

        // And between them they are the whole palette, used once each.
        let mut seen = GROUP_SWATCHES.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), GROUP_SWATCHES.len(), "a colour is handed out twice");
        for name in GROUP_SWATCHES {
            assert!(SWATCH_NAMES.contains(&name), "{name} is not a palette colour");
        }
    }

    #[test]
    fn an_unlinked_thing_has_no_colour_to_draw() {
        assert_eq!(LinkGroup::None.colour(&builtin_themes()[0]), None);
        assert_eq!(LinkGroup::None.swatch(), None);
        assert!(!LinkGroup::None.is_linked());
    }

    /// Two groups the eye reads as one colour is two groups nobody can tell
    /// apart on screen, which is the whole affordance gone. 0.06 is where
    /// `delta_e` puts two lines that read as the same line.
    ///
    /// Two badges the eye cannot separate are two badges that say nothing, so
    /// this holds for every built-in palette with no exceptions. It carried
    /// three for a while — Teal against Cyan in Paper and Daylight, where the
    /// swatches were always near-twins — until those palettes were retuned.
    #[test]
    fn no_two_groups_ever_look_alike() {
        let mut tight = Vec::new();
        for theme in builtin_themes() {
            let colours: Vec<(LinkGroup, String)> = ALL
                .iter()
                .filter_map(|g| g.colour(&theme).map(|hex| (*g, hex)))
                .collect();
            for (i, (a, a_hex)) in colours.iter().enumerate() {
                for (b, b_hex) in &colours[i + 1..] {
                    if delta_e(a_hex, b_hex) > 0.06 {
                        continue;
                    }
                    let name = |g: &LinkGroup| g.swatch().unwrap_or("Neutral");
                    tight.push(format!("{}: {}/{}", theme.name, name(a), name(b)));
                }
            }
        }
        tight.sort();
        assert!(tight.is_empty(), "these groups read as one colour: {tight:?}");
    }

    /// The neutral group has to stay clear of all eight hues, and a theme
    /// whose muted text is a lavender grey or a warm taupe will hand it one
    /// close enough to be mistaken for that group if nothing stops it.
    #[test]
    fn the_neutral_group_is_never_mistaken_for_a_coloured_one() {
        for theme in builtin_themes() {
            let neutral = LinkGroup::Group(1).colour(&theme).expect("a colour");
            for group in ALL.iter().filter(|g| g.swatch().is_some()) {
                let coloured = group.colour(&theme).expect("a colour");
                assert!(
                    delta_e(&neutral, &coloured) > 0.06,
                    "{} reads as {} in {}: {neutral} vs {coloured}",
                    group.label(),
                    group.swatch().unwrap(),
                    theme.name
                );
            }
        }
    }

    #[test]
    fn a_theme_missing_a_swatch_still_shows_you_which_group_you_are_in() {
        let mut theme = builtin_themes()[0].clone();
        theme.swatches.retain(|s| s.name != "Violet");
        assert_eq!(LinkGroup::Group(4).colour(&theme), Some(theme.ui.accent.clone()));
    }

    #[test]
    fn groups_are_named_by_number_because_the_colour_is_only_todays_answer() {
        assert_eq!(LinkGroup::None.label(), "Not linked");
        assert_eq!(LinkGroup::Group(1).label(), "Group 1");
        assert_eq!(LinkGroup::Group(9).label(), "Group 9");
    }

    #[test]
    fn the_picker_offers_the_way_out_first_and_then_every_group() {
        assert_eq!(ALL[0], LinkGroup::None);
        assert_eq!(ALL.len(), 1 + usize::from(GROUP_COUNT));
        for (n, group) in ALL[1..].iter().enumerate() {
            assert_eq!(group.number(), Some(n as u8 + 1));
        }
    }
}

