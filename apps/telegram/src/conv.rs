//! The conversation view: a bubble transcript with a composer beneath it.

use alloc::vec::Vec;

use symbian_ui::{
    chrome, Align, Canvas, Frame, Handled, Key, KeyEvent, ListState, Point, Rect, Rows,
    Softkey, TextField, Theme,
};

use crate::model::{Chat, Delivery};

/// One wrapped line, as a byte range into the message text.
struct Line {
    start: usize,
    end: usize,
    width: i32,
}

/// A message with its wrapping resolved for one specific width.
struct Laid {
    lines: Vec<Line>,
    bubble_w: i32,
    height: i32,
    /// True when the timestamp shares the last text line instead of getting its
    /// own. Saves a whole line of vertical space on most messages, which matters
    /// when the transcript viewport is 180px tall.
    time_inline: bool,
    /// Trailing space below this bubble; smaller when the next message is from the
    /// same sender.
    gap: i32,
}

/// Wrapping is expensive enough that we do it once per (width, message-count)
/// rather than per frame — a 6-message chat at 320px is ~40 `measure` calls, and
/// redraw happens on every keystroke.
pub struct Transcript {
    laid: Vec<Laid>,
    width: i32,
    count: usize,
}

const BUBBLE_HPAD: i32 = 6;
const BUBBLE_VPAD: i32 = 3;
/// Gap between messages from different senders.
const BUBBLE_GAP: i32 = 5;
/// Gap between consecutive messages from the same sender. Grouping them tightly
/// is what makes a transcript read as a conversation rather than a list.
const BUBBLE_GAP_SAME: i32 = 2;
/// Bubbles stop at this fraction of the width so the other party's alignment stays
/// legible even for a long message.
const BUBBLE_MAX_PCT: i32 = 74;

impl Transcript {
    pub fn build(chat: &Chat, theme: &Theme<'_>, avail_w: i32) -> Self {
        let body = theme.fonts.body;
        let small = theme.fonts.small;
        let max_bubble = (avail_w * BUBBLE_MAX_PCT / 100).max(40);
        let inner_max = max_bubble - BUBBLE_HPAD * 2;

        let mut laid = Vec::with_capacity(chat.messages.len());
        for (idx, m) in chat.messages.iter().enumerate() {
            let same_sender = idx > 0 && chat.messages[idx - 1].outgoing == m.outgoing;
            let gap = if same_sender { BUBBLE_GAP_SAME } else { BUBBLE_GAP };
            let mut lines: Vec<Line> = Vec::new();
            body.wrap(&m.text, inner_max, &mut |line: &str| {
                let start = line.as_ptr() as usize - m.text.as_ptr() as usize;
                lines.push(Line { start, end: start + line.len(), width: body.measure(line) });
            });
            if lines.is_empty() {
                lines.push(Line { start: 0, end: 0, width: 0 });
            }

            // Width of the trailing metadata: time, plus ticks when outgoing.
            let meta_w = small.measure(&m.time)
                + if m.outgoing { small.measure(m.state.glyph()) + 3 } else { 0 };

            let widest = lines.iter().map(|l| l.width).max().unwrap_or(0);
            let last_w = lines.last().map(|l| l.width).unwrap_or(0);

            // Try to tuck the timestamp onto the last line.
            let inline_need = last_w + 6 + meta_w;
            let time_inline = inline_need <= inner_max;
            let (content_w, extra_h) = if time_inline {
                (widest.max(inline_need), 0)
            } else {
                (widest.max(meta_w), small.line_height())
            };

            let height = BUBBLE_VPAD * 2
                + lines.len() as i32 * body.line_height()
                + extra_h
                + gap;

            laid.push(Laid {
                lines,
                bubble_w: (content_w + BUBBLE_HPAD * 2).min(max_bubble),
                height,
                time_inline,
                gap,
            });
        }

        Self { laid, width: avail_w, count: chat.messages.len() }
    }

    /// True when the cached layout no longer matches what we are about to draw.
    pub fn is_stale(&self, chat: &Chat, avail_w: i32) -> bool {
        self.width != avail_w || self.count != chat.messages.len()
    }
}

