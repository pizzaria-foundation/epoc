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
/// `_needed` is ignored: the bar is always drawn (see [`scrollbar`]), so reserving
/// conditionally would make row text reflow the moment a list crossed the "fits"
/// boundary — the same list jumping sideways as one message arrives.
pub fn scrollbar_gutter(theme: &Theme<'_>, _needed: bool) -> i32 {
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
        v.extend(core::iter::repeat(0xFFu8).take(24));
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
        drop(c);
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

    let mut text_x = r.x0 + 6;
    if let Some(pre) = style.prefix {
        c.draw_text(Point::new(text_x, top + body.ascent()), pre, body, p.dim);
        text_x += body.measure(pre) + 2;
    }

    // `display()` rather than `text()`: it is the one call that hides a password, so asking for it
    // here means no drawing path can leak one.
    let display = field.display();
    if display.is_empty() {
        if let Some(ph) = style.placeholder.filter(|ph| !ph.is_empty()) {
            c.draw_text(Point::new(text_x, top + body.ascent()), ph, body, p.dim);
        }
    } else {
        if let Some((from, to)) = field.selection() {
            paint::text_selection(
                c,
                text_x,
                top,
                top + body.line_height(),
                &display,
                field.display_offset(from),
                field.display_offset(to),
                body,
                p.selection.mid(),
            );
        }
        c.draw_text_in(
            Rect::new(text_x, top, r.x1 - 4, top + body.line_height()),
            &display,
            body,
            p.text,
            Align::Start,
        );
    }

    if style.focused {
        // `display_offset` is the conversion a masked field needs: the caret is a byte offset into
        // the password and the display is one `*` per character, so measuring the real prefix would
        // put the caret wherever the password happened to be wider than its stars.
        let shown = &display[..field.display_offset(field.cursor()).min(display.len())];
        let cx = text_x + body.measure(shown);
        c.fill_rect(Rect::new(cx, top, cx + 1, bottom), p.accent);
    }
}

/// The height [`text_field`] draws into, so a caller can place it without guessing.
pub fn text_field_height(theme: &Theme<'_>) -> i32 {
    theme.fonts.body.line_height() + theme.metrics.space.snug * 2
}
