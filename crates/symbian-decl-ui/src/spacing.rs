//! Distances named by their role, resolved against the theme.
//!
//! This is [`Ink`](crate::widgets::Ink) for space, and it exists for the identical reason. A `view`
//! is built without a theme — deliberately, so that a screen is a description that can be
//! constructed, compared and tested without a platform in hand — which means a colour cannot be
//! written there as a `Color` and a distance cannot be written there as a pixel count without
//! quietly deciding that the theme has no say.
//!
//! Before this, it did not have one. `symbian_ui::tokens::Space` lives on `theme.metrics.space`, so
//! an imperative screen writes `theme.metrics.space.snug` and a declarative one wrote `4` — the same
//! distance, one of them a name and the other a number that agrees with it until someone changes the
//! theme. The gap between the two was recorded rather than closed for exactly as long as it took the
//! component library to need thirty screens of it.
//!
//! ```ignore
//! Column::new().gap(Gap::Snug).pad(Gap::Base)      // named
//! Column::new().gap(4).pad(6)                      // still works — `i32` is `Gap::Exact`
//! ```
//!
//! # Where it is resolved
//!
//! In [`crate::layout`], on both passes. `measure_group` spends padding and gaps before dividing a
//! line, and `layout_group` spends them again when it hands out rects — so the placement pass, which
//! used to take no theme, now takes one.
//!
//! That is a change to a signature whose doc comment said it took no theme on purpose, and the
//! reason it was right to change is worth stating: the rule that pass obeys is that it *cannot
//! measure*, not that it cannot see a palette. Turning `Gap::Snug` into `4` is a lookup, not a
//! measurement — it consults no font and no string, and two calls with the same theme give the same
//! answer. What it still cannot do is ask a widget how big it is.
//!
//! # The known limit
//!
//! [`UiCache`](crate::UiCache) is keyed by content digest and offer, not by theme. A digest folds in
//! *which role* a gap is and not the pixels it resolves to, so a theme that changed `Space` between
//! two frames would leave every group holding a size measured against the old spacing.
//!
//! That is unreachable today — `Metrics::default()` is the only construction of metrics in the tree,
//! so no theme varies `Space` — and it is the same limit `Text` already lives with, since font
//! metrics come from the theme and are not in any digest either. When a second metrics set appears,
//! the answer is a theme generation in the cache key, and this paragraph is the note that says so.

use symbian_gfx::Edges;
use symbian_ui::Theme;

use crate::widget::{hash_i32, WidgetHash};

/// One distance, named by what it separates.
///
/// The five roles are [`symbian_ui::tokens::Space`]'s, unchanged: the point is to reach the same
/// numbers the imperative toolkit reaches, not to invent a second scale that nearly agrees.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Gap {
    /// Nothing at all. The default, because a container that has not been told a distance should not
    /// invent one.
    #[default]
    None,
    /// A separator's own width. Always 1 — named so a call site reads as intent rather than as a
    /// magic number that happens to be small.
    Hair,
    /// Between a glyph and the thing it labels.
    Tight,
    /// Between stacked lines of text.
    Snug,
    /// The default gap, and the side margin of a list row.
    Base,
    /// Between groups: around a bubble, or a screen's outer margin.
    Wide,
    /// A distance the caller insists on.
    ///
    /// Not deprecated and not a smell on its own — a badge two pixels taller than its line is a real
    /// measurement of a real design, not a role. But a `Gap::Exact` that appears three times with the
    /// same number is a role the scale is missing.
    Exact(i32),
}

impl Gap {
    /// The distance in pixels.
    pub fn resolve(self, theme: &Theme<'_>) -> i32 {
        let s = theme.metrics.space;
        match self {
            Gap::None => 0,
            Gap::Hair => s.hair,
            Gap::Tight => s.tight,
            Gap::Snug => s.snug,
            Gap::Base => s.base,
            Gap::Wide => s.wide,
            // Clamped here rather than at the builder, so every path into a `Gap` gets the rule and
            // a `Gap` constructed directly in a struct literal cannot smuggle a negative through.
            Gap::Exact(px) => px,
        }
        .max(0)
    }

    /// Fold this gap into a digest.
    ///
    /// **The role, not the pixels.** Two roles that resolve to the same number under today's theme
    /// are still different declarations, and hashing the resolved value would need a theme — which
    /// `content_hash` does not have and should not, since a digest is about the description.
    pub fn hash(self, seed: WidgetHash) -> WidgetHash {
        let (tag, payload) = match self {
            Gap::None => (0, 0),
            Gap::Hair => (1, 0),
            Gap::Tight => (2, 0),
            Gap::Snug => (3, 0),
            Gap::Base => (4, 0),
            Gap::Wide => (5, 0),
            Gap::Exact(px) => (6, px),
        };
        hash_i32(hash_i32(seed, tag), payload)
    }
}

