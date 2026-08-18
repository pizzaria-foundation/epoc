//! A whole screen: a title, some content, and softkeys that mean what they say.

use alloc::boxed::Box;
use alloc::string::String;

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{chrome, paint, Frame, Handled, KeyEvent, Theme};

use crate::constraints::Constraints;
use crate::keys::Softkeys;
use crate::widget::{KeyCtx, Widget, WidgetHash};
use crate::widgets::title_bar::TitleBar;

/// The top of a screen's widget tree.
///
/// ```ignore
/// Screen::new()
///     .title("Recent")
///     .content(ScrollList::new(&rows))
///     .on_options("Refresh", Msg::Refresh)
///     .on_action("Open", Msg::Open)
///     .on_back("Back", Msg::Back)
/// ```
///
/// # A label and its handler are one declaration
///
/// The bar this screen draws is generated from [`Softkeys`], and [`Screen::dispatch`] reads the
/// same value. There is no way to set the labels separately, because that is the bug: the launcher
/// shipped a task manager whose middle label read `Sort` and whose handler was bound to
/// `Softkey::Middle`, an event S60 never sends. Both halves were internally consistent and the key
/// did something other than what it said. Here you cannot write a label without a message, and you
/// cannot write a message without a label — see [`crate::keys`].
///
/// `handle_key` reports whether the bar claimed the key; the *message* comes out of
/// [`Screen::dispatch`], because [`Widget`] has no message type to return one through. An app
/// normally never calls either: [`DeclarativeApp::keys`](crate::app::DeclarativeApp::keys) routes
/// the same declaration from the bridge, which is where a key arrives.
///
/// # Why the content is not reported as a child
///
/// A screen does not divide its box by flex weight — it carves three fixed bands with
/// [`Frame::split`] and gives one of them away whole. Handing the content to the layout pass as a
/// child as well would invite that pass to place it a second time, at a rect it computed a
/// different way: two answers to where the list goes, and the one that draws last wins. The
/// [`Widget`] trait no longer has a way to say it either, which is the same conclusion reached from
/// the other end.
pub struct Screen<M> {
    title: Option<TitleBar>,
    /// Whether the softkey band is kept when no key is labelled. See [`Screen::keep_softkey_band`].
    keep_band: bool,
    content: Option<Box<dyn Widget>>,
    /// A band pinned to the bottom of the content area, above the softkey bar. See
    /// [`Screen::footer`].
    footer: Option<Box<dyn Widget>>,
    keys: Softkeys<M>,
}

impl<M: Clone> Screen<M> {
    pub fn new() -> Self {
        Self { title: None, content: None, footer: None, keys: Softkeys::new(), keep_band: false }
    }

