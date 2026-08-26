//! The contract every widget answers to.

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{Clipboard, Handled, KeyEvent, Theme};

use crate::constraints::Constraints;
use crate::layout::CrossAlign;

/// What a widget may need to answer a key, beyond the key itself.
///
/// One context rather than a parameter per need, and that is a decision with a history: the two
/// widgets that had to have more were [`Screen`](crate::widgets::Screen), which cannot find its own
/// content band without the *theme*, and [`TextField`](crate::widgets::TextField), which cannot
/// paste without a *clipboard*. Adding them one at a time means editing the trait and every
/// implementation twice — and a third want would do it again. A context grows without touching the
/// widgets that do not care.
///
/// It mirrors [`Widget::draw`], which already takes what it needs to paint (`canvas`, `rect`,
/// `theme`); this is what a widget needs to *act*.
pub struct KeyCtx<'a> {
    /// The same theme the draw pass uses, so a widget that carves bands finds the same ones.
    pub theme: &'a Theme<'a>,
    /// Where copied text goes and pasted text comes from. `NoClipboard` on a build without one, so
    /// a widget never has to ask whether it exists.
    pub clip: &'a mut dyn Clipboard,
}

impl<'a> KeyCtx<'a> {
    pub fn new(theme: &'a Theme<'a>, clip: &'a mut dyn Clipboard) -> Self {
        Self { theme, clip }
    }
}

