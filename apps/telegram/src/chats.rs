//! The dialog list.

use symbian_ui::{
    chrome, list::Uniform, Align, Canvas, Frame, Handled, Key, KeyEvent, ListState, Rect, Theme,
};

use crate::model::Store;

pub struct ChatList {
    pub state: ListState,
}

impl ChatList {
    pub fn new() -> Self {
        Self { state: ListState::new() }
    }

    fn rows(store: &Store, theme: &Theme<'_>) -> Uniform {
        Uniform { count: store.chats.len(), height: theme.metrics.row_h }
    }

    pub fn handle_key(&mut self, ev: KeyEvent, store: &Store, theme: &Theme<'_>, viewport_h: i32) -> (Handled, ChatListAction) {
        // Down at the bottom of the list asks for more dialogs. Checked before the
        // ListState dispatch so it is not shadowed.
        if ev.key == Key::Down && !store.chats.is_empty() {
            let rows = Self::rows(store, theme);
            let content_h = rows.count as i32 * rows.height;
            let bottom = (content_h - viewport_h).max(0);
            if self.state.selected == rows.count - 1 && self.state.scroll >= bottom {
                return (Handled::Consumed, ChatListAction::LoadMore);
            }
        }
        let rows = Self::rows(store, theme);
        let handled = self.state.handle_key(ev, &rows, viewport_h);
        (handled, ChatListAction::None)
    }

    pub fn draw(&self, c: &mut Canvas<'_>, store: &Store, theme: &Theme<'_>) {
        let screen = Rect::from_size(c.size());
        let frame = Frame::split(screen, theme, true, true);

        chrome::clear(c, theme);
        let sub = if store.dialogs_loading { "carregando…" } else { &store.status };
        chrome::title_bar(c, frame.title, theme, "Telegram", Some(sub));

        if store.chats.is_empty() {
            chrome::placeholder(c, frame.content, theme, "Nenhuma conversa");
        } else {
            let rows = Self::rows(store, theme);
            let bar = self.state.scrollbar(&rows, frame.content.height());
            let gutter = chrome::scrollbar_gutter(theme, bar.is_some());
            let body = Rect { x1: frame.content.x1 - gutter, ..frame.content };

            // Clip so a row straddling the top or bottom edge is cut cleanly
            // rather than bleeding into the chrome.
            let saved = c.save();
            c.clip_to(frame.content);
            self.state.for_visible(&rows, body, |i, r| {
                self.draw_row(c, r, theme, &store.chats[i], i == self.state.selected);
            });
            c.restore(saved);

            chrome::scrollbar(c, frame.content, theme, bar);
        }

        // "Atualizar" on the left. There is no push here — no updates subscription, no long
        // poll — so the list is only as fresh as the last request, and without a way to ask
        // again the only remedy is restarting the application.
        let refresh = if store.dialogs_loading { Some("...") } else { Some("Atualizar") };
        chrome::softkey_bar(c, frame.softkeys, theme, [refresh, Some("Abrir"), Some("Sair")]);
    }

