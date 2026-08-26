//! Screen furniture: the title bar, the softkey bar, scrollbars, avatars.
//!
//! We draw these ourselves rather than letting Avkon do it. The app is
//! constructed with `CEikAppUi::ENoScreenFurniture`, which removes the status
//! pane and the CBA — that hands us the whole 320x240 and, usefully, means
//! softkey presses arrive at our control instead of being eaten by Avkon's button
//! group.

use symbian_gfx::{Align, Canvas, Color, Point, Rect};

use crate::paint;
use crate::theme::Theme;

/// The three regions a screen is carved into. FP2 added a middle softkey, so the
/// bar holds three labels, not two.
#[derive(Copy, Clone, Debug)]
pub struct Frame {
    pub title: Rect,
    pub content: Rect,
    pub softkeys: Rect,
}

impl Frame {
    /// Split the screen. Pass `title: false` or `softkeys: false` for a screen
    /// that wants the space instead.
    pub fn split(screen: Rect, theme: &Theme<'_>, title: bool, softkeys: bool) -> Self {
        let m = &theme.metrics;
        let (title_r, rest) = if title {
            screen.split_top(m.title_h)
        } else {
            (Rect::EMPTY, screen)
        };
        let (sk_r, content) = if softkeys {
            rest.split_bottom(m.softkey_h)
        } else {
            (Rect::EMPTY, rest)
        };
        Self { title: title_r, content, softkeys: sk_r }
    }
}

/// Fill the screen background. Call before anything else.
pub fn clear(c: &mut Canvas<'_>, theme: &Theme<'_>) {
    let r = Rect::from_size(c.size());
    paint::band(c, r, &theme.palette.bg);
}

/// Title bar with an optional right-aligned detail, such as a connection state.
pub fn title_bar(c: &mut Canvas<'_>, r: Rect, theme: &Theme<'_>, title: &str, detail: Option<&str>) {
    if r.is_empty() {
        return;
    }
    let p = &theme.palette;
    // A band, not a fill: the light top edge and dark bottom edge are what make the
    // bar read as a layer above the content rather than as a differently-coloured
    // part of it. See crate::tokens for why that mattered on a skinnable platform.
    paint::band(c, r, &p.chrome);

    let inner = r.inset_xy(theme.metrics.pad, 0);
    let mut text_area = inner;
    if let Some(d) = detail {
        let w = theme.fonts.small.measure(d);
        let (right, left) = inner.split_right(w);
        c.draw_text_in(right, d, theme.fonts.small, p.dim, Align::End);
        // Keep a gap so a long title cannot butt against the detail.
        text_area = Rect { x1: left.x1 - theme.metrics.pad, ..left };
    }
    c.draw_text_in(text_area, title, theme.fonts.title, p.chrome_text, Align::Start);
}

/// The three slots of the softkey bar, in the order this SDK uses them.
///
/// # The convention
///
/// ```text
///   ┌──────────────────────────────────────────────┐
///   │  Options            Open            Back     │
///   └──────────────────────────────────────────────┘
///      left softkey    D-pad centre   right softkey
///      secondary       THE ACTION     way out
/// ```
///
/// **Middle is the action**, and it is not a softkey at all: S60 wires the centre of the D-pad to
/// the selection key, so it arrives as [`crate::Key::Select`], never as `Softkey::Middle`. Screens
/// therefore *label* the middle slot and handle `Select`. Getting this backwards is not a
/// theoretical mistake — a screen in this project bound its middle label to `Softkey::Middle`, the
/// arm never fired, and the key opened the highlighted row instead of doing what the label said.
///
/// **Left is options**: the secondary thing this screen offers — refresh, a menu, a mode switch.
/// Blank when there is nothing, which is common and fine.
///
/// **Right is back**, always, and only ever back or exit. It is the one key a user must be able to
/// press without reading, so it never becomes a second action key.
///
/// This mirrors what the native S60 applications do, which is the real argument for it: the phone
/// has trained its user for a decade, and a launcher that disagrees is the one that feels wrong.
pub struct Softkeys;