/// Build a throwaway [`KeyCtx`] over the test theme and no clipboard, and hand it to `f`.
///
/// A closure for the same reason [`symbian_ui::testing::with_theme`] is one: the theme borrows the
/// font atlas, so neither can be returned out of the function that owns them. Without this, every
/// test that presses a key against a widget spells out four lines of scaffolding — and the ones
/// that are about copy and paste pass their own clipboard to [`KeyCtx::new`] instead.
#[cfg(any(test, feature = "testing"))]
pub fn with_key_ctx<R>(f: impl FnOnce(&mut KeyCtx<'_>) -> R) -> R {
    symbian_ui::testing::with_theme(symbian_ui::Palette::DARK, |theme| {
        let mut clip = symbian_ui::NoClipboard;
        f(&mut KeyCtx::new(theme, &mut clip))
    })
}

/// A digest of everything about a widget that could change its size.
///
/// Not a general-purpose hash: it exists so the layout pass can skip [`Widget::measure`] on a frame
/// where nothing that matters moved. Text hashes its string and its font role; a
/// [`Group`](crate::widgets::Group) hashes its spacing and its children's digests. A widget whose
/// size never depends on its properties can leave it alone.
pub type WidgetHash = u64;

/// A thing that can size itself and draw itself.
///
/// Every method but [`Widget::measure`] and [`Widget::draw`] has a default, so a widget is two
/// functions. The defaults describe the simplest possible thing: no flex, and a hash of zero.
///
/// # What a widget is *not* asked
///
/// It is not asked what is inside it. This trait had `children()` and `gap()` once, on the theory
/// that a container would answer them and a generic pass would divide the box; the theory does not
/// survive the first column, because there is no axis in the trait and no way to recover one from a
/// `&dyn Widget` — a row and a column are indistinguishable through it. Structure lives in
/// [`Node`](crate::widgets::Node) and [`Group`](crate::widgets::Group), which the layout pass can
/// actually see into, and this trait is left saying only what a leaf can honestly answer. Two
/// methods nobody calls are worse than none: the next person implements them and wonders why
/// nothing happens.
///
/// # `content_hash` defaults to "always recompute"
///
/// Returning `0` means *this widget's size may have changed*, so measure runs every frame. That is
/// the safe default and the slow one; a widget opts into caching by returning a real digest. The
/// alternative default — assume nothing changed — would produce a screen that silently stops
/// updating, which is far harder to notice than a screen that is merely slower than it could be.
pub trait Widget {
    /// A digest of the properties that affect intrinsic size. See [`WidgetHash`].
    fn content_hash(&self) -> WidgetHash {
        0
    }

    /// The size this widget wants, within what the parent offers.
    ///
    /// Must return a size inside `constraints` — the layout pass constrains the result anyway, but
    /// a widget that ignores its offer has a bug that the clamp would otherwise hide.
    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size;

    /// Paint into the rect the layout gave. Never cached: drawing is cheap on this hardware
    /// compared with the blit that follows it, and a cached draw is a stale screen.
    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>);

    /// Where this widget sits across its parent's line, overriding the parent's choice.
    ///
    /// CSS's `align-self` to [`Group::align`](crate::widgets::Group::align)'s `align-items`, and it
    /// reaches the layout the same way [`flex_weight`](Self::flex_weight) does — the engine asks the
    /// child rather than the child registering anything with the parent, because a `Node` carries no
    /// per-child record to put it in.
    ///
    /// `None` means "whatever the line says", which is the right default: a row of labels wants one
    /// answer for all of them. Overriding is for the child that genuinely differs — a chat bubble
    /// that hugs the right edge because it is outgoing while its neighbours hug the left.
    fn align_self(&self) -> Option<CrossAlign> {
        None
    }

    /// Whether this widget is allowed to paint outside the rect it was given.
    ///
    /// CSS's `overflow`, declared by the box itself — and deliberately with the opposite default.
    /// In a browser `visible` is the initial value and a box never clips its own painting; here the
    /// default clips, because a widget whose draw runs a pixel wide would otherwise eat its
    /// neighbour silently, and on this hardware "silently" means a screenshot from the device.
    ///
    /// Returning `true` is for widgets whose ink is genuinely larger than their line box. The unread
    /// [`Badge`](crate::widgets::Badge) is the case this exists for: a pill is two pixels taller than
    /// the small text beside it, so in a line sized to that text it does not fit and the hand-written
    /// row lets it reach into the line above. That overlap is the design, not an overrun. An ancestor
    /// that clips still clips — see [`Group::overflow_visible`](crate::widgets::Group::overflow_visible)
    /// — which is what keeps a row from painting onto the title bar while its badge overlaps its own
    /// name line.
    fn overflow_visible(&self) -> bool {
        false
    }

    /// Handle a key aimed at this widget's `rect`. Default: not mine.
    ///
    /// `&self`, not `&mut self`: a widget that changes on a key holds its state in the slot table
    /// behind an `Rc` — a caret, a scroll offset — so a shared walk of the tree is enough to drive
    /// every key. See [`crate::slot`] for why that state does not live in the model.
    fn handle_key(&self, _ev: KeyEvent, _rect: Rect, _cx: &mut KeyCtx<'_>) -> Handled {
        Handled::Ignored
    }

    /// This widget's weight when its parent divides leftover space. `0` is fixed.
    ///
    /// Kept on the widget rather than moved onto the container with the rest of the structure,
    /// because it is the one layout property that reads better at the call site than in a parallel
    /// list: `Text::new(&chat.name).flex(1)` says what that label does where the label is written,
    /// and a container holding its children's weights in a second vector would be one more thing to
    /// keep in step with the children by hand.
    ///
    /// A negative or zero weight is not a share. That rule is not enforced here — it is applied
    /// once, in [`Node::weight`](crate::widgets::Node::weight), through
    /// [`Length::weight`](crate::length::Length::weight), so the container is the only thing that
    /// decides what counts as a claim.
    /// Whether this widget has the keyboard, for a container that wants to say so on its behalf.
    ///
    /// `None` means "not a thing that takes focus" — a `Spacer`, a `Text` — and is the default, so
    /// no existing widget has to answer.
    ///
    /// # Why a container has to ask instead of being told
    ///
    /// [`FieldRow`](crate::widgets::FieldRow) paints a focus cue — its caption goes to the accent —
    /// for a control it accepts **already built**, so it could not reach in and had to be told
    /// separately: `.control(TextField::new(slots).focused(here)).focused(here)`. Its own module
    /// docs called that a duplication and it cost two real bugs, both in this SDK's own gallery, one
    /// of which shipped and was found by a person holding the phone: the caption lit, the field
    /// stayed dead, and the only true signal was the *absence* of a caret.
    ///
    /// A duplicated parameter is a trap exactly when it carries no information. `FieldRow` has one
    /// control slot, so its two flags can only ever agree — a disagreement *is* the bug.
    /// [`ListItem`](crate::widgets::ListItem) has two (`leading` and `trailing`) and only one of
    /// them can hold the cursor, so its flags carry a real fact and it is deliberately left alone.
    ///
    /// The alternative was a closure, `control(|focused| Node)`, the shape
    /// [`FocusScope::stop`](crate::widgets::FocusScope::stop) uses. It was rejected on this crate's
    /// own precedent: a `FieldRow` builder must not depend on the order its methods are called in —
    /// there is a test saying so — which means the closure has to be *stored* rather than run, and
    /// storing it is a `Box` per field per frame. That is the cost `Outbox::wrapped` is refused for.
    fn focus_state(&self) -> Option<bool> {
        None
    }

    fn flex_weight(&self) -> i32 {
        0
    }
}

