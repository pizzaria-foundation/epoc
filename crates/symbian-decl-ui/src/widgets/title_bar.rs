//! The title bar, as a widget.

use alloc::string::String;

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{chrome, Theme};

use crate::constraints::Constraints;
use crate::widget::{hash_str, Widget, WidgetHash};

/// The bar across the top: what this screen is, and optionally one word about its state.
///
/// A thin wrapper over [`chrome::title_bar`] and deliberately nothing more. The band shape, the
/// light top edge and the right-aligned detail are decisions that belong to the imperative toolkit,
/// where every screen in this project — declarative or not — picks them up from one place. A
/// re-implementation here would be a second title bar to keep in step with the first, and the two
/// would drift the first time a palette gained an entry.
///
/// ```ignore
/// TitleBar::new("Telegram").detail("connected")
/// ```
#[derive(Clone, Debug, Default)]
pub struct TitleBar {
    title: String,
    detail: Option<String>,
}

impl TitleBar {
    pub fn new(title: impl Into<String>) -> Self {
        Self { title: title.into(), detail: None }
    }

    /// The right-aligned note: a connection state, an account, a count.
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// The same, but present only when there is something to say.
    pub fn detail_opt(mut self, detail: Option<impl Into<String>>) -> Self {
        self.detail = detail.map(Into::into);
        self
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn detail_text(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    /// The height this bar takes from the screen.
    ///
    /// From the theme, not from the text: the bar is furniture, and furniture that changed height
    /// with its label would move the content under it every time the detail appeared.
    pub fn height(theme: &Theme<'_>) -> i32 {
        theme.metrics.title_h
    }
}

impl Widget for TitleBar {
    /// Zero — always re-measure — and that is the cheap answer here, not the expensive one.
    ///
    /// This widget's height comes from `theme.metrics`, which a property hash cannot see. A digest
    /// over the title and detail would therefore claim "nothing changed" across a theme with a
    /// taller bar, and the content below would be laid out over the top of it. Measuring is two
    /// field reads; there is nothing to save by risking that.
    fn content_hash(&self) -> WidgetHash {
        0
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        constraints.constrain(Size::new(constraints.max_w, Self::height(theme)))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        chrome::title_bar(c, rect, theme, &self.title, self.detail.as_deref());
    }
}

/// A digest of a title bar's text, for a caller that is hashing a whole screen.
///
/// Not `content_hash`: this says "the words changed", which is a reason to redraw, while
/// `content_hash` says "the size changed", which is a reason to re-measure. They are different
/// questions and this bar answers them differently.
pub fn text_hash(bar: &TitleBar) -> WidgetHash {
    let h = hash_str(0, bar.title());
    hash_str(h, bar.detail_text().unwrap_or(""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::{testing, Palette};

    fn painted(px: &[u16]) -> usize {
        px.iter().filter(|&&p| p != 0).count()
    }

    #[test]
    fn the_bar_is_as_tall_as_the_theme_says_and_as_wide_as_it_is_offered() {
        testing::with_theme(Palette::DARK, |t| {
            let s = TitleBar::new("Recent").measure(Constraints::loose(320, 240), t);
            assert_eq!(s, Size::new(320, t.metrics.title_h));
            // Length of the title changes nothing: furniture that grew with its label would move
            // the content under it whenever the text did.
            let long = TitleBar::new("Recent conversations, all of them")
                .detail("connected")
                .measure(Constraints::loose(320, 240), t);
            assert_eq!(long, s);
        });
    }

    #[test]
    fn a_bar_offered_less_than_it_wants_takes_what_there_is() {
        testing::with_theme(Palette::DARK, |t| {
            let s = TitleBar::new("Recent").measure(Constraints::loose(320, 4), t);
            assert_eq!(s.h, 4, "clamped, not overflowing into the content below");
            assert!(s.h >= 0);
        });
    }

    #[test]
    fn it_draws_inside_its_rect_and_not_outside() {
        testing::with_theme(Palette::DARK, |t| {
            let h = t.metrics.title_h;
            let ((), px) = testing::with_canvas(Size::new(320, 240), |c| {
                TitleBar::new("Recent").detail("online").draw(c, Rect::new(0, 0, 320, h), t);
            });
            assert!(painted(&px) > 0);
            assert_eq!(painted(&px[(320 * h) as usize..]), 0, "the bar bled into the content");
        });
    }

    #[test]
    fn an_empty_rect_draws_nothing() {
        // `Frame::split` hands out `Rect::EMPTY` for a screen with no title, and every band has to
        // survive being handed one — a bar that drew at the origin instead would put a stripe
        // across the top of a screen that asked for none.
        testing::with_theme(Palette::DARK, |t| {
            let ((), px) = testing::with_canvas(Size::new(320, 240), |c| {
                TitleBar::new("Recent").draw(c, Rect::EMPTY, t);
            });
            assert_eq!(painted(&px), 0);
        });
    }

    #[test]
    fn the_text_digest_notices_both_halves() {
        let base = TitleBar::new("Recent");
        assert_ne!(text_hash(&base), text_hash(&TitleBar::new("Archive")));
        assert_ne!(text_hash(&base), text_hash(&base.clone().detail("online")));
        assert_ne!(
            text_hash(&base.clone().detail("online")),
            text_hash(&base.clone().detail("offline"))
        );
        assert_eq!(text_hash(&base), text_hash(&TitleBar::new("Recent")));
    }

    #[test]
    fn an_optional_detail_that_is_absent_is_the_same_as_none() {
        let none: Option<&str> = None;
        assert_eq!(TitleBar::new("x").detail_opt(none).detail_text(), None);
        assert_eq!(TitleBar::new("x").detail_opt(Some("y")).detail_text(), Some("y"));
    }
}
