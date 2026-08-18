//! The softkey bar, as a widget.

use alloc::string::String;

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{chrome, Theme};

use crate::constraints::Constraints;
use crate::keys::Softkeys;
use crate::widget::{Widget, WidgetHash};

/// The bar across the bottom: options on the left, the action in the middle, back on the right.
///
/// # Prefer not to build one of these
///
/// A [`Screen`](crate::widgets::screen::Screen) makes its own bar out of its
/// [`Softkeys`], so the labels drawn are the labels dispatched and there is nothing to keep in
/// step. That is the whole structural point of this layer — see [`crate::keys`] — and it is given
/// away the moment a caller builds a bar by hand and puts a word on it that no handler answers to.
///
/// This type exists for the case a `Screen` cannot cover: a widget that is not a whole screen and
/// still wants the native bar under it. [`SoftkeyBar::from_keys`] is the constructor to reach for,
/// because it takes the labels from the same declaration the dispatcher reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SoftkeyBar {
    /// Options, action, back — the order [`chrome::softkey_bar`] draws them in. An array rather
    /// than three fields so the order cannot be transposed on the way out.
    labels: [Option<String>; 3],
}

impl SoftkeyBar {
    pub fn new() -> Self {
        Self::default()
    }

    /// The labels a [`Softkeys`] declaration promises, so the bar cannot say something the
    /// dispatcher does not answer.
    pub fn from_keys<M: Clone>(keys: &Softkeys<M>) -> Self {
        let [o, a, b] = keys.labels();
        Self { labels: [o.map(String::from), a.map(String::from), b.map(String::from)] }
    }

    /// Left softkey: the secondary offer.
    pub fn options(mut self, label: impl Into<String>) -> Self {
        self.labels[0] = Some(label.into());
        self
    }

    /// The middle slot, which is the D-pad centre and not a softkey at all. See [`crate::keys`].
    pub fn action(mut self, label: impl Into<String>) -> Self {
        self.labels[1] = Some(label.into());
        self
    }

    /// Right softkey: the way out, and never a second action.
    pub fn back(mut self, label: impl Into<String>) -> Self {
        self.labels[2] = Some(label.into());
        self
    }

    /// Borrowed labels in draw order.
    pub fn labels(&self) -> [Option<&str>; 3] {
        [
            self.labels[0].as_deref(),
            self.labels[1].as_deref(),
            self.labels[2].as_deref(),
        ]
    }

    /// Whether there is anything to show.
    ///
    /// A screen asks this to decide whether the band exists at all. An empty bar drawn anyway is
    /// seventeen pixels of chrome-coloured nothing at the bottom of the screen, and seventeen
    /// pixels is half a list row on a 240-pixel display.
    pub fn is_empty(&self) -> bool {
        self.labels.iter().all(Option::is_none)
    }

    /// The height the bar takes, or nothing when it is empty.
    pub fn height(&self, theme: &Theme<'_>) -> i32 {
        if self.is_empty() {
            0
        } else {
            theme.metrics.softkey_h
        }
    }
}

impl Widget for SoftkeyBar {
    /// Zero, for the reason [`TitleBar`](super::title_bar::TitleBar) gives: the height is a theme
    /// metric that no digest of these strings can see.
    fn content_hash(&self) -> WidgetHash {
        0
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        constraints.constrain(Size::new(constraints.max_w, self.height(theme)))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        // A bar with no labels measures to nothing, so it should never be handed a band — but
        // `chrome::softkey_bar` paints its background before it looks at the labels, and a caller
        // who lays out by hand and passes one anyway would get seventeen pixels of chrome-coloured
        // nothing across the bottom of the screen. The guard is here rather than in `chrome`
        // because the imperative toolkit's callers pass a bar they mean to draw; this one is
        // derived from a declaration that may be empty.
        if self.is_empty() {
            return;
        }
        chrome::softkey_bar(c, rect, theme, self.labels());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::{testing, Palette};

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Msg {
        Refresh,
        Back,
    }

    fn painted(px: &[u16]) -> usize {
        px.iter().filter(|&&p| p != 0).count()
    }

    #[test]
    fn the_labels_come_out_in_the_order_the_bar_draws_them() {
        let bar = SoftkeyBar::new().options("Refresh").action("Open").back("Back");
        assert_eq!(bar.labels(), [Some("Refresh"), Some("Open"), Some("Back")]);
    }

    #[test]
    fn a_bar_built_from_a_declaration_says_exactly_what_it_dispatches() {
        // The defect this crate is written against: a label in one function, a handler in another,
        // and nothing checking that they agree. Here there is one declaration and the bar is
        // derived from it.
        let keys = Softkeys::new().options("Refresh", Msg::Refresh).back("Back", Msg::Back);
        let bar = SoftkeyBar::from_keys(&keys);
        assert_eq!(bar.labels(), keys.labels());
        assert_eq!(bar.labels()[1], None, "no action was declared, so none is offered");
    }

    #[test]
    fn an_empty_bar_takes_no_height() {
        // Seventeen pixels of chrome-coloured nothing is half a list row on this screen.
        testing::with_theme(Palette::DARK, |t| {
            assert!(SoftkeyBar::new().is_empty());
            assert_eq!(SoftkeyBar::new().height(t), 0);
            assert_eq!(SoftkeyBar::new().measure(Constraints::loose(320, 240), t).h, 0);

            let one = SoftkeyBar::new().back("Back");
            assert!(!one.is_empty());
            assert_eq!(one.height(t), t.metrics.softkey_h);
        });
    }

    #[test]
    fn an_empty_bar_draws_nothing_even_when_handed_a_band() {
        testing::with_theme(Palette::DARK, |t| {
            let ((), px) = testing::with_canvas(Size::new(320, 240), |c| {
                SoftkeyBar::new().draw(c, Rect::new(0, 223, 320, 240), t);
            });
            assert_eq!(painted(&px), 0, "a bar with no labels still painted its band");
        });
    }

    #[test]
    fn it_draws_inside_its_band() {
        testing::with_theme(Palette::DARK, |t| {
            let top = 240 - t.metrics.softkey_h;
            let ((), px) = testing::with_canvas(Size::new(320, 240), |c| {
                SoftkeyBar::new().options("Refresh").action("Open").back("Back").draw(
                    c,
                    Rect::new(0, top, 320, 240),
                    t,
                );
            });
            assert!(painted(&px) > 0);
            assert_eq!(painted(&px[..(320 * top) as usize]), 0, "the bar bled into the content");
        });
    }

    #[test]
    fn an_empty_rect_draws_nothing() {
        testing::with_theme(Palette::DARK, |t| {
            let ((), px) = testing::with_canvas(Size::new(320, 240), |c| {
                SoftkeyBar::new().back("Back").draw(c, Rect::EMPTY, t);
            });
            assert_eq!(painted(&px), 0);
        });
    }
}
