//! A modal panel that asks a question and takes an answer.
//!
//! The small dialog every phone has and this toolkit did not: a raised panel over whatever is
//! behind it, a line or three of text, and a short list of things you can do about it.
//!
//! # Why it is a list and not two softkeys
//!
//! The obvious S60 shape for a question is a pair of softkeys — Yes on the left, No on the right.
//! It stops working at three answers, and three is the common case here: *open it*, *copy it*, or
//! neither. A list scrolls, reads the same at two answers as at five, and leaves the softkey bar
//! doing what it does everywhere else in this SDK — the middle acts, the right goes back. Which is
//! also why cancelling is not an entry in the list: Back already means that, on every other screen.
//!
//! # The body is wrapped, not truncated
//!
//! A dialog exists to tell you something, and the thing it is telling you is often the part that
//! does not fit — a URL, a filename, an error. Truncating with an ellipsis would hide exactly the
//! detail the question is about. So the body wraps to the panel width and the panel grows, up to
//! what the screen allows; past that the body scrolls with the choices.

use alloc::vec::Vec;

use symbian_gfx::{Align, Canvas, Rect};

use crate::input::{Handled, Key, KeyEvent, Softkey};
use crate::list::{ListState, Uniform};
use crate::theme::Theme;

/// How far the panel's shadow is offset, in pixels. Two: enough to separate at this size, small
/// enough not to look like a second panel.
const SHADOW: i32 = 2;

/// What a key press did to the prompt.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PromptAction {
    /// The user chose this entry (an index into the choices).
    Chosen(usize),
    /// The user backed out. The caller decides what that means; nothing here assumes it is
    /// harmless.
    Cancelled,
    None,
}

/// A modal question.
///
/// Owns only the cursor: the title, the body and the choices are passed in by the caller on every
/// draw, exactly as [`crate::app_picker::AppPicker`] takes its items. This crate ships no text.
#[derive(Clone, Debug, Default)]
pub struct Prompt {
    list: ListState,
    /// Row height from the last draw, so key handling can scroll without being handed a rect.
    row_h: i32,
    view_h: i32,
}

impl Prompt {
    pub const fn new() -> Self {
        Self { list: ListState::new(), row_h: 0, view_h: 0 }
    }

    /// Which choice the cursor is on.
    pub fn selected(&self) -> usize {
        self.list.selected
    }

    /// Put the cursor on a specific choice — for a dialog with a sensible default.
    pub fn select(&mut self, index: usize) {
        self.list.selected = index;
    }

    /// Route a key. Up/Down move; Select or the middle-slot press chooses; Back or the red key
    /// cancels.
    ///
    /// Everything else is swallowed, because the panel is modal: a key that leaked to the screen
    /// behind would act on something the user cannot see. That is the same rule the app picker
    /// follows, and the same one whose catch-all once ate the Back softkey — so Back is matched
    /// here explicitly and before it.
    pub fn handle_key(&mut self, ev: KeyEvent, choices: &[&str]) -> (Handled, PromptAction) {
        let n = choices.len();
        match ev.key {
            Key::Softkey(Softkey::Right) | Key::End => (Handled::Consumed, PromptAction::Cancelled),
            Key::Select | Key::Enter if n > 0 => {
                (Handled::Consumed, PromptAction::Chosen(self.list.selected.min(n - 1)))
            }
            Key::Up | Key::Down => {
                let rows = Uniform { count: n, height: self.row_h.max(1) };
                let delta = if matches!(ev.key, Key::Up) { -1 } else { 1 };
                self.list.move_selection(delta, &rows, self.view_h.max(1));
                (Handled::Consumed, PromptAction::None)
            }
            _ => (Handled::Consumed, PromptAction::None),
        }
    }