impl Rows for Transcript {
    fn len(&self) -> usize {
        self.laid.len()
    }
    fn height(&self, index: usize) -> i32 {
        self.laid[index].height
    }
}

/// Which half of the screen has focus. The composer only receives characters when
/// it is focused, so the D-pad stays available for scrolling the transcript.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Focus {
    Transcript,
    Composer,
}

pub struct Conversation {
    pub chat: usize,
    pub state: ListState,
    pub composer: TextField,
    pub focus: Focus,
    transcript: Option<Transcript>,
}

pub enum ConvAction {
    Back,
    Send(alloc::string::String),
    None,
}

impl Conversation {
    pub fn new(chat: usize) -> Self {
        Self {
            chat,
            state: ListState::new(),
            // Telegram's own limit; also stops one message from wrapping forever.
            composer: TextField::with_limit(4096),
            focus: Focus::Composer,
            transcript: None,
        }
    }

    fn composer_h(&self, theme: &Theme<'_>) -> i32 {
        theme.fonts.body.line_height() + 8
    }

    /// Re-wrap if the width or the message count changed, then hand back the
    /// layout along with the transcript viewport height.
    fn ensure_layout(&mut self, chat: &Chat, theme: &Theme<'_>, screen: Rect) -> (Rect, Rect) {
        let frame = Frame::split(screen, theme, true, true);
        let (composer, transcript_area) = frame.content.split_bottom(self.composer_h(theme));
        let avail = transcript_area.width() - theme.metrics.pad * 2;

        let stale = self.transcript.as_ref().is_none_or(|t| t.is_stale(chat, avail));
        if stale {
            let t = Transcript::build(chat, theme, avail);
            // A fresh conversation opens at the newest message, which is what a
            // chat client must always do.
            let first_build = self.transcript.is_none();
            self.transcript = Some(t);
            if first_build {
                let t = self.transcript.as_ref().unwrap();
                self.state.scroll_to_end(t, transcript_area.height());
            } else {
                let t = self.transcript.as_ref().unwrap();
                self.state.clamp(t, transcript_area.height());
            }
        }
        (transcript_area, composer)
    }

    pub fn handle_key(
        &mut self,
        ev: KeyEvent,
        chat: &Chat,
        theme: &Theme<'_>,
        screen: Rect,
    ) -> (Handled, ConvAction) {
        let (area, _) = self.ensure_layout(chat, theme, screen);
        let vp = area.height();

        match ev.key {
            Key::Softkey(Softkey::Right) => return (Handled::Consumed, ConvAction::Back),
            Key::Softkey(Softkey::Middle) | Key::Enter | Key::Call => {
                if !self.composer.is_empty() {
                    return (Handled::Consumed, ConvAction::Send(self.composer.take()));
                }
                return (Handled::Consumed, ConvAction::None);
            }
            // Up out of the composer moves focus into the transcript; Down at the
            // end of the transcript hands focus back.
            Key::Up if self.focus == Focus::Composer => {
                self.focus = Focus::Transcript;
                return (Handled::Consumed, ConvAction::None);
            }
            Key::Down if self.focus == Focus::Transcript => {
                let t = self.transcript.as_ref().unwrap();
                if self.state.selected + 1 >= t.len() {
                    self.focus = Focus::Composer;
                    return (Handled::Consumed, ConvAction::None);
                }
            }
            _ => {}
        }

        let handled = match self.focus {
            Focus::Composer => self.composer.handle_key(ev),
            Focus::Transcript => {
                let t = self.transcript.as_ref().unwrap();
                self.state.handle_key(ev, t, vp)
            }
        };
        (handled, ConvAction::None)
    }