impl Softkeys {
    /// The standard arrangement: `(options, action, back)`.
    ///
    /// A named constructor rather than a bare array so the order cannot be transposed silently —
    /// `[a, b, c]` reads the same whichever meaning you had in mind, and the compiler cannot help.
    // Not a constructor for `Self`: `Softkeys` is a namespace here, and what this builds is the
    // three-slot array the bar is actually drawn from. The name is the point — see the doc above.
    #[allow(clippy::new_ret_no_self)]
    pub const fn new<'a>(
        options: Option<&'a str>,
        action: Option<&'a str>,
        back: Option<&'a str>,
    ) -> [Option<&'a str>; 3] {
        [options, action, back]
    }

    /// An action and a way out, with nothing on the left. The common shape.
    pub const fn action<'a>(action: &'a str, back: &'a str) -> [Option<&'a str>; 3] {
        [None, Some(action), Some(back)]
    }

    /// Only a way out — for a screen that reads rather than does.
    pub const fn back(back: &str) -> [Option<&str>; 3] {
        [None, None, Some(back)]
    }
}

/// Softkey bar. `labels` is left, middle, right; `None` leaves a slot blank.
///
/// Build them with [`Softkeys`], which documents what each slot means in this SDK and why the
/// middle one is the D-pad centre rather than a softkey.
///
/// The middle label is centred and the outer two hug their edges, which is what
/// S60 does and therefore what muscle memory expects.
pub fn softkey_bar(c: &mut Canvas<'_>, r: Rect, theme: &Theme<'_>, labels: [Option<&str>; 3]) {
    if r.is_empty() {
        return;
    }
    let p = &theme.palette;
    paint::band(c, r, &p.chrome);

    let inner = r.inset_xy(theme.metrics.pad, 0);
    let third = inner.width() / 3;
    if let Some(l) = labels[0] {
        let a = Rect { x1: inner.x0 + third, ..inner };
        c.draw_text_in(a, l, theme.fonts.small, p.chrome_text, Align::Start);
    }
    if let Some(m) = labels[1] {
        let a = Rect { x0: inner.x0 + third, x1: inner.x1 - third, ..inner };
        c.draw_text_in(a, m, theme.fonts.small, p.accent, Align::Center);
    }
    if let Some(rr) = labels[2] {
        let a = Rect { x0: inner.x1 - third, ..inner };
        c.draw_text_in(a, rr, theme.fonts.small, p.chrome_text, Align::End);
    }
}

/// Vertical scrollbar in the gutter at the right edge of `area`.
///
/// `thumb` is the `(y, height)` pair from [`crate::list::ListState::scrollbar`], in
/// `area`-relative coordinates. `None` means the content fits — and the bar is still
/// drawn, full height.
///
/// Drawing it either way is deliberate, and it is one of the things the era got
/// right that modern UIs dropped. On a screen showing five rows of a fifty-row list,
/// "where am I and how much is left" is not incidental information, and a bar that
/// appears only while scrolling answers the question exactly when you have stopped
/// needing it. A full-height thumb says "this is all of it" — which is an answer.
pub fn scrollbar(c: &mut Canvas<'_>, area: Rect, theme: &Theme<'_>, thumb: Option<(i32, i32)>) {
    let w = theme.metrics.scrollbar_w;
    if w <= 0 || area.is_empty() {
        return;
    }
    let track = Rect { x0: area.x1 - w, ..area };
    c.fill_rect(track, theme.palette.scrollbar_track);
    let (y, h) = thumb.unwrap_or((0, track.height()));
    c.fill_rect(
        Rect::new(track.x0, area.y0 + y, track.x1, area.y0 + y + h),
        theme.palette.scrollbar,
    );
}

/// The gutter a scrollbar occupies, so content can reserve it.
///
/// # Why this takes no "is it needed" flag
///
/// It used to, and the flag was ignored. The bar is always drawn (see [`scrollbar`]), so reserving
/// conditionally would make row text reflow the moment a list crossed the "fits" boundary — the same
/// list jumping sideways as one message arrives. Every caller in the SDK passed `true`, which is the
/// tell: an argument nobody varies and the callee never reads is not a flag, it is a comment that
/// looks like a decision. It is gone, so a caller can no longer believe it made one.
pub fn scrollbar_gutter(theme: &Theme<'_>) -> i32 {
    theme.metrics.scrollbar_w + 2
}

