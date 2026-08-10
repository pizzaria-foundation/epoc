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
    /// Extra height for a media placeholder, above the text. Includes [`MEDIA_GAP`].
    media_h: i32,
}

impl Laid {
    /// Height of the media band itself, without the gap under it.
    fn media_band_h(&self) -> i32 {
        (self.media_h - MEDIA_GAP).max(0)
    }

    /// Where the timestamp row starts, measured from the bubble's inner top.
    ///
    /// A method rather than two expressions inside the drawing, because it has to agree
    /// with where the text was actually left — and it did not. The inline branch counted
    /// text lines from `inner.y0` and forgot the media band above them, so on a media
    /// message with little or no text the timestamp and the delivery tick were drawn
    /// straight over the label. Every photo, voice and sticker row showed it.
    fn meta_offset(&self, line_height: i32) -> i32 {
        let lines = self.lines.len() as i32;
        if self.time_inline {
            // Shares the last text line.
            self.media_h + (lines - 1).max(0) * line_height
        } else {
            // Its own line, under everything.
            self.media_h + lines * line_height
        }
    }
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
/// Padding above and below the media placeholder's own label.
const MEDIA_VPAD: i32 = 3;
/// Space between the placeholder and the text under it.
const MEDIA_GAP: i32 = 2;

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

            // The placeholder band plus the gap below it. One number, read by both the
            // layout and the drawing, so they cannot drift apart.
            let media_h = if m.media.is_some() {
                body.line_height() + MEDIA_VPAD * 2 + MEDIA_GAP
            } else {
                0
            };

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
                + gap
                + media_h;