    pub fn draw(&mut self, c: &mut Canvas<'_>, chat: &Chat, theme: &Theme<'_>) {
        let screen = Rect::from_size(c.size());
        let (area, composer_r) = self.ensure_layout(chat, theme, screen);
        let frame = Frame::split(screen, theme, true, true);
        let t = self.transcript.as_ref().unwrap();
        let p = &theme.palette;

        chrome::clear(c, theme);
        chrome::title_bar(c, frame.title, theme, &chat.name, None);

        let bar = self.state.scrollbar(t, area.height());
        let gutter = chrome::scrollbar_gutter(theme, bar.is_some());
        // A short conversation hangs from the composer rather than the title bar.
        // Every chat client does this, and the alternative — a wall of empty space
        // below two messages — reads as a rendering fault.
        let anchor = (area.height() - ListState::content_height(t)).max(0);
        let body = Rect {
            x0: area.x0 + theme.metrics.pad,
            y0: area.y0 + anchor,
            x1: area.x1 - theme.metrics.pad - gutter,
            ..area
        };

        let saved = c.save();
        c.clip_to(area);
        self.state.for_visible(t, body, |i, r| {
            let focused = self.focus == Focus::Transcript && i == self.state.selected;
            draw_bubble(c, r, theme, &chat.messages[i], &t.laid[i], focused);
        });
        c.restore(saved);
        chrome::scrollbar(c, area, theme, bar);

        // Composer
        c.hline(composer_r.y0, composer_r.x0, composer_r.x1, p.divider);
        symbian_ui::paint::band(c, Rect { y0: composer_r.y0 + 1, ..composer_r }, &p.chrome);
        let field = composer_r.inset_xy(theme.metrics.pad, 3);
        let focused = self.focus == Focus::Composer;

        if self.composer.is_empty() {
            c.draw_text_in(field, "Mensagem…", theme.fonts.body, p.dim, Align::Start);
        } else {
            c.draw_text_in(field, self.composer.text(), theme.fonts.body, p.text, Align::Start);
        }
        if focused {
            // Caret at the measured width of the text before the cursor, so it
            // tracks the insertion point rather than always sitting at the end.
            let before = &self.composer.text()[..self.composer.cursor()];
            let cx = field.x0 + theme.fonts.body.measure(before);
            c.fill_rect(Rect::new(cx, field.y0, cx + 1, field.y1), p.accent);
        }

        let send = if self.composer.is_empty() { None } else { Some("Enviar") };
        chrome::softkey_bar(c, frame.softkeys, theme, [Some("Opções"), send, Some("Voltar")]);
    }
}

