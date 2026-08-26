//! The screens as they were drawn on the day the migration started, frozen.
//!
//! # Why a second copy of the drawing code exists at all
//!
//! Every other parity harness in this SDK compares two *implementations that both ship* — `tg`'s
//! `chats.rs` against `chats_decl.rs`, the launcher's `SettingsScreen::draw_with_icons` against
//! `settings_decl::view`. Those screens were rewritten beside the original, so the comparison had
//! two live sides to point at.
//!
//! This crate is being migrated **in place**: `draw_entry`, `draw_boot` and `settings::draw` are
//! rewritten where they stand, and after the rewrite the old pixels exist nowhere. A harness written
//! against the new code alone would compare it to itself and report `identical`, which is what
//! "identical" means when there is only one implementation, and is worth nothing.
//!
//! So the "before" is preserved here as code rather than as a PNG, for one practical reason:
//! `symbian_preview` can write PNGs and cannot read them, so a golden image could be produced and
//! never checked. Frozen source can be diffed, and `git log -p` on this file is the audit — if this
//! file changes in the same commit that changes `lib.rs`, the comparison was bent to fit rather than
//! satisfied.
//!
//! # What it is not
//!
//! It is not a fallback, and nothing calls it on a device. It exists for
//! `examples/parity.rs`. What it does **not** cover is anything it shares with the live code:
//! `row_node`, `entry_rows`, `row_name` and `move_hint` are called, not copied, because they were
//! already declarative before this migration began and are not what the migration is changing. A
//! change to those is invisible to this comparison, which is the honest boundary of the instrument.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use symbian_bootcfg::config::Policy;
use symbian_bootcfg::status::State;
use symbian_bootcfg::{BOOTCTL_UID, BOOTD_UID};
use symbian_decl_ui::cache::UiCache;
use symbian_decl_ui::constraints::Constraints;
use symbian_decl_ui::layout;
use symbian_decl_ui::widgets::Node;
use symbian_ui::Ground;
use symbian_ui::{
    chrome, Align, Canvas, ListState, PickerItem, Rect, Theme, Uniform,
};

use crate::settings::{SettingsScreen, REFRESH_LABELS};
use crate::{BootScreen, TABS, TAB_ORDER, ROW_ENABLED, ROW_POLICY, ROW_RETRIES};

/// The whole boot screen, as `App::draw` drew it.
pub fn draw_boot_screen(s: &mut BootScreen, c: &mut Canvas<'_>, theme: &Theme<'_>) {
    draw_screen(s, c, theme);
    if let Some(m) = s.confirm.as_mut() {
        m.draw(c, theme);
    }
}

fn draw_screen(s: &mut BootScreen, c: &mut Canvas<'_>, theme: &Theme<'_>) {
    let screen = Rect::from_size(c.size());
    let (title, tabs, content, softkeys) = BootScreen::regions(screen, theme);
    chrome::clear(c, theme);

    if s.entry_open {
        let name =
            s.selected_entry().map(|i| s.row_name(i)).unwrap_or_else(|| String::from("Entry"));
        chrome::title_bar(c, title, theme, &name, Some("at boot"));
        let area = Rect::new(tabs.x0, tabs.y0, content.x1, content.y1);
        draw_entry(s, c, area, theme);
        chrome::softkey_bar(c, softkeys, theme, chrome::Softkeys::new(None, None, Some("Back")));
        return;
    }

    if let Some(p) = s.picker.as_mut() {
        chrome::title_bar(c, title, theme, "Add an app", Some("to the boot list"));
        let area = Rect::new(tabs.x0, tabs.y0, content.x1, content.y1);
        let items: Vec<PickerItem<'_>> = s
            .roster
            .iter()
            .filter(|a| a.uid3 != BOOTD_UID && a.uid3 != BOOTCTL_UID)
            .filter(|a| !s.cfg.entries.iter().any(|e| e.uid3 == a.uid3))
            .map(|a| PickerItem::with_tile(a.uid3, &a.caption, a.uid3))
            .collect();
        p.draw(c, area, theme, &items, "No apps left to add.");
        chrome::softkey_bar(
            c,
            softkeys,
            theme,
            chrome::Softkeys::new(None, Some("Add"), Some("Back")),
        );
        return;
    }

    let detail = if s.move_mode {
        String::from("Move: Up/Down")
    } else {
        format!("{} at boot", s.cfg.active().count())
    };
    chrome::title_bar(c, title, theme, "Boot", Some(&detail));
    s.tabs.draw(c, tabs, theme, &TABS);

    match s.tabs.active() {
        TAB_ORDER => draw_list(s, c, content, theme),
        _ => draw_boot(s, c, content, theme),
    }

    let left = match s.tabs.active() {
        TAB_ORDER if s.move_mode => Some("Done"),
        TAB_ORDER if s.selected_entry().is_some() => Some("Move"),
        crate::TAB_LAST => Some("Reset"),
        _ => None,
    };
    chrome::softkey_bar(c, softkeys, theme, chrome::Softkeys::new(left, None, Some("Back")));
}

fn draw_list(s: &mut BootScreen, c: &mut Canvas<'_>, content: Rect, theme: &Theme<'_>) {
    let nodes: Vec<Node> = (0..s.list_rows()).map(|i| s.row_node(i)).collect();
    let rows = Uniform { count: nodes.len(), height: theme.metrics.row_h };
    let sel = s.list.selected;
    s.list.draw_visible(c, &rows, content, |c, i, row| {
        if i == sel {
            chrome::selection(c, row, theme);
        }
        let row = Rect { x1: row.x1 - chrome::scrollbar_gutter(theme), ..row };
        let t = theme.on(if i == sel { Ground::Band } else { Ground::Page });
        let mut cache = UiCache::with_capacity(nodes[i].slot_count() + 4);
        layout::measure_node(
            &nodes[i],
            0,
            Constraints::tight(row.width(), row.height()),
            &t,
            &mut cache,
        );
        layout::layout_node(&nodes[i], 0, row, &mut cache, &t);
        layout::draw_node(&nodes[i], 0, &cache, c, &t);
    });
    chrome::scrollbar(c, content, theme, s.list.scrollbar(&rows, content.height()));
}