    /// A plain title bar with this text.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(TitleBar::new(title));
        self
    }

    /// A title bar built elsewhere — with a detail on the right, say.
    pub fn title_bar(mut self, bar: TitleBar) -> Self {
        self.title = Some(bar);
        self
    }

    /// What fills the space between the bars.
    pub fn content(mut self, content: impl Widget + 'static) -> Self {
        self.content = Some(Box::new(content));
        self
    }

    /// Left softkey, with the message it promises.
    pub fn on_options(mut self, label: impl Into<String>, msg: M) -> Self {
        self.keys = self.keys.options(label, msg);
        self
    }

    /// The action — labelled in the middle of the bar, fired by the D-pad centre.
    pub fn on_action(mut self, label: impl Into<String>, msg: M) -> Self {
        self.keys = self.keys.action(label, msg);
        self
    }

    /// Right softkey: the way out.
    pub fn on_back(mut self, label: impl Into<String>, msg: M) -> Self {
        self.keys = self.keys.back(label, msg);
        self
    }

    /// A whole bar declared elsewhere — for a screen whose offer is computed from its model.
    pub fn softkeys(mut self, keys: Softkeys<M>) -> Self {
        self.keys = keys;
        self
    }

    /// The declaration behind both the bar and the dispatch.
    pub fn keys(&self) -> &Softkeys<M> {
        &self.keys
    }

    /// The labels this screen draws, in bar order. Derived, never stored.
    pub fn labels(&self) -> [Option<&str>; 3] {
        self.keys.labels()
    }

    pub fn title_text(&self) -> Option<&str> {
        self.title.as_ref().map(TitleBar::title)
    }

    /// Keep the softkey band even when no key is labelled.
    ///
    /// By default a screen with no labels gives the band's pixels to its content, which is right for
    /// something drawn edge to edge — a photo viewer — and wrong for a *form* that happens to have
    /// nothing to offer this second. `tg`'s login screen is the second case: with no connection yet
    /// there is no "Avançar" to press, and the hand-written screen still draws the bar, because on
    /// S60 the bar is furniture rather than a control. Without this the band vanishes, the content
    /// band grows by seventeen pixels, and everything centred in it moves — which is what the
    /// comparison against that screen found.
    pub fn keep_softkey_band(mut self) -> Self {
        self.keep_band = true;
        self
    }

    /// Whether there is a softkey bar at all.
    pub fn has_softkeys(&self) -> bool {
        self.keep_band || self.labels().iter().any(Option::is_some)
    }

    /// The three bands, in screen coordinates.
    ///
    /// [`Frame::split`] is the one place this arithmetic lives, shared with every imperative screen
    /// in the toolkit. Recomputing it here would be a second answer to "how tall is the title bar",
    /// and the two would disagree the first time a theme changed a metric.
    ///
    /// A band that is not wanted comes back as [`Rect::EMPTY`] and its pixels go to the content —
    /// not as a zero-height rect sitting at the boundary, which would leave a gap the content could
    /// not use and every drawing routine would have to test for anyway.
    pub fn bands(&self, screen: Rect, theme: &Theme<'_>) -> Frame {
        Frame::split(screen, theme, self.title.is_some(), self.has_softkeys())
    }

    /// A band pinned to the bottom of the content, above the softkey bar.
    ///
    /// The fourth band, and it is a band rather than the last child of the content because it does
    /// not compete for space: a message composer is as tall as one line of text plus its rule,
    /// whatever the transcript above it is doing, and a transcript that grew until it squeezed the
    /// composer would be a transcript you could no longer reply from.
    ///
    /// It takes its own measured height off the bottom of the content band and the rest goes to the
    /// content, so the content never has to know the footer exists. That is the same bargain
    /// [`Frame::split`] already strikes for the title and softkey bars — this simply makes it four
    /// bands instead of three, for the one screen shape that needs it.
    ///
    /// A footer taller than the whole content band is clamped to it rather than pushing the content
    /// to a negative height.
    pub fn footer(mut self, footer: impl Widget + 'static) -> Self {
        self.footer = Some(Box::new(footer));
        self
    }

    /// Split the content band into what the content gets and what the footer gets.
    ///
    /// One function, called by both `draw` and anything that needs to know where the content
    /// actually ended up — a second copy of this arithmetic is how a caret lands one band off.
    fn content_and_footer(&self, content: Rect, theme: &Theme<'_>) -> (Rect, Rect) {
        let Some(footer) = &self.footer else {
            return (content, Rect::EMPTY);
        };
        let want = footer
            .measure(Constraints::loose(content.width(), content.height()), theme)
            .h
            .clamp(0, content.height());
        content.split_bottom(want)
    }

    /// What this key press means to this screen, if anything.
    ///
    /// Delegates to [`Softkeys::dispatch`], which knows the one piece of platform trivia involved:
    /// the middle label is fired by [`Key::Select`](symbian_ui::Key::Select), never by
    /// `Softkey::Middle`.
    pub fn dispatch(&self, ev: KeyEvent) -> Option<M> {
        self.keys.dispatch(ev)
    }
}

impl<M: Clone> Default for Screen<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: Clone> Widget for Screen<M> {
    /// Zero: measuring a screen is clamping the offer, which is cheaper than the hash would be.
    ///
    /// A constant non-zero digest would be the "nothing ever changes" answer and would be wrong for
    /// a subtler reason — the size depends on the *offer*, not on any property, and the cache keys
    /// on the digest alone. A screen given a different rect would keep the old one.
    fn content_hash(&self) -> WidgetHash {
        0
    }