impl From<i32> for Gap {
    /// So `.gap(4)` keeps working, and every screen written before the roles existed still compiles.
    fn from(px: i32) -> Self {
        Gap::Exact(px)
    }
}

/// How tall a row is, named by what kind of row it is.
///
/// [`Gap`] for the vertical extent of a list row, and it exists for the same reason and one more.
/// The reason it shares: `theme.metrics.row_h` is 38 and a `view` has no theme, so before this every
/// declarative list wrote `38` — a number that agrees with the imperative screens until a theme
/// moves. The extra reason is that once heights are *roles*, a list can hold a mixture of them
/// without the caller resolving anything: a heading and a row are different kinds, not different
/// numbers.
///
/// It does not replace an explicit height. A transcript's rows are as tall as their wrapped text,
/// which only the caller can measure — that is what [`RowHeight::Exact`] is, and it is a first-class
/// answer here rather than an escape hatch.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum RowHeight {
    /// `metrics.row_h` — an ordinary list row. The default.
    #[default]
    Row,
    /// A section heading: shorter than a row, so it does not read as one of them. Matches
    /// `SectionHeader`'s own published height.
    Header,
    /// A height the caller measured. What a wrapped message bubble is.
    Exact(i32),
}

impl RowHeight {
    pub fn resolve(self, theme: &Theme<'_>) -> i32 {
        match self {
            RowHeight::Row => theme.metrics.row_h,
            // The same expression `SectionHeader::height` returns, and the two must not drift: a
            // list told a heading is one height while the heading measures another scrolls a
            // fraction short of its last row for ever. `the_header_role_is_the_headers_own_height`
            // in `section_header.rs` is the assertion that pins them together.
            RowHeight::Header => theme.fonts.small.line_height() + theme.metrics.space.snug,
            RowHeight::Exact(px) => px,
        }
        .max(0)
    }

    /// Fold the *role* into a digest, never the pixels. See [`Gap::hash`].
    pub fn hash(self, seed: WidgetHash) -> WidgetHash {
        let (tag, px) = match self {
            RowHeight::Row => (0, 0),
            RowHeight::Header => (1, 0),
            RowHeight::Exact(px) => (2, px),
        };
        hash_i32(hash_i32(seed, tag), px)
    }
}

impl From<i32> for RowHeight {
    /// So `ScrollList::new(slots, n, 38)` keeps working, and every list written before the roles
    /// existed still compiles.
    fn from(px: i32) -> Self {
        RowHeight::Exact(px)
    }
}

/// Four distances, one per side.
///
/// [`Edges`] with roles instead of pixels. It converts from `Edges`, so a caller holding a measured
/// rect's insets can still hand them over without spelling out four `Gap::Exact`s.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Pad {
    pub left: Gap,
    pub top: Gap,
    pub right: Gap,
    pub bottom: Gap,
}

impl Pad {
    pub const ZERO: Self =
        Self { left: Gap::None, top: Gap::None, right: Gap::None, bottom: Gap::None };

    /// The same distance on every side.
    pub fn all(g: impl Into<Gap>) -> Self {
        let g = g.into();
        Self { left: g, top: g, right: g, bottom: g }
    }

    /// One distance across, another down — the shape a row of text in a band actually wants.
    pub fn xy(x: impl Into<Gap>, y: impl Into<Gap>) -> Self {
        let (x, y) = (x.into(), y.into());
        Self { left: x, top: y, right: x, bottom: y }
    }

    /// Each side named.
    pub fn edges(
        left: impl Into<Gap>,
        top: impl Into<Gap>,
        right: impl Into<Gap>,
        bottom: impl Into<Gap>,
    ) -> Self {
        Self { left: left.into(), top: top.into(), right: right.into(), bottom: bottom.into() }
    }

    /// The pixels, as the geometry layer wants them.
    pub fn resolve(self, theme: &Theme<'_>) -> Edges {
        Edges {
            left: self.left.resolve(theme),
            top: self.top.resolve(theme),
            right: self.right.resolve(theme),
            bottom: self.bottom.resolve(theme),
        }
    }

    pub fn hash(self, seed: WidgetHash) -> WidgetHash {
        self.bottom.hash(self.right.hash(self.top.hash(self.left.hash(seed))))
    }
}