fn draw_bubble(
    c: &mut Canvas<'_>,
    row: Rect,
    theme: &Theme<'_>,
    m: &crate::model::Message,
    laid: &Laid,
    focused: bool,
) {
    let p = &theme.palette;
    let body = theme.fonts.body;
    let small = theme.fonts.small;

    let h = row.height() - laid.gap;
    let bubble = if m.outgoing {
        Rect::from_xywh(row.x1 - laid.bubble_w, row.y0, laid.bubble_w, h)
    } else {
        Rect::from_xywh(row.x0, row.y0, laid.bubble_w, h)
    };

    let (fill, fg): (symbian_ui::Surface, _) = if m.outgoing {
        (p.bubble_out, p.bubble_out_text)
    } else {
        (p.bubble_in, p.bubble_in_text)
    };
    symbian_ui::paint::band_round(c, bubble, &fill, theme.metrics.radius);
    if focused {
        // Outline rather than a fill swap, so the incoming/outgoing colour still
        // reads while the row has focus.
        c.stroke_rect(bubble, p.accent);
    }

    let inner = bubble.inset_xy(BUBBLE_HPAD, BUBBLE_VPAD);
    let mut y = inner.y0;
    for l in &laid.lines {
        let text = &m.text[l.start..l.end];
        c.draw_text(Point::new(inner.x0, y + body.ascent()), text, body, fg);
        y += body.line_height();
    }

    // Metadata: dimmed against the bubble fill rather than the page background.
    let meta_col = if m.outgoing { p.accent_text.with_alpha(0xB0) } else { p.dim };
    let meta_y = if laid.time_inline {
        inner.y0 + (laid.lines.len() as i32 - 1) * body.line_height()
    } else {
        y
    };
    let meta_row = Rect::new(inner.x0, meta_y, inner.x1, meta_y + body.line_height());

    let mut right = inner.x1;
    if m.outgoing {
        let g = m.state.glyph();
        let gw = small.measure(g);
        let col = match m.state {
            Delivery::Failed => p.unread,
            Delivery::Read => p.accent_text,
            _ => meta_col,
        };
        c.draw_text_in(Rect { x0: right - gw, ..meta_row }, g, small, col, Align::End);
        right -= gw + 3;
    }
    let tw = small.measure(&m.time);
    c.draw_text_in(
        Rect { x0: right - tw, x1: right, ..meta_row },
        &m.time,
        small,
        meta_col,
        Align::End,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Store;
    use symbian_ui::{BitmapFont, Fonts, Size};

    fn theme_with<'a>(f: &'a BitmapFont<'a>) -> Theme<'a> {
        Theme::dark(Fonts { body: f, strong: f, small: f, title: f })
    }

    fn atlas() -> alloc::vec::Vec<u8> {
        // Every glyph 6x8, advance 6, so widths are trivially predictable.
        let chars: alloc::vec::Vec<char> = (0x20u32..0x500)
            .filter_map(char::from_u32)
            .collect();
        let mut idx = alloc::vec::Vec::new();
        let mut blob = alloc::vec::Vec::new();
        for ch in &chars {
            idx.extend_from_slice(&(*ch as u32).to_le_bytes());
            idx.extend_from_slice(&(blob.len() as u32).to_le_bytes());
            idx.extend_from_slice(&[6, 8, 6, 0]);
            idx.extend_from_slice(&0i16.to_le_bytes());
            idx.extend_from_slice(&8i16.to_le_bytes());
            blob.extend(core::iter::repeat(0x80u8).take(48));
        }
        let mut v = alloc::vec::Vec::new();
        v.extend_from_slice(b"SBF1");
        v.extend_from_slice(&12u16.to_le_bytes());
        v.extend_from_slice(&9i16.to_le_bytes());
        v.extend_from_slice(&3i16.to_le_bytes());
        v.extend_from_slice(&(chars.len() as u16).to_le_bytes());
        v.push(1);
        v.push(6);
        v.extend_from_slice(&0u16.to_le_bytes());
        v.extend_from_slice(&idx);
        v.extend_from_slice(&blob);
        v
    }

    #[test]
    fn every_line_fits_inside_its_bubble() {
        let data = atlas();
        let f = BitmapFont::new(&data).unwrap();
        let t = theme_with(&f);
        let store = Store::mock();
        let avail = 320 - t.metrics.pad * 2;

        for chat in &store.chats {
            let tr = Transcript::build(chat, &t, avail);
            for (m, l) in chat.messages.iter().zip(&tr.laid) {
                let inner = l.bubble_w - BUBBLE_HPAD * 2;
                for line in &l.lines {
                    assert!(
                        line.width <= inner,
                        "line {:?} is {}px in a {}px bubble",
                        &m.text[line.start..line.end],
                        line.width,
                        inner
                    );
                }
                assert!(l.bubble_w <= avail * BUBBLE_MAX_PCT / 100 + 1);
            }
        }
    }

    #[test]
    fn wrapping_preserves_every_character() {
        let data = atlas();
        let f = BitmapFont::new(&data).unwrap();
        let t = theme_with(&f);
        let store = Store::mock();
        let tr = Transcript::build(&store.chats[0], &t, 200);

        for (m, l) in store.chats[0].messages.iter().zip(&tr.laid) {
            let joined: alloc::string::String =
                l.lines.iter().map(|ln| &m.text[ln.start..ln.end]).collect();
            let expect: alloc::string::String =
                m.text.chars().filter(|c| *c != ' ' && *c != '\n').collect();
            let got: alloc::string::String = joined.chars().filter(|c| *c != ' ').collect();
            assert_eq!(got, expect, "text lost while wrapping {:?}", m.text);
        }
    }

    #[test]
    fn transcript_opens_at_the_newest_message() {
        let data = atlas();
        let f = BitmapFont::new(&data).unwrap();
        let t = theme_with(&f);
        let store = Store::mock();
        let mut conv = Conversation::new(0);
        let screen = Rect::from_size(Size::new(320, 240));
        conv.ensure_layout(&store.chats[0], &t, screen);

        let tr = conv.transcript.as_ref().unwrap();
        assert_eq!(conv.state.selected, tr.len() - 1);
        // And is scrolled as far as the content allows.
        let vp = Frame::split(screen, &t, true, true)
            .content
            .split_bottom(conv.composer_h(&t))
            .1
            .height();
        let expect = (ListState::content_height(tr) - vp).max(0);
        assert_eq!(conv.state.scroll, expect);
    }

    #[test]
    fn right_softkey_goes_back_and_middle_sends() {
        let data = atlas();
        let f = BitmapFont::new(&data).unwrap();
        let t = theme_with(&f);
        let store = Store::mock();
        let screen = Rect::from_size(Size::new(320, 240));
        let mut conv = Conversation::new(0);

        let (_, a) = conv.handle_key(KeyEvent::new(Key::Softkey(Softkey::Right)), &store.chats[0], &t, screen);
        assert!(matches!(a, ConvAction::Back));

        // Empty composer: middle softkey must not emit an empty message.
        let (_, a) = conv.handle_key(KeyEvent::new(Key::Softkey(Softkey::Middle)), &store.chats[0], &t, screen);
        assert!(matches!(a, ConvAction::None));

        for ch in "oi".chars() {
            conv.handle_key(KeyEvent::new(Key::Char(ch)), &store.chats[0], &t, screen);
        }
        let (_, a) = conv.handle_key(KeyEvent::new(Key::Softkey(Softkey::Middle)), &store.chats[0], &t, screen);
        match a {
            ConvAction::Send(s) => assert_eq!(s, "oi"),
            _ => panic!("expected a send"),
        }
        assert!(conv.composer.is_empty(), "composer must clear after sending");
    }

    #[test]
    fn focus_moves_between_composer_and_transcript() {
        let data = atlas();
        let f = BitmapFont::new(&data).unwrap();
        let t = theme_with(&f);
        let store = Store::mock();
        let screen = Rect::from_size(Size::new(320, 240));
        let mut conv = Conversation::new(0);

        assert_eq!(conv.focus, Focus::Composer);
        conv.handle_key(KeyEvent::new(Key::Up), &store.chats[0], &t, screen);
        assert_eq!(conv.focus, Focus::Transcript);
        // Already on the last message, so Down hands focus straight back.
        conv.handle_key(KeyEvent::new(Key::Down), &store.chats[0], &t, screen);
        assert_eq!(conv.focus, Focus::Composer);
    }

    #[test]
    fn typing_while_the_transcript_has_focus_does_not_edit_the_composer() {
        let data = atlas();
        let f = BitmapFont::new(&data).unwrap();
        let t = theme_with(&f);
        let store = Store::mock();
        let screen = Rect::from_size(Size::new(320, 240));
        let mut conv = Conversation::new(0);
        conv.focus = Focus::Transcript;
        conv.handle_key(KeyEvent::new(Key::Char('x')), &store.chats[0], &t, screen);
        assert!(conv.composer.is_empty());
    }

    #[test]
    fn relayout_after_a_new_message_keeps_scroll_valid() {
        let data = atlas();
        let f = BitmapFont::new(&data).unwrap();
        let t = theme_with(&f);
        let mut store = Store::mock();
        let screen = Rect::from_size(Size::new(320, 240));
        let mut conv = Conversation::new(0);
        conv.ensure_layout(&store.chats[0], &t, screen);

        store.chats[0].messages.truncate(1);
        conv.ensure_layout(&store.chats[0], &t, screen);
        let tr = conv.transcript.as_ref().unwrap();
        assert_eq!(tr.len(), 1);
        assert!(conv.state.scroll >= 0);
        assert!(conv.state.selected < tr.len());
    }
}