    fn measure(&self, constraints: Constraints, _theme: &Theme<'_>) -> Size {
        // Everything it is given: a screen is the root, and there is nothing above it to fill.
        constraints.constrain(Size::new(constraints.max_w, constraints.max_h))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        if rect.is_empty() {
            return;
        }
        let f = self.bands(rect, theme);

        // The page, first and once. Not `chrome::clear`, which fills the whole canvas: a screen is
        // drawn into the rect it was given, and a screen that painted outside it could not be
        // composed inside anything else — a dialog over a list, for one.
        paint::band(c, rect, &theme.palette.bg);

        if let Some(bar) = &self.title {
            bar.draw(c, f.title, theme);
        }

        let (body, footer_band) = self.content_and_footer(f.content, theme);
        if let Some(footer) = &self.footer {
            footer.draw(c, footer_band, theme);
        }

        if let Some(content) = &self.content {
            // A screen is where the overflow shows: the content band has the softkey bar directly
            // under it, so a child that returns a size larger than the band draws over the bar and
            // nothing says so. Costs a second measure in a debug build and nothing at all in a
            // release one — see [`crate::overflow`].
            crate::overflow::check("Screen content", content.as_ref(), body, theme);

            // Handing the band straight to the child, which is what [`Widget::draw`] means: paint
            // into the rect the layout gave you. Correct for a leaf and for any container that
            // places its own children; what it does not do is reuse a cached measurement.
            //
            // TODO(layout): `crate::layout` measures, places and draws a `widgets::Node` tree with
            // a `UiCache` behind it — `draw_tree(&Node, &UiCache, &mut Canvas, &Theme)` — rather
            // than a `&dyn Widget` and a rect. Screen keeps its content as a `Box<dyn Widget>`
            // because that is what `Widget::children` is typed as, so joining the two is a decision
            // about which of the two tree representations wins, not a line to write here. Flagged
            // in the Phase 2 report; deliberately not resolved by growing a second layout pass in
            // this file, which is the duplication the crate exists to remove.
            content.draw(c, body, theme);
        }

        // From `self.keys`, not from a stored bar. The labels drawn and the labels dispatched are
        // the same value read twice; there is nothing here that could disagree with `dispatch`.
        chrome::softkey_bar(c, f.softkeys, theme, self.labels());
    }