/// Selection highlight behind a focused list row.
///
/// Full-bleed, square-cornered, edge to edge. With no pointer, this band is the only
/// thing telling you where you are, so it should be the loudest object on screen — a
/// rounded inset pill reads as a button you press rather than as a cursor.
pub fn selection(c: &mut Canvas<'_>, r: Rect, theme: &Theme<'_>) {
    paint::highlight(c, r, &theme.palette.selection);
}

/// A round avatar with up to two initials, tinted deterministically from `seed`
/// so the same contact always gets the same colour across launches.
pub fn avatar(c: &mut Canvas<'_>, r: Rect, theme: &Theme<'_>, initials: &str, seed: u32) {
    let size = r.width().min(r.height());
    let box_ = Rect::from_xywh(r.x0, r.y0 + (r.height() - size) / 2, size, size);
    c.fill_round_rect(box_, size / 2, avatar_color(seed));

    // Two chars at most; more is unreadable at 32px.
    let mut buf = [0u8; 8];
    let mut len = 0;
    for ch in initials.chars().take(2) {
        let s = ch.encode_utf8(&mut buf[len..]);
        len += s.len();
    }
    let text = core::str::from_utf8(&buf[..len]).unwrap_or("");
    c.draw_text_in(box_, text, theme.fonts.strong, Color::WHITE, Align::Center);
}

/// A fixed spread of hues that all carry white text legibly. Picked by hand
/// rather than generated, because a random hue lands on yellow often enough to
/// matter and mid-yellow with white text is unreadable.
pub fn avatar_color(seed: u32) -> Color {
    const PALETTE: [u32; 8] = [
        0xC2504E, 0xB05CA8, 0x4E7FC2, 0x3E9A7A, 0xC28A3E, 0x7A5CC2, 0xC25C8A, 0x4E8AA8,
    ];
    Color::hex(PALETTE[(seed as usize) % PALETTE.len()])
}

/// A small filled pill, for unread counts and other badges. Returns its width so
/// the caller can lay text out to its left.
///
/// Colours are explicit rather than taken from the palette because the one place
/// this is used — an unread count on a list row — needs to invert when the row is
/// selected. The default accent pill on the accent-coloured selection fill is
/// almost invisible, which loses exactly the number the badge exists to show.
pub fn badge(
    c: &mut Canvas<'_>,
    right_edge: Point,
    theme: &Theme<'_>,
    text: &str,
    fill: Color,
    fg: Color,
) -> i32 {
    let f = theme.fonts.small;
    let tw = f.measure(text);
    let h = f.line_height() + 2;
    // Keep single digits circular rather than letting a 1 make a narrow slot.
    let w = (tw + 8).max(h);
    let r = Rect::from_xywh(right_edge.x - w, right_edge.y, w, h);
    c.fill_round_rect(r, h / 2, fill);
    c.draw_text_in(r, text, f, fg, Align::Center);
    w
}

/// The fill/text pair an unread badge should use on a row in the given state.
pub fn unread_colors(theme: &Theme<'_>, selected: bool) -> (Color, Color) {
    if selected {
        (theme.palette.selection_text, theme.palette.selection.mid())
    } else {
        (theme.palette.unread, theme.palette.unread_text)
    }
}

/// The three colours a control paints with, given whether it sits on the selection band.
///
/// Returns `(ground, ink, quiet)`: what is behind the control, the colour of its "on" state, and the
/// colour of its "off" state.
///
/// # Why every control needs this
///
/// A switch, a checkbox, a slider and a stepper all paint a mark on whatever the row is. Off the
/// band that is the page; on it, the selection surface — and the palette's `accent`, `dim` and `bg`
/// were all chosen against the *page*. Used unchanged on the band they are three colours picked for
/// a ground that is not there.
///
/// This was visible rather than theoretical. On `HIGH_CONTRAST`, whose selection band is white and
/// whose `dim` is also white, the focused row's switch became a black dot floating in nothing: the
/// track vanished into the band and only the knob survived. `DARK` and `S60` looked fine, which is
/// exactly why a five-palette sweep is worth having — the defect existed in every palette and was
/// only *visible* in one.
///
/// # Why it is one function and not four
///
/// Because four answers to "what colour goes on the band" is four chances to disagree, and the
/// disagreement would show as one control looking wrong beside three that look right. The same
/// argument [`unread_colors`] makes above, which is this function's precedent and the reason it lives
/// here rather than in each control's own file.
pub fn control_colors(theme: &Theme<'_>, selected: bool) -> (Color, Color, Color) {
    let p = &theme.palette;
    if selected {
        // On the band, the text colour is the one thing the palette guarantees is readable against it
        // — `Palette::check` requires 70 luma between the two. So the "on" mark takes it, and the
        // "off" mark is that colour pulled back toward the band, which keeps it visible without
        // competing with the on state.
        let band = p.selection.mid();
        (band, p.selection_text, p.selection_text.lerp(band, 128))
    } else {
        (p.bg.mid(), p.accent, p.dim)
    }
}