            laid.push(Laid {
                lines,
                bubble_w: (content_w + BUBBLE_HPAD * 2).min(max_bubble),
                height,
                time_inline,
                gap,
                media_h,
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
    /// Set briefly when the user taps a media message, so the screen can show feedback.
    pub note: Option<alloc::string::String>,
}

pub enum ConvAction {
    Back,
    Send(alloc::string::String),
    /// Scroll reached the top of the transcript; request the page above.
    LoadMore,
    /// The user pressed Select on a message with media.
    OpenMedia(usize),
    /// Re-fetch the newest page of this conversation.
    Refresh,
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
            note: None,
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
            // Save which message is at the top of the viewport so we can restore it
            // after the rebuild. Without this, new messages prepended by lazy loading
            // shift everything down and the user jumps to a different point.
            let saved_top = self.transcript.as_ref()
                .and_then(|t| ListState::row_at(t, self.state.scroll));
            let saved_selected = self.state.selected;

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
                // Restore the position: find the same message index in the new layout
                // and set scroll to its top. The selected index is restored as well.
                if let Some(idx) = saved_top {
                    if idx < t.len() {
                        self.state.selected = saved_selected.min(t.len().saturating_sub(1));
                        self.state.scroll = ListState::row_top(t, self.state.selected);
                    } else {
                        self.state.clamp(t, transcript_area.height());
                    }
                } else {
                    self.state.clamp(t, transcript_area.height());
                }
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
            // Re-fetch this conversation. Nothing here is pushed, so a reply that arrived
            // while the screen was open is invisible until something asks for it.
            Key::Softkey(Softkey::Left) => {
                self.note = Some(alloc::string::String::from("atualizando…"));
                return (Handled::Consumed, ConvAction::Refresh);
            }
            Key::Softkey(Softkey::Middle) | Key::Enter | Key::Call | Key::Select => {
                // In Transcript focus, open media or ignore — never send text.
                if self.focus == Focus::Transcript {
                    if let Some(msg) = chat.messages.get(self.state.selected) {
                        if let Some(media) = &msg.media {
                            let label = media_label(media, theme.fonts.body);
                            self.note = Some(alloc::format!("abrindo {label}…"));
                            return (Handled::Consumed, ConvAction::OpenMedia(self.state.selected));
                        }
                    }
                    return (Handled::Consumed, ConvAction::None);
                }
                // Composer focus: send if not empty.
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
            // Up at the top of the transcript asks for older messages. Checked
            // before the focus dispatch so ListState's handle_key — which would
            // consume it without moving — does not shadow the trigger.
            Key::Up
                if self.focus == Focus::Transcript
                    && self.state.selected == 0
                    && self.state.scroll == 0 =>
            {
                // The window holds the newest hundred and drops the rest, so above them
                // there is nothing to fetch — asking would spend a request on a page that
                // is discarded on arrival. Say so instead of appearing to hang.
                if chat.windowed && !chat.complete {
                    self.note = Some(alloc::string::String::from("inicio do que esta guardado"));
                    return (Handled::Consumed, ConvAction::None);
                }
                return (Handled::Consumed, ConvAction::LoadMore);
            }
            Key::Down if self.focus == Focus::Transcript => {
                let t = self.transcript.as_ref().unwrap();
                if self.state.selected + 1 >= t.len() {
                    self.focus = Focus::Composer;
                    return (Handled::Consumed, ConvAction::None);
                }
            }
            // Left/Right in Transcript: move one message, not a page jump.
            Key::Left if self.focus == Focus::Transcript => {
                let t = self.transcript.as_ref().unwrap();
                self.state.move_selection(-1, t, vp);
                return (Handled::Consumed, ConvAction::None);
            }
            Key::Right if self.focus == Focus::Transcript => {
                let t = self.transcript.as_ref().unwrap();
                self.state.move_selection(1, t, vp);
                return (Handled::Consumed, ConvAction::None);
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
        let sub = if chat.loading { Some("carregando…") } else { self.note.as_deref() };
        chrome::title_bar(c, frame.title, theme, &chat.name, sub);

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
        chrome::softkey_bar(c, frame.softkeys, theme, [Some("Atualizar"), send, Some("Voltar")]);
    }
}

/// Whether the atlas can actually draw every character of `s`.
///
/// A missing codepoint is not a visible box here: `mkfont.py` deliberately drops glyphs the
/// source font does not have rather than shipping `.notdef`, so the row is charged
/// `fallback_advance` and *nothing is painted*. A label made of characters outside the
/// atlas therefore renders as blank space, which reads as a bug in the layout rather than
/// as a missing font.
///
/// This matters for exactly one thing: a sticker's `alt` is an emoji, and the device
/// atlases carry ASCII, Latin-1, Cyrillic and a handful of punctuation — no emoji at all.
/// See `tools/mkfont.py`'s `default_charset`.
fn renderable(font: &dyn symbian_ui::Font, s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| font.glyph(c).is_some())
}

/// What a media row says before anything is loaded.
///
/// A label, never a picture: nothing is fetched *or decoded* until the user presses Select,
/// including previews that arrived inside the message. So the label has to carry enough to
/// decide with — how long the voice note runs, how large the file is — and to say that
/// there is something to open.
fn media_label(media: &crate::model::Media, font: &dyn symbian_ui::Font) -> alloc::string::String {
    use crate::model::Media;
    // A glyph for the kind, and the word when the glyph is not in the atlas. The mark this
    // used to draw was U+266A EIGHTH NOTE, which is in *none* of the three text fonts nor
    // in Noto Emoji — so every voice row rendered as "[ 0:07]" with a hole in it, and only
    // a rendered screenshot showed it. `mark` therefore never assumes.
    let mark = |glyph: char, word: &str| -> alloc::string::String {
        if font.glyph(glyph).is_some() {
            alloc::string::String::from(glyph)
        } else {
            alloc::string::String::from(word)
        }
    };
    match media {
        Media::Photo { size, .. } => {
            let m = mark('\u{1F5BC}', "Foto");
            if *size > 0 {
                alloc::format!("[{m} {}]", size_fmt(*size))
            } else {
                alloc::format!("[{m}]")
            }
        }
        // The emoji from documentAttributeSticker, but only when the atlas has it. It
        // almost never does, and the alternative to checking is a bubble that draws an
        // empty box.
        Media::Sticker { alt, .. } => {
            if renderable(font, alt) {
                alloc::format!("[{alt}]")
            } else {
                alloc::string::String::from("[Sticker]")
            }
        }
        // A microphone for a voice note, a note for a music file: the same distinction the
        // `voice` flag makes in the protocol, which is why reading that flag mattered.
        Media::Voice { duration, .. } => {
            alloc::format!("[{} {}]", mark('\u{1F3A4}', "Voz"), mmss(*duration))
        }
        Media::Audio { filename, duration, .. } => {
            let m = mark('\u{1F3B5}', "Audio");
            if filename.is_empty() {
                alloc::format!("[{m} {}]", mmss(*duration))
            } else {
                alloc::format!("[{m} {filename} · {}]", mmss(*duration))
            }
        }
        Media::File { filename, size, .. } => {
            let m = mark('\u{1F4CE}', "Arquivo");
            if filename.is_empty() {
                alloc::format!("[{m} {}]", size_fmt(*size))
            } else {
                alloc::format!("[{m} {filename} · {}]", size_fmt(*size))
            }
        }
        Media::Unknown => alloc::string::String::from("[Midia]"),
    }
}

/// `M:SS`, or `H:MM:SS` for anything over an hour.
fn mmss(seconds: i32) -> alloc::string::String {
    let s = seconds.max(0);
    if s >= 3600 {
        alloc::format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        alloc::format!("{}:{:02}", s / 60, s % 60)
    }
}

fn size_fmt(bytes: i64) -> alloc::string::String {
    if bytes < 1024 { return alloc::format!("{bytes}") }
    if bytes < 1024 * 1024 { return alloc::format!("{} KB", bytes / 1024) }
    alloc::format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
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

    if let Some(media) = &m.media {
        // A label, and nothing else. No thumbnail is drawn here even when the message
        // brought one inline: loading is the user's decision, and a transcript that
        // decoded a preview per row would spend the codec's four slots and the heap on
        // pictures nobody asked to see.
        let label = media_label(media, body);
        let lw = body.measure(&label);
        // The height the layout reserved, less the gap it also budgeted for. Taken from
        // `laid` rather than recomputed: the two used to disagree by two pixels, with the
        // layout reserving `line_height + 8` and the drawing using `+ 6`, and nothing would
        // have caught it growing into a real overlap.
        let ph = Rect::from_xywh(inner.x0, y, inner.width(), laid.media_band_h());
        // Subtle fill behind the placeholder so it reads as a distinct target.
        symbian_ui::paint::band(c, ph, &theme.palette.chrome);
        let lx = ph.x0 + (ph.width() - lw) / 2;
        let ly = ph.y0 + (ph.height() - body.line_height()) / 2 + body.ascent();
        c.draw_text(symbian_ui::Point::new(lx.max(ph.x0 + 4), ly), &label, body, fg);
        y = ph.y1 + MEDIA_GAP;
    }
    for l in &laid.lines {
        let text = &m.text[l.start..l.end];
        c.draw_text(Point::new(inner.x0, y + body.ascent()), text, body, fg);
        y += body.line_height();
    }

    // Metadata: dimmed against the bubble fill rather than the page background.
    let meta_col = if m.outgoing { p.accent_text.with_alpha(0xB0) } else { p.dim };
    let meta_y = inner.y0 + laid.meta_offset(body.line_height());
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
    // `Font` for its `glyph`: the sticker-label test asserts the atlas genuinely lacks the
    // emoji it is falling back from, so the fixture cannot drift away from the device.
    use symbian_ui::{BitmapFont, Font as _, Fonts, Size};

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
    fn the_timestamp_never_lands_on_top_of_the_media_label() {
        // What the preview caught: the inline-timestamp row was measured from the top of the
        // bubble's text, ignoring the media band above it — so on a photo, voice or sticker
        // row the time and the delivery tick were drawn straight over the label. Visible on
        // every single media message.
        let data = atlas();
        let f = BitmapFont::new(&data).unwrap();
        let t = theme_with(&f);
        let store = Store::mock();
        let screen = Rect::from_size(Size::new(320, 240));
        let chat = &store.chats[0];
        let tr = Transcript::build(chat, &t, screen.width());

        let lh = t.fonts.body.line_height();
        let mut checked = 0;
        for (i, m) in chat.messages.iter().enumerate() {
            let laid = &tr.laid[i];
            if m.media.is_none() {
                assert_eq!(laid.media_h, 0, "no media, no reserved band");
                continue;
            }
            checked += 1;
            assert!(laid.media_band_h() > 0, "a media row reserves a band");
            assert!(
                laid.meta_offset(lh) >= laid.media_band_h(),
                "message {i}: timestamp at {} overlaps a band ending at {}",
                laid.meta_offset(lh),
                laid.media_band_h(),
            );
            // And everything still fits inside the height the layout claimed, which is what
            // the scroll arithmetic upstream trusts.
            assert!(
                laid.meta_offset(lh) + lh + BUBBLE_VPAD * 2 + laid.gap <= laid.height,
                "message {i} overflows its own bubble",
            );
        }
        assert!(checked >= 3, "the mock must carry media rows to test; found {checked}");
    }

    #[test]
    fn a_sticker_label_does_not_rely_on_an_emoji_the_atlas_lacks() {
        // The device atlases carry ASCII, Latin-1 and Cyrillic — no emoji. A missing glyph
        // paints nothing and only advances, so putting the sticker's `alt` in the label
        // unconditionally would draw an empty box that reads as a layout bug.
        let data = atlas();
        let f = BitmapFont::new(&data).unwrap();
        let sticker = crate::model::Media::Sticker {
            id: 1,
            access_hash: 2,
            file_reference: alloc::vec::Vec::new(),
            dc_id: 2,
            alt: alloc::string::String::from("\u{1F600}"),
            preview: None,
        };
        // The test atlas covers 0x20..0x500, which excludes U+1F600 just as the real ones do.
        assert!(f.glyph('\u{1F600}').is_none(), "the fixture matches the device here");
        assert_eq!(media_label(&sticker, &f), "[Sticker]");

        // And when the emoji *is* drawable, it is used — so adding emoji to the atlas is all
        // it would take, with no change here.
        let ascii = crate::model::Media::Sticker {
            id: 1,
            access_hash: 2,
            file_reference: alloc::vec::Vec::new(),
            dc_id: 2,
            alt: alloc::string::String::from(":)"),
            preview: None,
        };
        assert_eq!(media_label(&ascii, &f), "[:)]");
    }

    #[test]
    fn a_media_bubble_shows_a_label_and_never_a_picture() {
        // Loading is the user's decision. Even a message carrying a complete inline JPEG
        // draws a label, because decoding one per visible row would spend the codec's four
        // slots and the heap on pictures nobody asked to see.
        let data = atlas();
        let f = BitmapFont::new(&data).unwrap();
        let with_preview = crate::model::Media::Photo {
            id: 1,
            access_hash: 2,
            file_reference: alloc::vec::Vec::new(),
            dc_id: 2,
            thumb_size: alloc::string::String::from("m"),
            size: 12_000,
            preview: Some(alloc::vec![0xFF, 0xD8, 0xFF, 0xE0]),
        };
        let label = media_label(&with_preview, &f);
        assert!(label.starts_with("[Foto"), "still a label: {label}");
        assert!(label.contains("KB"), "and it says what opening would cost: {label}");
    }

    #[test]
    fn the_left_softkey_refreshes_without_disturbing_back_or_send() {
        // There is no push in this client, so a screen is only as fresh as its last request.
        // The left softkey was empty on every screen; the right one is Back and must stay
        // Back, because that is where S60 puts it.
        let data = atlas();
        let f = BitmapFont::new(&data).unwrap();
        let t = theme_with(&f);
        let store = Store::mock();
        let screen = Rect::from_size(Size::new(320, 240));
        let mut conv = Conversation::new(0);

        let (h, a) = conv.handle_key(
            KeyEvent::new(Key::Softkey(Softkey::Left)), &store.chats[0], &t, screen,
        );
        assert!(matches!(h, Handled::Consumed));
        assert!(matches!(a, ConvAction::Refresh));
        assert!(conv.note.is_some(), "the user gets told something is happening");

        // And the other two are unchanged.
        let (_, a) = conv.handle_key(
            KeyEvent::new(Key::Softkey(Softkey::Right)), &store.chats[0], &t, screen,
        );
        assert!(matches!(a, ConvAction::Back));
    }

    #[test]
    fn refreshing_does_not_send_the_composer_text() {
        // The composer and the refresh share a screen; asking for new messages must not
        // post what is half-typed.
        let data = atlas();
        let f = BitmapFont::new(&data).unwrap();
        let t = theme_with(&f);
        let store = Store::mock();
        let screen = Rect::from_size(Size::new(320, 240));
        let mut conv = Conversation::new(0);
        for ch in "rascunho".chars() {
            conv.handle_key(KeyEvent::new(Key::Char(ch)), &store.chats[0], &t, screen);
        }
        let (_, a) = conv.handle_key(
            KeyEvent::new(Key::Softkey(Softkey::Left)), &store.chats[0], &t, screen,
        );
        assert!(matches!(a, ConvAction::Refresh));
        assert_eq!(conv.composer.text(), "rascunho", "the draft survives");
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