fn draw_entry(s: &mut BootScreen, c: &mut Canvas<'_>, content: Rect, theme: &Theme<'_>) {
    if s.selected_entry().is_none() {
        chrome::placeholder(c, content, theme, "Pick a row on the List tab.");
        return;
    }
    let rh = theme.metrics.row_h;
    let mut rest = content;
    for row in s.entry_rows() {
        let (r, below) = rest.split_top(rh);
        rest = below;
        let focused = s.entry_focus == row;
        match row {
            ROW_ENABLED => s.entry_enabled.draw(c, r, theme, "Start at boot", focused),
            ROW_POLICY => {
                s.entry_policy.draw(c, r, theme, &Policy::LABELS, focused);
                let p = &theme.palette;
                let col = if focused { p.selection_text } else { p.text };
                let cell = r.inset_xy(theme.metrics.pad, 0);
                c.draw_text_in(cell, "When it stops", theme.fonts.body, col, Align::Start);
            }
            ROW_RETRIES => s.entry_retries.draw(c, r, theme, "Restart limit", focused),
            _ => s.entry_delay.draw(c, r, theme, "Delay before launch (s)", focused),
        }
    }

    if s.entry_policy.is_open() {
        let box_ = symbian_ui::select::popup_box(content, Policy::LABELS.len(), theme);
        s.entry_policy.draw_popup(c, box_, theme, &Policy::LABELS);
    }
}

fn draw_boot(s: &mut BootScreen, c: &mut Canvas<'_>, content: Rect, theme: &Theme<'_>) {
    let Some(st) = s.status.clone() else {
        chrome::placeholder(c, content, theme, "No boot recorded yet.");
        return;
    };
    let mut lines: Vec<String> = Vec::with_capacity(st.entries.len() + 1);
    lines.push(match st.mode {
        symbian_bootcfg::Mode::Normal => {
            format!("Last boot: normal \u{b7} {} restarts", st.restarts_used)
        }
        symbian_bootcfg::Mode::Safe => {
            format!("SAFE MODE \u{2014} {} boots never settled", st.boot_count)
        }
        symbian_bootcfg::Mode::ConfigError => String::from("Config unreadable \u{2014} nothing ran"),
        symbian_bootcfg::Mode::Disabled => String::from("Boot manager is switched off"),
    });
    for e in &st.entries {
        let name = s
            .cfg
            .entries
            .iter()
            .find(|x| x.uid3 == e.uid3)
            .map(|x| x.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("[{:08X}]", e.uid3));
        let mut line = format!("{name} \u{2014} {}", e.state.describe());
        if e.state == State::LaunchFailed {
            line = format!("{line} ({})", e.last_rc);
        } else if e.restarts > 0 {
            line = format!("{line}, {} restarts", e.restarts);
        }
        lines.push(line);
    }

    let rows = Uniform { count: lines.len(), height: theme.fonts.body.line_height() + 2 };
    let p = &theme.palette;
    let view = &mut ListState::new();
    view.draw_visible(c, &rows, content, |c, i, row| {
        let col = if i == 0 { p.accent } else { p.text };
        let font = if i == 0 { theme.fonts.strong } else { theme.fonts.body };
        c.draw_text_in(row.inset_xy(theme.metrics.pad, 0), &lines[i], font, col, Align::Start);
    });
}

/// The settings screen, as `SettingsScreen::draw` drew it.
pub fn draw_settings(s: &mut SettingsScreen, c: &mut Canvas<'_>, theme: &Theme<'_>) {
    let screen = Rect::from_size(c.size());
    let f = chrome::Frame::split(screen, theme, true, true);
    chrome::clear(c, theme);
    chrome::title_bar(c, f.title, theme, "Settings", None);

    let p = &theme.palette;
    let rh = theme.metrics.row_h;
    let content = f.content;
    let (r0, rest) = content.split_top(rh);
    let (r1, rest) = rest.split_top(rh);
    let (r2, rest) = rest.split_top(rh);
    let (r3, _) = rest.split_top(rh);

    s.enabled.draw(c, r0, theme, "Boot manager enabled", s.focus == crate::settings::ROW_ENABLED);
    s.first_delay.draw(c, r1, theme, "First launch delay (s)", s.focus == crate::settings::ROW_FIRST);
    let ceil = format!("Restart ceiling per boot: {}", s.ceiling.value());
    s.ceiling.draw(c, r2, theme, &ceil, s.focus == crate::settings::ROW_CEILING);

    s.refresh.draw(c, r3, theme, &REFRESH_LABELS, s.focus == crate::settings::ROW_REFRESH);
    let col =
        if s.focus == crate::settings::ROW_REFRESH { p.selection_text } else { p.text };
    c.draw_text_in(
        r3.inset_xy(theme.metrics.pad, 0),
        "Packages auto-refresh",
        theme.fonts.body,
        col,
        Align::Start,
    );

    chrome::softkey_bar(
        c,
        f.softkeys,
        theme,
        chrome::Softkeys::new(Some("Reset safe mode"), None, Some("Back")),
    );
    if s.refresh.is_open() {
        let box_ = symbian_ui::select::popup_box(content, REFRESH_LABELS.len(), theme);
        s.refresh.draw_popup(c, box_, theme, &REFRESH_LABELS);
    }
}