/// Centred message for an empty list or an error.
pub fn placeholder(c: &mut Canvas<'_>, area: Rect, theme: &Theme<'_>, text: &str) {
    let f = theme.fonts.body;
    let line = Rect::from_xywh(
        area.x0 + theme.metrics.pad,
        area.y0 + (area.height() - f.line_height()) / 2,
        area.width() - theme.metrics.pad * 2,
        f.line_height(),
    );
    c.draw_text_in(line, text, f, theme.palette.dim, Align::Center);
}

#[cfg(test)]
mod control_color_tests {
    use super::*;
    use crate::testing;
    use crate::theme::Palette;
    use crate::tokens::luma;

    #[test]
    fn a_control_can_be_seen_on_the_band_in_every_palette() {
        // The defect a person found on the handset. On `HIGH_CONTRAST` the selection band is white and
        // `dim` is also white, so a focused row's switch became a black dot floating in nothing — the
        // track vanished into the band and only the knob was left. It was wrong in every palette and
        // visible in one, which is the whole argument for sweeping all five.
        //
        // 40 is the same distance `Palette::check` demands between `dim` and the page: it is the same
        // question — how far apart two colours must be to be two colours.
        for (name, palette) in Palette::ALL {
            testing::with_theme(palette, |t| {
                for selected in [false, true] {
                    let (ground, ink, quiet) = control_colors(t, selected);
                    let d = |a: Color, b: Color| (luma(a) as i32 - luma(b) as i32).abs();
                    assert!(d(ink, ground) >= 40, "{name} selected={selected}: the on state vanishes");
                    assert!(
                        d(quiet, ground) >= 20,
                        "{name} selected={selected}: the off state vanishes"
                    );
                    // Channel distance, not luma, and the difference matters: `DARK`'s accent is a
                    // blue at luma 128 and its `dim` is a grey at 146 — eighteen apart by luma and
                    // obviously two colours on screen. Luma answers "can this be read on that"; it
                    // does not answer "are these two different marks", and using it for the second
                    // question failed this test on a palette that is fine.
                    let chan = |a: Color, b: Color| {
                        (a.r() as i32 - b.r() as i32).abs()
                            + (a.g() as i32 - b.g() as i32).abs()
                            + (a.b() as i32 - b.b() as i32).abs()
                    };
                    assert!(
                        chan(ink, quiet) >= 60,
                        "{name} selected={selected}: on and off are the same mark"
                    );
                }
            });
        }
    }

    #[test]
    fn the_band_and_the_page_get_different_answers() {
        // If they did not, this function would be a rename of the palette and the defect would still
        // be there — which is what it looked like before, four controls reading `accent`/`dim`/`bg`
        // whatever they were sitting on.
        testing::with_theme(Palette::DARK, |t| {
            assert_ne!(control_colors(t, false), control_colors(t, true));
        });
    }

    #[test]
    fn a_switch_on_the_band_is_not_a_dot_in_nothing() {
        // The specific picture, on the palette that showed it. Counting pixels of each colour: the
        // track has to be a different colour from the band it is on, or there is no track.
        use symbian_gfx::Size;
        let palette = Palette::HIGH_CONTRAST;
        let band = symbian_gfx::Rect::from_xywh(0, 0, 60, 38);
        let (_, buf) = testing::with_canvas(Size::new(60, 38), |c| {
            testing::with_theme(palette, |t| {
                selection(c, band, t);
                crate::toggle::draw_switch(c, crate::toggle::switch_track(band, t), t, false, true);
            });
        });
        let band_px = palette.selection.mid().to_rgb565().0;
        let track = testing::with_theme(palette, |t| crate::toggle::switch_track(band, t));
        let row = (track.y0 + track.height() / 2) as usize;
        let on_track: usize = (track.x0..track.x1)
            .filter(|&x| buf[row * 60 + x as usize] != band_px)
            .count();
        assert!(on_track > 10, "only {on_track} pixels of the track differ from the band");
    }