    fn draw_row(
        &self,
        c: &mut Canvas<'_>,
        r: Rect,
        theme: &Theme<'_>,
        chat: &crate::model::Chat,
        selected: bool,
    ) {
        let p = &theme.palette;
        let m = &theme.metrics;

        if selected {
            chrome::selection(c, r, theme);
        }
        // Divider on the bottom edge, skipped under the selection so the
        // highlight reads as one solid block.
        if !selected {
            c.hline(r.y1 - 1, r.x0 + m.pad, r.x1, p.divider);
        }

        let (name_col, time_col) = (
            if selected { p.selection_text } else { p.text },
            if selected { p.selection_text } else { p.dim },
        );

        // Avatar, vertically centred, then the text column beside it.
        let avatar_size = r.height() - 8;
        let av = Rect::from_xywh(r.x0 + m.pad, r.y0 + 4, avatar_size, avatar_size);
        chrome::avatar(c, av, theme, &chat.initials(), chat.color_seed());

        let text_x = av.x1 + m.pad;
        let mut right = r.x1 - m.pad;

        // Time first: it is fixed-width and sets the ceiling for the name.
        let tw = theme.fonts.small.measure(&chat.time);
        let time_row = Rect::new(right - tw, r.y0 + 4, right, r.y0 + 4 + theme.fonts.small.line_height());
        c.draw_text_in(time_row, &chat.time, theme.fonts.small, time_col, Align::End);
        right -= tw + m.pad;

        let name_row = Rect::new(text_x, r.y0 + 3, right, r.y0 + 3 + theme.fonts.strong.line_height());
        c.draw_text_in(name_row, &chat.name, theme.fonts.strong, name_col, Align::Start);

        // Unread badge sits at the bottom right and steals width from the preview.
        let mut preview_right = r.x1 - m.pad;
        if chat.unread > 0 {
            let mut buf = itoa(chat.unread);
            let label: &str = if chat.unread > 999 { "999+" } else { buf.as_str() };
            let by = r.y1 - 4 - (theme.fonts.small.line_height() + 2);
            let (fill, fg) = chrome::unread_colors(theme, selected);
            let w = chrome::badge(
                c,
                symbian_ui::Point::new(preview_right, by),
                theme,
                label,
                fill,
                fg,
            );
            preview_right -= w + m.pad;
            buf.clear();
        }

        let prev_col = if selected { p.selection_text } else { p.dim };
        let prev_row = Rect::new(
            text_x,
            r.y1 - 4 - theme.fonts.small.line_height(),
            preview_right,
            r.y1 - 4,
        );
        // A leading tick marks our own last message, the way S60 clients did.
        if chat.last_outgoing {
            let tick = "\u{2713} ";
            let tw = theme.fonts.small.measure(tick);
            c.draw_text_in(
                Rect { x1: prev_row.x0 + tw, ..prev_row },
                tick,
                theme.fonts.small,
                if selected { p.selection_text } else { p.accent },
                Align::Start,
            );
            c.draw_text_in(
                Rect { x0: prev_row.x0 + tw, ..prev_row },
                chat.preview(),
                theme.fonts.small,
                prev_col,
                Align::Start,
            );
        } else {
            c.draw_text_in(prev_row, chat.preview(), theme.fonts.small, prev_col, Align::Start);
        }
    }
}

/// Small unsigned integer to text without `core::fmt`, which would drag in
/// formatting machinery we are trying to keep out of the binary.
fn itoa(mut v: u32) -> alloc::string::String {
    let mut buf = [0u8; 10];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    alloc::string::String::from_utf8_lossy(&buf[i..]).into_owned()
}

/// What the list wants to do next, decided by the app rather than here.
pub enum ChatListAction {
    Open(usize),
    Exit,
    /// Scrolled to the bottom of the dialog list; request the next page.
    LoadMore,
    /// Re-fetch the dialog list from the server.
    Refresh,
    None,
}

impl ChatList {
    pub fn activate(&self, ev: KeyEvent, store: &Store) -> ChatListAction {
        match ev.key {
            Key::Select | Key::Softkey(symbian_ui::Softkey::Middle) | Key::Call => {
                if store.chats.is_empty() {
                    ChatListAction::None
                } else {
                    ChatListAction::Open(self.state.selected)
                }
            }
            // Left, not right: right is Exit here and Back everywhere else, which is where
            // S60 puts it and where a thumb expects it. Left is the slot this app had left
            // empty on every screen, and it is where S60 puts Options.
            Key::Softkey(symbian_ui::Softkey::Left) => ChatListAction::Refresh,
            Key::Softkey(symbian_ui::Softkey::Right) => ChatListAction::Exit,
            _ => ChatListAction::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn itoa_matches_decimal() {
        assert_eq!(itoa(0), "0");
        assert_eq!(itoa(7), "7");
        assert_eq!(itoa(42), "42");
        assert_eq!(itoa(999), "999");
        assert_eq!(itoa(4_294_967_295), "4294967295");
    }

    #[test]
    fn initials_take_word_starts() {
        let mut s = Store::mock();
        s.chats[0].name = "Ana Paula".into();
        assert_eq!(s.chats[0].initials(), "AP");
        s.chats[0].name = "Marina".into();
        assert_eq!(s.chats[0].initials(), "M");
        s.chats[0].name = "".into();
        assert_eq!(s.chats[0].initials(), "?");
    }

    #[test]
    fn color_seed_is_stable_per_name() {
        let a = Store::mock();
        let b = Store::mock();
        for (x, y) in a.chats.iter().zip(&b.chats) {
            assert_eq!(x.color_seed(), y.color_seed());
        }
    }
}