/// Combine a value into a running hash.
///
/// FNV-1a, chosen because it is eight lines, needs no allocation and no state, and this is a
/// change-detector rather than a hash table — collisions cost a missed re-measure on one frame, not
/// corruption. Deliberately not `core::hash`, whose `Hasher` would drag a trait object or a
/// generic parameter through the whole widget tree for no benefit at this size.
pub fn hash_bytes(seed: WidgetHash, bytes: &[u8]) -> WidgetHash {
    const PRIME: u64 = 0x0000_0100_0000_01B3;
    let mut h = if seed == 0 { 0xcbf2_9ce4_8422_2325 } else { seed };
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// [`hash_bytes`] for a number.
pub fn hash_i32(seed: WidgetHash, v: i32) -> WidgetHash {
    hash_bytes(seed, &v.to_le_bytes())
}

/// [`hash_bytes`] for text.
pub fn hash_str(seed: WidgetHash, s: &str) -> WidgetHash {
    hash_bytes(seed, s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::testing;

    /// The smallest possible widget: a fixed empty box. Also the shape the plan calls `Spacer`,
    /// kept here so the trait has something to prove itself against before any real widget exists.
    struct Fixed(i32, i32);

    impl Widget for Fixed {
        fn content_hash(&self) -> WidgetHash {
            hash_i32(hash_i32(0, self.0), self.1)
        }
        fn measure(&self, constraints: Constraints, _theme: &Theme<'_>) -> Size {
            constraints.constrain(Size::new(self.0, self.1))
        }
        fn draw(&self, _c: &mut Canvas<'_>, _rect: Rect, _theme: &Theme<'_>) {}
    }

    #[test]
    fn a_leaf_widget_is_two_functions() {
        testing::with_theme(symbian_ui::Palette::DARK, |t| {
            let w = Fixed(10, 4);
            assert_eq!(w.measure(Constraints::loose(100, 50), t), Size::new(10, 4));
            // Everything else comes from the defaults.
            assert_eq!(w.flex_weight(), 0);
        });
    }

    #[test]
    fn measure_respects_the_offer() {
        testing::with_theme(symbian_ui::Palette::DARK, |t| {
            // Asked for more than it may have: the answer is clamped, not the request honoured.
            assert_eq!(Fixed(999, 999).measure(Constraints::loose(20, 10), t), Size::new(20, 10));
            // A tight offer leaves no room to be smaller.
            assert_eq!(Fixed(1, 1).measure(Constraints::tight(30, 40), t), Size::new(30, 40));
        });
    }

    #[test]
    fn the_hash_changes_when_the_size_would() {
        assert_ne!(Fixed(10, 4).content_hash(), Fixed(11, 4).content_hash());
        assert_ne!(Fixed(10, 4).content_hash(), Fixed(10, 5).content_hash());
        assert_eq!(Fixed(10, 4).content_hash(), Fixed(10, 4).content_hash());
    }

    #[test]
    fn hashing_distinguishes_the_things_it_has_to() {
        // Different text, different digest — otherwise a re-worded label keeps a stale size.
        assert_ne!(hash_str(0, "Open"), hash_str(0, "Back"));
        // Order matters: two fields swapped are not the same widget.
        assert_ne!(hash_i32(hash_i32(0, 1), 2), hash_i32(hash_i32(0, 2), 1));
        // And the same input is always the same digest, or nothing would ever cache.
        assert_eq!(hash_str(0, "Open"), hash_str(0, "Open"));
    }

    #[test]
    fn a_zero_seed_is_the_start_not_a_value() {
        // `content_hash` returns 0 to mean "always recompute", so the hasher must never produce 0
        // for real content — otherwise a widget would opt out of caching by accident.
        assert_ne!(hash_str(0, ""), 0);
        assert_ne!(hash_i32(0, 0), 0);
    }
}