    #[test]
    fn a_focused_field_looks_different_from_a_sleeping_one_in_every_palette() {
        // The gap `docs/ui-catalog.md` recorded and a person then found on the handset: the band was
        // drawn identically either way, so the *only* difference was a one-pixel caret — and an empty
        // field that was never told it had focus draws no caret at all, which looks like a thin caret
        // rather than like a dead control.
        //
        // Counting differing pixels rather than naming a colour, so a future outline that changes for
        // a good reason does not turn this red.
        use crate::edit::TextField as Editor;
        let paint = |palette: crate::theme::Palette, focused: bool| {
            let mut ed = Editor::new();
            ed.set_text("aaa");
            let (_, buf) = testing::with_canvas(symbian_gfx::Size::new(160, 30), |c| {
                testing::with_theme(palette, |t| {
                    c.clear(palette.bg.mid());
                    text_field(
                        c,
                        Rect { x0: 0, y0: 0, x1: 160, y1: 24 },
                        t,
                        &ed,
                        FieldStyle { focused, ..Default::default() },
                    );
                });
            });
            buf
        };
        for (name, palette) in crate::theme::Palette::ALL {
            let (awake, asleep) = (paint(palette, true), paint(palette, false));
            let differing = awake.iter().zip(&asleep).filter(|(a, b)| a != b).count();
            // More than a caret's worth. A caret in this box is 1x18; an outline is the perimeter.
            assert!(
                differing > 40,
                "{name}: a focused field differs from a sleeping one by only {differing} pixels"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_softkey_helpers_place_each_job_in_its_own_slot() {
        // The order is the convention, and the whole reason these constructors exist: an array
        // literal reads the same whichever meaning the author had in mind, and the compiler cannot
        // tell you that you transposed two of them.
        assert_eq!(Softkeys::new(Some("Options"), Some("Open"), Some("Back")),
                   [Some("Options"), Some("Open"), Some("Back")]);
        // The action goes in the MIDDLE, which is the D-pad centre — never on the left.
        assert_eq!(Softkeys::action("Open", "Back"), [None, Some("Open"), Some("Back")]);
        // Back is always the right-hand slot, alone.
        assert_eq!(Softkeys::back("Back"), [None, None, Some("Back")]);
    }

    use super::*;
    use crate::theme::{Fonts, Theme};
    use symbian_gfx::{BitmapFont, Size};

    // A tiny valid atlas: one glyph, so measure() and line_height() are real.
    fn atlas() -> alloc::vec::Vec<u8> {
        let mut v = alloc::vec::Vec::new();
        v.extend_from_slice(b"SBF1");
        v.extend_from_slice(&12u16.to_le_bytes());
        v.extend_from_slice(&9i16.to_le_bytes());
        v.extend_from_slice(&3i16.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.push(1); // FLAG_AA
        v.push(5); // fallback advance
        v.extend_from_slice(&0u16.to_le_bytes());
        v.extend_from_slice(&(b'a' as u32).to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(&[4, 6, 5, 0]);
        v.extend_from_slice(&0i16.to_le_bytes());
        v.extend_from_slice(&6i16.to_le_bytes());
        v.extend(std::iter::repeat_n(0xFFu8, 24));
        v
    }

    #[test]
    fn frame_partitions_the_screen_exactly() {
        let data = atlas();
        let f = BitmapFont::new(&data).unwrap();
        let fonts = Fonts { body: &f, strong: &f, small: &f, title: &f };
        let t = Theme::dark(fonts);
        let screen = Rect::from_size(Size::new(320, 240));

        let fr = Frame::split(screen, &t, true, true);
        assert_eq!(fr.title.height(), t.metrics.title_h);
        assert_eq!(fr.softkeys.height(), t.metrics.softkey_h);
        assert_eq!(fr.title.y1, fr.content.y0);
        assert_eq!(fr.content.y1, fr.softkeys.y0);
        assert_eq!(
            fr.title.height() + fr.content.height() + fr.softkeys.height(),
            240
        );
        // The chrome must leave whole rows: no half-row peeking at the bottom
        // edge, which reads as a rendering fault rather than as scrollable content.
        let content = 240 - t.metrics.title_h - t.metrics.softkey_h;
        assert_eq!(fr.content.height(), content);
        assert!(
            content / t.metrics.row_h >= 5,
            "only {} rows fit in {content}px; the list looks empty on a 240px screen",
            content / t.metrics.row_h
        );
    }

    #[test]
    fn frame_gives_the_space_back_when_chrome_is_off() {
        let data = atlas();
        let f = BitmapFont::new(&data).unwrap();
        let fonts = Fonts { body: &f, strong: &f, small: &f, title: &f };
        let t = Theme::dark(fonts);
        let screen = Rect::from_size(Size::new(320, 240));

        let fr = Frame::split(screen, &t, false, false);
        assert_eq!(fr.content, screen);
        assert!(fr.title.is_empty());
        assert!(fr.softkeys.is_empty());
    }

    #[test]
    fn avatar_colour_is_stable_and_in_range() {
        for seed in 0..64u32 {
            let a = avatar_color(seed);
            assert_eq!(a, avatar_color(seed), "must be deterministic");
            assert!(a.is_opaque());
        }
    }

    #[test]
    fn drawing_chrome_into_a_zero_sized_rect_is_a_no_op() {
        let data = atlas();
        let f = BitmapFont::new(&data).unwrap();
        let fonts = Fonts { body: &f, strong: &f, small: &f, title: &f };
        let t = Theme::dark(fonts);

        let mut buf = alloc::vec![0u16; 320 * 240];
        let mut c = symbian_gfx::Canvas::from_slice(&mut buf, Size::new(320, 240));
        title_bar(&mut c, Rect::EMPTY, &t, "x", None);
        softkey_bar(&mut c, Rect::EMPTY, &t, [Some("a"), None, Some("b")]);
        scrollbar(&mut c, Rect::EMPTY, &t, Some((0, 10)));
        scrollbar(&mut c, Rect::EMPTY, &t, None);
        selection(&mut c, Rect::EMPTY, &t);
        assert!(buf.iter().all(|&p| p == 0), "nothing should have been drawn");
    }
}

#[cfg(test)]
extern crate alloc;

/// What dresses a text field: what stands before the text, what stands in for it when empty.
///
/// A struct rather than four arguments, because three of the four are usually absent and a call site
/// reading `text_field(c, r, t, &f, None, None, true)` says nothing about which `None` is which.
#[derive(Copy, Clone, Debug, Default)]
pub struct FieldStyle<'a> {
    /// Drawn dimmed before the text and not part of it — the fixed `+` of a phone number, which the
    /// field must not store or a paste would end up with two of them.
    pub prefix: Option<&'a str>,
    /// Shown dimmed while the field is empty.
    pub placeholder: Option<&'a str>,
    /// Only a focused field shows a caret. A screen with one field passes `true`; a screen with two
    /// passes it to the one holding the keyboard, which is what stops both from looking active.
    pub focused: bool,
}

/// A one-line editor: its box, its text or its mask, its selection, its caret.
///
/// # Why this is here and not in two places
///
/// It was in two places. `tg`'s login screens drew a field by hand — band, prefix, selection, caret
/// — and `symbian-decl-ui`'s `TextField` widget drew a different one: a stroked rectangle, no
/// prefix, no selection painting, a caret centred rather than inset. Both were reasonable and they
/// could never agree, which means the declarative login screen could never have been compared with
/// the one it replaces. Two drawings of one control is the same defect as two routings of one key.
///
/// So the pixels live here, next to the rest of the furniture, and the callers bring a rect and a
/// [`crate::edit::TextField`]. The *placement* stays with the caller: a login screen centres its
/// field in the panel and a composer pins it to the bottom, and neither is this function's business.
///
/// # The geometry, and why these numbers
///
/// They are the login screen's, because that is what ships on the phone: the text is inset 6 pixels
/// from the left edge, the line box sits 3 pixels below the top, and the caret runs from there to 3
/// pixels above the bottom. The field's *height* is `body.line_height() + space.snug * 2`, which the
/// declarative widget already measured to and the login screen already spelled as `+ 8` — the same
/// number, now named once.
///
/// The selection is painted before the text so the characters land on top of it. A selection nobody
/// can see is worse than none: the next keystroke replaces text the user did not know was chosen.
pub fn text_field(
    c: &mut Canvas<'_>,
    r: Rect,
    theme: &Theme<'_>,
    field: &crate::edit::TextField,
    style: FieldStyle<'_>,
) {
    let p = &theme.palette;
    let body = theme.fonts.body;
    let (top, bottom) = (r.y0 + 3, r.y1 - 3);

    paint::band(c, r, &p.chrome);
    // A focused field has to look different from an unfocused one somewhere other than in a caret.
    //
    // It did not, and the cost was two real bugs: `FieldRow` lights its *caption* in the accent from
    // a flag the control never sees, so a form whose field was never told it had focus showed a lit
    // caption over a dead box — and the only true signal on screen was the *absence* of a one-pixel
    // caret, which reads as a thin caret rather than as a broken field. A person found it on the
    // handset; the host had nothing to say. `docs/ui-catalog.md` had recorded the gap already.
    //
    // An outline rather than a different fill: the fill is `chrome`, which the palette authored to
    // hold text, and a second authored fill would be a sixth colour per palette to keep in step. The
    // outline is drawn *on* the band's edge and not inside it, so nothing below moves by a pixel
    // when a field takes the cursor — a form that shifted as the cursor walked it would be a worse
    // defect than the one this fixes.

    if style.focused {
        c.stroke_rect(r, p.accent);
    }

    let mut text_x = r.x0 + 6;
    if let Some(pre) = style.prefix {
        c.draw_text(Point::new(text_x, top + body.ascent()), pre, body, p.dim);
        text_x += body.measure(pre) + 2;
    }

    // `display()` rather than `text()`: it is the one call that hides a password, so asking for it
    // here means no drawing path can leak one.
    let display = field.display();
    let right = r.x1 - 4;

    if display.is_empty() {
        if let Some(ph) = style.placeholder.filter(|ph| !ph.is_empty()) {
            c.draw_text(Point::new(text_x, top + body.ascent()), ph, body, p.dim);
        }
        if style.focused {
            c.fill_rect(Rect::new(text_x, top, text_x + 1, bottom), p.accent);
        }
    } else {
        // Horizontal scroll so the caret is always visible: measure where the caret sits from the
        // start of the text, and if that is past the visible width, shift the whole run left by the
        // overflow so the caret rides the right edge. Stateless — recomputed every frame from the
        // caret position — which is why a long URL no longer just truncates with an ellipsis and
        // hides the end the user is typing at. `display_offset` maps the caret's byte offset onto
        // the (possibly masked) display string.
        let caret_off = field.display_offset(field.cursor()).min(display.len());
        let caret_px = body.measure(&display[..caret_off]);
        let avail = (right - text_x).max(1);
        let scroll = (caret_px - avail).max(0);
        let base_x = text_x - scroll;

        // Clip to the content area so text scrolled off the left does not paint over the prefix or
        // outside the field's band.
        let saved = c.save();
        c.clip_to(Rect::new(text_x, r.y0, right, r.y1));

        if let Some((from, to)) = field.selection() {
            paint::text_selection(
                c,
                base_x,
                top,
                top + body.line_height(),
                &display,
                field.display_offset(from),
                field.display_offset(to),
                body,
                p.selection.mid(),
            );
        }
        c.draw_text(Point::new(base_x, top + body.ascent()), &display, body, p.text);

        if style.focused {
            let cx = base_x + caret_px;
            c.fill_rect(Rect::new(cx, top, cx + 1, bottom), p.accent);
        }
        c.restore(saved);
    }
}

/// The height [`text_field`] draws into, so a caller can place it without guessing.
pub fn text_field_height(theme: &Theme<'_>) -> i32 {
    theme.fonts.body.line_height() + theme.metrics.space.snug * 2
}