impl From<Edges> for Pad {
    fn from(e: Edges) -> Self {
        Self::edges(e.left, e.top, e.right, e.bottom)
    }
}

impl From<i32> for Pad {
    fn from(px: i32) -> Self {
        Self::all(px)
    }
}

impl From<Gap> for Pad {
    fn from(g: Gap) -> Self {
        Self::all(g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::{testing, Palette};

    #[test]
    fn the_roles_resolve_to_the_toolkits_own_scale() {
        // The whole point: the same numbers an imperative screen reaches through
        // `theme.metrics.space`, not a second scale that nearly agrees with it.
        testing::with_theme(Palette::DARK, |t| {
            let s = t.metrics.space;
            assert_eq!(Gap::None.resolve(t), 0);
            assert_eq!(Gap::Hair.resolve(t), s.hair);
            assert_eq!(Gap::Tight.resolve(t), s.tight);
            assert_eq!(Gap::Snug.resolve(t), s.snug);
            assert_eq!(Gap::Base.resolve(t), s.base);
            assert_eq!(Gap::Wide.resolve(t), s.wide);
        });
    }

    #[test]
    fn a_number_is_still_a_gap() {
        // Every screen written before the roles existed keeps compiling, which is what makes this
        // change safe to land in one commit.
        testing::with_theme(Palette::DARK, |t| {
            assert_eq!(Gap::from(7).resolve(t), 7);
            assert_eq!(Pad::from(3).resolve(t), Edges::all(3));
        });
    }

    #[test]
    fn a_negative_distance_is_nothing_rather_than_a_hole() {
        // Distances flow into insets, and a negative one grows a rect instead of shrinking it —
        // which on a list row means painting on the title bar.
        testing::with_theme(Palette::DARK, |t| {
            assert_eq!(Gap::Exact(-5).resolve(t), 0);
            assert_eq!(Pad::all(-5).resolve(t), Edges::all(0));
        });
    }

    #[test]
    fn the_digest_is_of_the_role_and_not_of_the_pixels() {
        // `Gap::Snug` and `Gap::Exact(4)` are the same distance under this theme and are not the same
        // declaration. Hashing the resolved value would need a theme, which `content_hash` has not
        // got — and folding in the pixels would make a digest that changes with the palette.
        assert_ne!(Gap::Snug.hash(0), Gap::Exact(4).hash(0));
        // And the roles are told apart from each other.
        assert_ne!(Gap::Snug.hash(0), Gap::Base.hash(0));
        assert_ne!(Gap::None.hash(0), Gap::Hair.hash(0));
        assert_eq!(Gap::Base.hash(0), Gap::Base.hash(0));
    }

    #[test]
    fn a_pad_distinguishes_which_side_a_distance_is_on() {
        // Folded in order, so a padding of 4 on the left is not the same digest as 4 on the right —
        // which would otherwise let a mirrored layout keep its neighbour's measured size.
        let l = Pad { left: Gap::Snug, ..Pad::ZERO };
        let r = Pad { right: Gap::Snug, ..Pad::ZERO };
        assert_ne!(l.hash(0), r.hash(0));
        assert_eq!(l.hash(0), Pad { left: Gap::Snug, ..Pad::ZERO }.hash(0));
    }

    #[test]
    fn xy_is_across_then_down() {
        // Easy to transpose and invisible when transposed on a square box, so it is asserted rather
        // than left to the parameter names.
        testing::with_theme(Palette::DARK, |t| {
            let p = Pad::xy(Gap::Wide, Gap::Hair).resolve(t);
            assert_eq!((p.left, p.right), (t.metrics.space.wide, t.metrics.space.wide));
            assert_eq!((p.top, p.bottom), (t.metrics.space.hair, t.metrics.space.hair));
        });
    }

    #[test]
    fn edges_convert_both_ways_to_the_same_numbers() {
        testing::with_theme(Palette::DARK, |t| {
            let e = Edges { left: 1, top: 2, right: 3, bottom: 4 };
            assert_eq!(Pad::from(e).resolve(t), e);
        });
    }

    #[test]
    fn the_default_is_nothing() {
        // A container that has not been told a distance must not invent one: the old field default
        // was a zeroed `Edges` and a zero `i32`, and this has to keep meaning the same thing or every
        // screen in two external repos shifts by a few pixels.
        testing::with_theme(Palette::DARK, |t| {
            assert_eq!(Gap::default().resolve(t), 0);
            assert_eq!(Pad::default().resolve(t), Edges::all(0));
        });
    }
}