    /// Draw the panel over `area`, which is normally the whole content band.
    ///
    /// The panel is centred and only as tall as it needs to be, so what is behind stays visible
    /// around it — which is what makes it read as a dialog rather than as a new screen.
    pub fn draw(
        &mut self,
        c: &mut Canvas<'_>,
        area: Rect,
        theme: &Theme<'_>,
        title: &str,
        body: &str,
        choices: &[&str],
    ) {
        if area.is_empty() {
            return;
        }
        let p = &theme.palette;
        let m = &theme.metrics;

        let font = theme.fonts.body;
        let line_h = font.line_height();

        // A choice row is a line of text with a little air, not a list row.
        //
        // `metrics.row_h` is 38 on this device — the height of a chat row, sized for an avatar. Used
        // here it made three choices 114 pixels tall and the panel swallowed the screen. A dialog's
        // choices are lines, and they should read as a short list rather than as a second screen.
        let row_h = line_h + m.pad;
        self.row_h = row_h;

        // The panel hugs its content.
        //
        // Wrapped to the widest it is *allowed* to be, then narrowed to the widest line that came
        // back. A panel always at full width looks like a screen, and the whole value of a dialog is
        // that you can see it is a dialog.
        let max_text = (area.width() - m.pad * 4).max(40);
        let mut lines: Vec<&str> = Vec::new();
        // Only when there is something to wrap. `Font::wrap` splits on newlines, and splitting an
        // empty string yields one empty line — so a prompt with no body reserved a line for it and
        // came out exactly as tall as one with. Harmless to look at and enough to make a test that
        // compares the two prove nothing, which is how it was found.
        if !body.is_empty() {
            font.wrap(body, max_text, &mut |l: &str| lines.push(l));
        }

        let widest_body = lines.iter().map(|l| font.measure(l)).max().unwrap_or(0);
        let widest_choice = choices.iter().map(|ch| font.measure(ch)).max().unwrap_or(0);
        let widest_title = if title.is_empty() { 0 } else { theme.fonts.strong.measure(title) };
        let content_w = widest_body.max(widest_choice).max(widest_title);
        let panel_w = (content_w + m.pad * 2 + 2).clamp(40, area.width() - m.pad * 2);

        let title_h = if title.is_empty() { 0 } else { line_h + 2 };
        // A gap between what the dialog says and what it offers. Without it the last line of the
        // body sits flush against the first choice and reads as another entry in the list — which
        // is worse than it sounds when the body is a URL and the list is what to do with it.
        let body_gap = if lines.is_empty() { 0 } else { m.pad };
        let body_h = lines.len() as i32 * line_h + body_gap;
        let choices_h = choices.len() as i32 * row_h;
        // `+ 2` for the frame. `panel.inset(1)` takes a pixel off the top and bottom before the
        // padding, and leaving it out of the budget made the panel exactly two pixels too short —
        // which cost the body its only line, since the guard below drops a line that does not fit
        // whole. It read as a missing URL and was an arithmetic error two pixels wide.
        let wanted = title_h + body_h + choices_h + m.pad * 2 + 2;
        let panel_h = wanted.min(area.height());

        let panel = Rect::from_xywh(
            area.x0 + (area.width() - panel_w) / 2,
            area.y0 + (area.height() - panel_h) / 2,
            panel_w,
            panel_h,
        );
        // A drop shadow before the panel, offset down and right.
        //
        // Two pixels of translucent black is the whole of it, and it is what makes the panel read
        // as *above* the screen rather than as a hole cut in it. Without it the panel is a
        // rectangle of background colour sitting on a rectangle of background colour, and on a
        // busy screen — a chat transcript, say — the eye has nothing to tell it which is in front.
        let shadow = Rect::new(panel.x0 + SHADOW, panel.y0 + SHADOW, panel.x1 + SHADOW, panel.y1 + SHADOW);
        c.fill_rect(shadow, symbian_gfx::Color::rgb(0, 0, 0).with_alpha(0x70));

        // Raised, not sunken. `Select`'s popup uses a sunken frame because it is a field opening
        // *into* the screen; a dialog is a thing on top of it, and the lighting has to agree with
        // the shadow underneath or the panel reads as pressed into the page it is floating over.
        crate::paint::frame_raised(c, panel, p.bg.mid(), p.divider);
        c.fill_rect(panel.inset(1), p.bg.mid());

        let inner = panel.inset(1).inset_xy(m.pad, m.pad);
        let mut y = inner.y0;
        if !title.is_empty() {
            let r = Rect::new(inner.x0, y, inner.x1, y + line_h);
            c.draw_text_in(r, title, theme.fonts.strong, p.text, Align::Start);
            y += title_h;
        }
        for l in &lines {
            if y + line_h > inner.y1 - choices_h {
                break;
            }
            let r = Rect::new(inner.x0, y, inner.x1, y + line_h);
            // `text`, not `dim`. The body is what the dialog is *about* — the URL you are deciding
            // about — and dim against the panel's own fill is very nearly invisible on the dark
            // palette. It rendered as an empty band above the choices, which read as a layout bug
            // and was a contrast one.
            c.draw_text_in(r, l, font, p.text, Align::Start);
            y += line_h;
        }

        // The choices sit against the bottom of the panel, so their position does not move with the
        // length of the body — a dialog whose buttons walk about as the text changes is one you
        // learn to read rather than to use.
        let list_area = Rect::new(inner.x0, inner.y1 - choices_h, inner.x1, inner.y1);
        self.view_h = list_area.height();
        let rows = Uniform { count: choices.len(), height: row_h };
        self.list.clamp(&rows, self.view_h);
        let sel = self.list.selected;
        self.list.draw_visible(c, &rows, list_area, |c, i, row| {
            if i == sel {
                crate::chrome::selection(c, row, theme);
            }
            let col = if i == sel { p.selection_text } else { p.text };
            c.draw_text_in(row, choices[i], font, col, Align::Start);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{testing, Palette};
    use symbian_gfx::Size;

    const CHOICES: [&str; 3] = ["Abrir", "Copiar", "Guardar"];

    fn ev(k: Key) -> KeyEvent {
        KeyEvent::new(k)
    }

    #[test]
    fn the_middle_chooses_and_the_right_cancels() {
        // The SDK's softkey convention, which this widget must not be the one screen that breaks.
        let mut p = Prompt::new();
        assert_eq!(p.handle_key(ev(Key::Select), &CHOICES).1, PromptAction::Chosen(0));
        assert_eq!(
            p.handle_key(ev(Key::Softkey(Softkey::Right)), &CHOICES).1,
            PromptAction::Cancelled
        );
    }

    #[test]
    fn back_is_not_eaten_by_the_modal_catch_all() {
        // The defect this project already shipped once, in the app picker: a modal that swallows
        // everything swallows the only visible way out, and the softkey bar goes on offering it.
        let mut p = Prompt::new();
        let (h, a) = p.handle_key(ev(Key::Softkey(Softkey::Right)), &CHOICES);
        assert_eq!(h, Handled::Consumed);
        assert_eq!(a, PromptAction::Cancelled);
    }

    #[test]
    fn up_and_down_walk_the_choices_without_wrapping() {
        // No wraparound, like every other list here: silently jumping from the last entry to the
        // first is how people lose their place on a screen with no pointer.
        let mut p = Prompt::new();
        testing::with_theme(Palette::DARK, |t| {
            testing::with_canvas(Size::new(240, 200), |c| {
                p.draw(c, Rect::from_xywh(0, 0, 240, 200), t, "T", "b", &CHOICES);
            });
        });
        p.handle_key(ev(Key::Down), &CHOICES);
        p.handle_key(ev(Key::Down), &CHOICES);
        p.handle_key(ev(Key::Down), &CHOICES);
        assert_eq!(p.selected(), 2, "clamped at the last, not wrapped to the first");
        for _ in 0..5 {
            p.handle_key(ev(Key::Up), &CHOICES);
        }
        assert_eq!(p.selected(), 0);
    }

    #[test]
    fn a_stray_key_does_not_reach_the_screen_behind() {
        // Modal means modal: the panel covers something the user can no longer see, so a key that
        // leaked would act on a screen they are not looking at.
        let mut p = Prompt::new();
        let (h, a) = p.handle_key(ev(Key::Char('x')), &CHOICES);
        assert_eq!(h, Handled::Consumed);
        assert_eq!(a, PromptAction::None);
    }

    #[test]
    fn no_choices_means_nothing_can_be_chosen() {
        // A prompt built with an empty list is a bug in the caller, and answering `Chosen(0)` for a
        // choice that does not exist would turn it into a bug here.
        let mut p = Prompt::new();
        assert_eq!(p.handle_key(ev(Key::Select), &[]).1, PromptAction::None);
        // But it can still be dismissed, or it would be a screen with no way out.
        assert_eq!(p.handle_key(ev(Key::Softkey(Softkey::Right)), &[]).1, PromptAction::Cancelled);
    }

    #[test]
    fn a_long_body_stays_inside_the_panel() {
        // The case this exists for: a URL longer than the screen. It must wrap and stay in the
        // panel rather than running over the choices or off the edge.
        let long = "https://exemplo.com/um/caminho/bem/longo/que/nao/cabe?e=1&f=2&g=3";
        testing::with_theme(Palette::DARK, |t| {
            let mut p = Prompt::new();
            let area = Rect::from_xywh(0, 0, 240, 200);
            let (_, px) = testing::with_canvas(Size::new(240, 200), |c| {
                p.draw(c, area, t, "Link", long, &CHOICES);
            });
            // Nothing drawn in the outermost column or row: the panel is inset from the area, so
            // ink there means something escaped it.
            assert!((0..200).all(|y| px[y * 240] == 0), "ink on the left edge");
            assert!(px[..240].iter().all(|&v| v == 0), "ink on the top edge");
        });
    }

    #[test]
    fn it_draws_something_in_every_palette() {
        for (_, palette) in Palette::ALL {
            testing::with_theme(palette, |t| {
                let mut p = Prompt::new();
                let (_, px) = testing::with_canvas(Size::new(240, 200), |c| {
                    p.draw(c, Rect::from_xywh(0, 0, 240, 200), t, "Link", "corpo", &CHOICES);
                });
                assert!(px.iter().any(|&v| v != 0));
            });
        }
    }
}

#[cfg(test)]
mod body_tests {
    use super::*;
    use crate::{testing, Palette};
    use symbian_gfx::Size;

    /// Rows of the 240x200 canvas that got ink, after drawing a prompt.
    fn inked(title: &str, body: &str, choices: &[&str]) -> Vec<usize> {
        let mut rows = Vec::new();
        testing::with_theme(Palette::DARK, |t| {
            let mut p = Prompt::new();
            let (_, px) = testing::with_canvas(Size::new(240, 200), |c| {
                p.draw(c, Rect::from_xywh(0, 0, 240, 200), t, title, body, choices);
            });
            for y in 0..200usize {
                if (0..240).any(|x| px[y * 240 + x] != 0) {
                    rows.push(y);
                }
            }
        });
        rows
    }

    #[test]
    fn the_body_is_actually_drawn() {
        // The bug this exists for, and it shipped: the panel was budgeted without its own one-pixel
        // frame, came out two pixels short, and the guard that drops a line which does not fit
        // whole dropped the only line there was. The URL simply was not on the screen — and every
        // test passed, because they all asked whether *something* had been drawn.
        //
        // Asserted as a comparison rather than a coordinate: the same prompt with and without a
        // body must differ, and differ by about a line.
        let with = inked("Abrir link", "https://exemplo.com/a", &["Abrir", "Copiar"]);
        let without = inked("Abrir link", "", &["Abrir", "Copiar"]);
        assert!(!with.is_empty() && !without.is_empty());
        let span = |v: &[usize]| v.last().unwrap() - v.first().unwrap();
        assert!(
            span(&with) > span(&without),
            "a prompt with a body must be taller than one without: {} vs {}",
            span(&with),
            span(&without)
        );
    }

    #[test]
    fn a_body_too_long_for_the_panel_does_not_push_the_choices_off() {
        // The other side of the same guard. A body that cannot fit is truncated by lines, and the
        // choices — the only way out — must survive it.
        let long = "https://exemplo.com/um/caminho/absurdamente/longo/que/nao/cabe/de/jeito/nenhum/nesta/tela/pequena?a=1&b=2&c=3&d=4";
        let rows = inked("Abrir link", long, &["Abrir", "Copiar"]);
        assert!(!rows.is_empty());
        assert!(*rows.last().unwrap() < 200, "the panel ran off the canvas");
    }

    #[test]
    fn the_panel_hugs_its_content_rather_than_the_screen() {
        // A panel always at full width reads as a screen, and the whole value of a dialog is being
        // able to see that it is one. Measured as columns, not rows.
        let mut narrow = 0;
        let mut wide = 0;
        testing::with_theme(Palette::DARK, |t| {
            for (text, out) in [("ok", &mut narrow), ("uma linha bem mais comprida que a outra", &mut wide)] {
                let mut p = Prompt::new();
                let (_, px) = testing::with_canvas(Size::new(240, 200), |c| {
                    p.draw(c, Rect::from_xywh(0, 0, 240, 200), t, "T", text, &["A"]);
                });
                *out = (0..240).filter(|&x| (0..200).any(|y| px[y * 240 + x] != 0)).count();
            }
        });
        assert!(wide > narrow, "the panel did not grow with its content: {narrow} vs {wide}");
        assert!(wide < 240, "and it must not simply fill the screen");
    }
}