    /// The bar first, then the rodapé, then the content.
    ///
    /// The message goes through [`Screen::dispatch`]; this answers the only question [`Widget`] can
    /// ask, which is whether anyone wanted the press. Returning [`Handled::Consumed`] for a key
    /// nobody answered would be the worse mistake: it tells the host to repaint for a press that
    /// changed nothing, and on this device a repaint is a full-screen blit.
    ///
    /// # Why this had to forward at all
    ///
    /// Every declarative app is one `Node::leaf(Screen)`, so the tree walk that finally delivers
    /// keys ([`crate::layout::dispatch_key`]) reaches this widget and stops. A screen that only
    /// asked its softkey bar meant a composer inside it could never be typed into — the key arrived
    /// at the screen and died one band short of the field.
    ///
    /// The bands come from [`Screen::bands`] and [`Screen::content_and_footer`], the same two
    /// functions `draw` uses. That is the whole reason [`KeyCtx`] carries a theme: computing the
    /// bands a second way here is how a caret ends up in a different place than the text it belongs
    /// to.
    ///
    /// Footer before content, because the footer is the thing pinned under the user's hands — a
    /// composer under a transcript — and a key that both would take belongs to the one being typed
    /// into. Both self-veto when unfocused, so in practice at most one of them answers.
    fn handle_key(&self, ev: KeyEvent, rect: Rect, cx: &mut KeyCtx<'_>) -> Handled {
        if self.dispatch(ev).is_some() {
            return Handled::Consumed;
        }
        let f = self.bands(rect, cx.theme);
        let (body, foot) = self.content_and_footer(f.content, cx.theme);
        if let Some(footer) = &self.footer {
            if !foot.is_empty() && footer.handle_key(ev, foot, cx) == Handled::Consumed {
                return Handled::Consumed;
            }
        }
        if let Some(content) = &self.content {
            if !body.is_empty() {
                return content.handle_key(ev, body, cx);
            }
        }
        Handled::Ignored
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::{testing, Key, Palette, Softkey};

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Msg {
        Refresh,
        Open,
        Back,
    }

    /// Content that inks every pixel it is given, so a test can see exactly which band it got.
    struct Fill;

    impl Widget for Fill {
        fn measure(&self, c: Constraints, _t: &Theme<'_>) -> Size {
            c.constrain(Size::new(c.max_w, c.max_h))
        }
        fn draw(&self, c: &mut Canvas<'_>, rect: Rect, _t: &Theme<'_>) {
            c.fill_rect(rect, symbian_gfx::Color::hex(0xFF00FF));
        }
    }

    fn full() -> Screen<Msg> {
        Screen::new()
            .title("Recent")
            .content(Fill)
            .on_options("Refresh", Msg::Refresh)
            .on_action("Open", Msg::Open)
            .on_back("Back", Msg::Back)
    }

    fn press(k: Key) -> KeyEvent {
        KeyEvent::new(k)
    }

    /// Rows of the buffer that contain the given colour, as a `y0..y1` span.
    fn rows_with(px: &[u16], w: i32, h: i32, wanted: u16) -> Option<(i32, i32)> {
        let mut first = None;
        let mut last = None;
        for y in 0..h {
            let row = &px[(y * w) as usize..((y + 1) * w) as usize];
            if row.contains(&wanted) {
                first.get_or_insert(y);
                last = Some(y);
            }
        }
        Some((first?, last? + 1))
    }

    // ---- the bands ------------------------------------------------------------------------------

    #[test]
    fn the_bands_tile_the_screen_with_nothing_lost_and_nothing_overlapping() {
        testing::with_theme(Palette::DARK, |t| {
            let screen = testing::SCREEN;
            let f = full().bands(screen, t);

            assert_eq!(f.title.y0, screen.y0, "the title starts at the top");
            assert_eq!(f.title.y1, f.content.y0, "a gap between the title and the content");
            assert_eq!(f.content.y1, f.softkeys.y0, "a gap above the softkeys");
            assert_eq!(f.softkeys.y1, screen.y1, "the bar ends at the bottom");
            assert_eq!(
                f.title.height() + f.content.height() + f.softkeys.height(),
                screen.height(),
                "the three bands must account for every row; the arithmetic loses one otherwise"
            );
            for band in [f.title, f.content, f.softkeys] {
                assert_eq!(band.x0, screen.x0);
                assert_eq!(band.x1, screen.x1);
            }
        });
    }

    #[test]
    fn a_form_can_keep_the_band_with_nothing_on_it() {
        // The opposite case, and it is not symmetry for its own sake: on S60 the softkey bar is
        // furniture, and a form with no offer this second — a login screen waiting for a connection
        // — still draws it. Without this the band's seventeen pixels go to the content and
        // everything centred in it moves, which is what the comparison against `tg`'s login screen
        // found.
        testing::with_theme(Palette::DARK, |t| {
            let screen = testing::SCREEN;
            let labelled = full().bands(screen, t);
            let bare = Screen::<Msg>::new()
                .title("Entrar")
                .content(Fill)
                .keep_softkey_band()
                .bands(screen, t);

            assert!(!bare.softkeys.is_empty(), "the band was dropped for want of a label");
            assert_eq!(bare.content, labelled.content, "the content band must not have moved");
            assert_eq!(bare.softkeys.height(), t.metrics.softkey_h);
            // And it still routes nothing, because there is nothing to route.
            assert_eq!(Screen::<Msg>::new().keep_softkey_band().dispatch(press(Key::Select)), None);
        });
    }

    #[test]
    fn a_screen_with_no_softkeys_gives_the_band_to_the_content() {
        // Absent, not present-and-empty. A zero-height bar sitting at the boundary would leave the
        // pixels stranded: neither the bar nor the content could use them.
        testing::with_theme(Palette::DARK, |t| {
            let screen = testing::SCREEN;
            let with = full().bands(screen, t);
            let without = Screen::<Msg>::new().title("Recent").content(Fill).bands(screen, t);

            assert!(without.softkeys.is_empty());
            assert_eq!(without.content.y1, screen.y1, "the content runs to the bottom edge");
            assert_eq!(
                without.content.height(),
                with.content.height() + t.metrics.softkey_h,
                "the content gained exactly the bar's pixels"
            );
            assert_eq!(
                without.title.height() + without.content.height() + without.softkeys.height(),
                screen.height()
            );
        });
    }

    #[test]
    fn a_screen_with_no_title_gives_that_band_to_the_content_too() {
        testing::with_theme(Palette::DARK, |t| {
            let screen = testing::SCREEN;
            let f = Screen::<Msg>::new().content(Fill).on_back("Back", Msg::Back).bands(screen, t);
            assert!(f.title.is_empty());
            assert_eq!(f.content.y0, screen.y0);
            assert_eq!(f.content.height() + f.softkeys.height(), screen.height());
        });
    }

    #[test]
    fn a_bare_screen_is_all_content() {
        testing::with_theme(Palette::DARK, |t| {
            let f = Screen::<Msg>::new().content(Fill).bands(testing::SCREEN, t);
            assert_eq!(f.content, testing::SCREEN);
            assert!(f.title.is_empty() && f.softkeys.is_empty());
        });
    }

    #[test]
    fn the_bands_never_invert_however_little_room_there_is() {
        // A screen shorter than its own chrome is not hypothetical — it is what a band gets after
        // a parent has subtracted padding from it. An inverted rect draws nothing and reports
        // nothing, which is the failure mode `Constraints` exists to keep out of the layout.
        testing::with_theme(Palette::DARK, |t| {
            for h in [0, 1, 5, 17, 18, 34, 35, 36, 240] {
                let screen = Rect::from_xywh(0, 0, 320, h);
                let f = full().bands(screen, t);
                for (name, band) in
                    [("title", f.title), ("content", f.content), ("softkeys", f.softkeys)]
                {
                    assert!(band.x1 >= band.x0 && band.y1 >= band.y0, "{name} inverted at h={h}");
                }
                assert_eq!(
                    f.title.height() + f.content.height() + f.softkeys.height(),
                    h,
                    "rows went missing at h={h}"
                );
            }
        });
    }

    #[test]
    fn a_screen_measures_to_everything_it_is_offered() {
        testing::with_theme(Palette::DARK, |t| {
            assert_eq!(full().measure(Constraints::tight(320, 240), t), Size::new(320, 240));
            assert_eq!(full().measure(Constraints::loose(320, 240), t), Size::new(320, 240));
            assert_eq!(full().measure(Constraints::loose(-4, -4), t), Size::new(0, 0));
        });
    }

    // ---- the labels and the keys are one declaration ---------------------------------------------

    #[test]
    fn the_dpad_centre_fires_the_action_and_the_outer_keys_their_own_slots() {
        let s = full();
        assert_eq!(s.dispatch(press(Key::Select)), Some(Msg::Open));
        assert_eq!(s.dispatch(press(Key::Softkey(Softkey::Left))), Some(Msg::Refresh));
        assert_eq!(s.dispatch(press(Key::Softkey(Softkey::Right))), Some(Msg::Back));
    }

    #[test]
    fn every_label_has_a_handler_and_every_handler_has_a_label() {
        // The invariant the builder buys, checked slot by slot on every shape a screen can take.
        // There is no `.softkey_bar(..)` setter to break it with — the drawn labels are read out of
        // the same `Softkeys` that `dispatch` reads, so a bar that promises something no arm
        // answers is not a thing that can be written.
        let slots = [
            Key::Softkey(Softkey::Left),
            Key::Select,
            Key::Softkey(Softkey::Right),
        ];
        let screens = [
            Screen::<Msg>::new(),
            Screen::<Msg>::new().on_back("Back", Msg::Back),
            Screen::<Msg>::new().on_action("Open", Msg::Open),
            Screen::<Msg>::new().on_options("Refresh", Msg::Refresh).on_back("Back", Msg::Back),
            full(),
        ];
        for s in &screens {
            for (i, key) in slots.iter().enumerate() {
                assert_eq!(
                    s.labels()[i].is_some(),
                    s.dispatch(press(*key)).is_some(),
                    "slot {i} labelled {:?} but dispatch says {:?}",
                    s.labels()[i],
                    s.dispatch(press(*key)).is_some()
                );
            }
        }
    }

    #[test]
    fn an_unlabelled_slot_fires_nothing() {
        let only_back = Screen::<Msg>::new().on_back("Back", Msg::Back);
        assert_eq!(only_back.dispatch(press(Key::Select)), None);
        assert_eq!(only_back.dispatch(press(Key::Softkey(Softkey::Left))), None);
        assert_eq!(only_back.dispatch(press(Key::Softkey(Softkey::Right))), Some(Msg::Back));
        assert_eq!(only_back.labels(), [None, None, Some("Back")]);
    }

    #[test]
    fn ordinary_keys_are_left_for_the_content() {
        // Up, Down and typing must reach the list and the text field. A bar that swallowed them
        // would break every screen that has anything in it.
        let s = full();
        crate::widget::with_key_ctx(|cx| {
            for k in [Key::Up, Key::Down, Key::Left, Key::Right, Key::Char('a'), Key::Backspace] {
                assert_eq!(s.dispatch(press(k)), None, "{k:?} was taken by the bar");
                // `Fill` is the content here and answers no key, so these still fall through — but
                // now they fall through *the content*, which is the whole point of the forward.
                assert_eq!(s.handle_key(press(k), testing::SCREEN, cx), Handled::Ignored);
            }
        });
    }

    #[test]
    fn handle_key_consumes_only_what_it_answers() {
        // `Consumed` is what tells the host to repaint. Saying it for a key nothing answered is a
        // full-screen blit for a press that changed nothing.
        let s = full();
        crate::widget::with_key_ctx(|cx| {
            assert_eq!(s.handle_key(press(Key::Select), testing::SCREEN, cx), Handled::Consumed);
            assert_eq!(
                s.handle_key(press(Key::Softkey(Softkey::Right)), testing::SCREEN, cx),
                Handled::Consumed
            );

            let bare = Screen::<Msg>::new().content(Fill);
            assert_eq!(bare.handle_key(press(Key::Select), testing::SCREEN, cx), Handled::Ignored);
            assert_eq!(
                bare.handle_key(press(Key::Softkey(Softkey::Left)), testing::SCREEN, cx),
                Handled::Ignored
            );
        });
    }

    #[test]
    fn a_softkeys_value_built_elsewhere_lands_in_both_places() {
        // A screen whose offer depends on its model builds the declaration and hands it over whole.
        // Both halves still come from it.
        let keys = Softkeys::new().action("Send", Msg::Open).back("Cancel", Msg::Back);
        let s = Screen::<Msg>::new().softkeys(keys);
        assert_eq!(s.labels(), [None, Some("Send"), Some("Cancel")]);
        assert_eq!(s.dispatch(press(Key::Select)), Some(Msg::Open));
        assert_eq!(s.dispatch(press(Key::End)), Some(Msg::Back), "the red key is a way out");
    }

    // ---- drawing --------------------------------------------------------------------------------

    #[test]
    fn the_content_is_drawn_into_the_content_band_and_not_over_the_chrome() {
        testing::with_theme(Palette::DARK, |t| {
            let f = full().bands(testing::SCREEN, t);
            let ((), px) = testing::with_canvas(Size::new(320, 240), |c| {
                full().draw(c, testing::SCREEN, t);
            });
            let marker = symbian_gfx::Color::hex(0xFF00FF).to_rgb565().0;
            assert_eq!(
                rows_with(&px, 320, 240, marker),
                Some((f.content.y0, f.content.y1)),
                "the content did not fill exactly the band it was given"
            );
        });
    }

    #[test]
    fn a_screen_with_no_title_puts_no_stripe_at_the_top() {
        testing::with_theme(Palette::DARK, |t| {
            let ((), px) = testing::with_canvas(Size::new(320, 240), |c| {
                Screen::<Msg>::new().content(Fill).on_back("Back", Msg::Back).draw(
                    c,
                    testing::SCREEN,
                    t,
                );
            });
            let marker = symbian_gfx::Color::hex(0xFF00FF).to_rgb565().0;
            let (first, _) = rows_with(&px, 320, 240, marker).expect("content drew nothing");
            assert_eq!(first, 0, "something occupied the top row of a screen with no title bar");
        });
    }

    #[test]
    fn a_screen_drawn_into_an_empty_rect_paints_nothing() {
        testing::with_theme(Palette::DARK, |t| {
            let ((), px) = testing::with_canvas(Size::new(320, 240), |c| {
                full().draw(c, Rect::EMPTY, t);
                full().draw(c, Rect::new(100, 100, 20, 20), t);
            });
            assert!(px.iter().all(|&p| p == 0), "a screen painted outside an empty band");
        });
    }

    #[test]
    fn a_screen_stays_inside_the_rect_it_was_given() {
        // The reason `draw` does not call `chrome::clear`: a screen must be composable inside
        // something else, and one that filled the canvas could never be a dialog over a list.
        testing::with_theme(Palette::DARK, |t| {
            let inner = Rect::new(40, 40, 280, 200);
            let ((), px) = testing::with_canvas(Size::new(320, 240), |c| {
                full().draw(c, inner, t);
            });
            for y in 0..240 {
                for x in 0..320 {
                    let inside = x >= inner.x0 && x < inner.x1 && y >= inner.y0 && y < inner.y1;
                    if !inside {
                        assert_eq!(px[(y * 320 + x) as usize], 0, "painted outside at {x},{y}");
                    }
                }
            }
        });
    }

}
