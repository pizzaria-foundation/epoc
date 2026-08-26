//! The grounds this palette has, named by role.
//!
//! # Why this is not in `card.rs` any more
//!
//! It was, and it was called `CardSurface`, because a card was the first thing that needed to paint
//! a ground it did not author. It is not the last: [`Group`](super::Group) can paint one too now,
//! and a group is where most grounds actually come from — a card is a group that also rounds itself
//! and takes a slot.
//!
//! Naming it after one of its callers was the kind of mistake that is free to fix on the day it is
//! noticed and expensive a year later, when the name has spread into three repositories.

use symbian_ui::{Ground, Surface, Theme};

/// How far a derived card is pushed away from the page, in 1/255ths.
///
/// The number `tokens.rs`'s own `raised`/`sunken` tests use, borrowed rather than picked: a second
/// strength chosen here would make a card and a hand-written panel two slightly different depths on
/// the same screen.
pub(crate) const LIFT: u8 = 40;


/// What a band is made of, named by its role and resolved against the theme at draw time.
///
/// [`Ink`](super::Ink) for a ground, and it exists for the identical reason — see the module docs on
/// why a `Color` cannot be written in a `view` at all.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum SurfaceRole {
    /// The palette's own furniture band — the colour of the title and softkey bars.
    ///
    /// The default, and deliberately not a derivation. `Raised` and `Sunken` are computed *from the
    /// page*, and `DARK`'s page is `0x0B0F14`: lightening near-black by twenty gives a card that is
    /// technically distinguishable and practically invisible on a TN panel in daylight. `chrome` is
    /// authored per palette by whoever chose the palette, so it is the one surface guaranteed to
    /// read against the page in all five.
    #[default]
    Chrome,
    /// The page lifted: lighter above, darker below. A panel sitting *on* the screen.
    Raised,
    /// The page pressed in: darker above, lighter below. What the era used to say "something goes
    /// here" — a read-only block of values, a well around a form.
    Sunken,
}

impl SurfaceRole {
    /// The band this role paints.
    ///
    /// There is no `Flat` variant, and its absence is the point: `Surface::flat(bg.mid())` is the
    /// page, so a flat card is a card that is not there. A caller who wants only the padding and the
    /// rounding wants a [`Group`](super::Group), which costs no slot and no cache.
    pub fn resolve(self, theme: &Theme<'_>) -> Surface {
        self.resolve_on(theme, false)
    }

    /// The band this role paints, on a row that may be the selected one.
    ///
    /// # Why `selected` has to be a parameter
    ///
    /// Measured, not supposed: on `HIGH_CONTRAST` a default card draws **zero** pixels differing
    /// from the selection band. `chrome` is white there and so is `selection`, so a card on a
    /// focused row is a card that is not there — and `Raised`/`Sunken` are worse, because they are
    /// derived from `bg.mid()`, which is the *page* and not the ground under this card at all.
    ///
    /// This is the third instance of one defect: `chrome::control_colors` exists because `accent`,
    /// `dim` and `bg` are chosen against the page; `Chip` grew `.selected()` for the same reason;
    /// and the caption ink in `apps/uigallery` is the fourth. A colour role is only a role with
    /// respect to some ground, and until now every one of them assumed the page.
    pub fn resolve_on(self, theme: &Theme<'_>, selected: bool) -> Surface {
        let p = &theme.palette;
        if !selected {
            return match self {
                SurfaceRole::Chrome => p.chrome,
                SurfaceRole::Raised => Surface::raised(p.bg.mid(), LIFT),
                SurfaceRole::Sunken => Surface::sunken(p.bg.mid(), LIFT),
            };
        }
        // What changes on the band is the **base**, not the role. A `Raised` card is a panel sitting
        // on whatever it sits on and a `Sunken` one is a well pressed into it; that reading survives
        // the ground changing, so the derivation is the same one against `selection` instead of
        // against the page. Collapsing all three to one answer was the alternative and it throws
        // away the only thing the role was carrying.
        //
        // `Chrome` is the exception, because it is not a derivation at all — it is an authored
        // colour meaning "the palette's other band", and on `HIGH_CONTRAST` that colour *is* the
        // selection band. There is no authored "chrome on a highlight", so it takes the nearest
        // thing its name promises: a distinct band, lifted off the ground it is on.
        let band = p.selection.mid();
        match self {
            SurfaceRole::Chrome | SurfaceRole::Raised => Surface::raised(band, LIFT),
            SurfaceRole::Sunken => Surface::sunken(band, LIFT),
        }
    }

    /// What text drawn on this band should consider itself to be on.
    ///
    /// `Raised` and `Sunken` are the **page**, lifted or pressed by `LIFT`, and `LIFT` is 40 out of
    /// 255 — far enough to see an edge, nowhere near far enough to invalidate an ink chosen against
    /// the page. Saying `Ground::Page` for them is not a shrug; it is the measured answer, and it is
    /// why they are usable without a matching set of inks.
    ///
    /// `Chrome` is a different colour authored by whoever wrote the palette, so it gets its own
    /// ground and its own ink.
    pub fn ground(self) -> Ground {
        match self {
            SurfaceRole::Chrome => Ground::Chrome,
            SurfaceRole::Raised | SurfaceRole::Sunken => Ground::Page,
        }
    }

    /// A distinct byte per role, for [`content_hash`](Widget::content_hash).
    ///
    /// In the digest although a ground moves no pixel of the box, unlike `ListItem`'s `selected` —
    /// because it is one `i32` folded once per card and there are a handful of cards on a screen,
    /// where `selected` is folded once per row on a list of two hundred. The cost that made the
    /// exclusion worth arguing there does not exist here.
    pub(crate) const fn tag(self) -> i32 {
        self as i32
    }
}

