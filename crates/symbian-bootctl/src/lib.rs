//! The boot manager's screens: what starts at boot, in what order, and what happens when it dies.
//!
//! Four tabs, because the four questions are genuinely separate:
//!
//! - **List** — the boot order itself. Reordering is a *move mode* rather than a widget: the left
//!   softkey turns it on, and Up/Down then carry the focused row instead of the cursor. Position in
//!   the list is the order, so a move is a swap and nothing can disagree with anything else.
//! - **Entry** — the focused row's policy and delay.
//! - **Setup** — the master switch, the first-launch delay, the restart ceiling.
//! - **Boot** — what actually happened last boot, as `apps/bootd` recorded it.
//!
//! This crate does no I/O. It is handed a [`BootConfig`] and a [`BootStatus`] and hands an edited
//! config back, so `apps/bootctl` owns every file access and every screen here is host-testable.
//!
//! It also never launches or stops anything. The editor changes what the *next* boot does; that is
//! the whole contract, and it is why a mistake here costs a reboot rather than a running phone.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use symbian::apps::AppInfo;
use symbian_bootcfg::config::{Entry, Policy};
use symbian_bootcfg::status::State;
use symbian_bootcfg::{BootConfig, BootStatus, BOOTCTL_UID, BOOTD_UID};
use symbian_ui::{
    chrome, App, AppPicker, Align, Canvas, Handled, Key, KeyEvent, ListState, PickerAction,
    PickerItem, Rect, Select, Softkey, Stepper, Tabs, Theme, Toggle, Uniform,
};

const TABS: [&str; 4] = ["List", "Entry", "Setup", "Boot"];

/// Rows on the Entry tab. Retries is drawn only for `Times`, but keeping the focus index stable
/// means the cursor does not jump when the policy changes under it.
const ENTRY_ROWS: usize = 4;
/// Rows on the Setup tab.
const SETUP_ROWS: usize = 3;

/// Per-entry delays step in half-seconds, 0..=20 — nought to ten seconds.
const HALF_SECOND_MS: u32 = 500;
/// The global first-launch delay steps in five-second jumps, 0..=12 — nought to a minute.
const FIRST_DELAY_STEP_MS: u32 = 5_000;
const FIRST_DELAY_STEPS: i32 = 12;
/// Ranges are kept deliberately short because on a tabbed screen the tab strip owns Left/Right, so
/// a stepper is driven by Select cycling up with wrap. A 0..=180 range would be 180 presses.
const MAX_DELAY_HALVES: i32 = 20;
const MAX_RETRIES: i32 = 10;
const MAX_CEILING: i32 = 20;

pub struct BootScreen {
    cfg: BootConfig,
    status: Option<BootStatus>,
    roster: Vec<AppInfo>,

    tabs: Tabs,
    last_tab: usize,
    list: ListState,
    move_mode: bool,
    picker: Option<AppPicker>,

    entry_focus: usize,
    entry_enabled: Toggle,
    entry_policy: Select,
    entry_retries: Stepper,
    entry_delay: Stepper,

    setup_focus: usize,
    setup_enabled: Toggle,
    setup_first_delay: Stepper,
    setup_ceiling: Stepper,

    changed: bool,
    reset_requested: bool,
    back: bool,
}

impl BootScreen {
    pub fn new(cfg: BootConfig, status: Option<BootStatus>, roster: Vec<AppInfo>) -> Self {
        let mut me = Self {
            setup_enabled: Toggle::new(cfg.enabled),
            setup_first_delay: Stepper::new(
                (cfg.first_delay_ms / FIRST_DELAY_STEP_MS) as i32,
                0,
                FIRST_DELAY_STEPS,
            ),
            setup_ceiling: Stepper::new(cfg.max_restarts as i32, 0, MAX_CEILING),
            cfg,
            status,
            roster,
            tabs: Tabs::new(),
            last_tab: 0,
            list: ListState::new(),
            move_mode: false,
            picker: None,
            entry_focus: 0,
            entry_enabled: Toggle::new(true),
            entry_policy: Select::new(0),
            entry_retries: Stepper::new(3, 1, MAX_RETRIES),
            entry_delay: Stepper::new(4, 0, MAX_DELAY_HALVES),
            setup_focus: 0,
            changed: false,
            reset_requested: false,
            back: false,
        };
        me.load_entry();
        me
    }

    /// The edited config, for the caller to persist.
    pub fn config(&self) -> &BootConfig {
        &self.cfg
    }

    /// True once the right softkey was pressed.
    pub fn back(&self) -> bool {
        self.back
    }

    /// Whether anything was edited since the last call. Consumed, so the caller writes once.
    pub fn take_changed(&mut self) -> bool {
        core::mem::take(&mut self.changed)
    }

    /// Whether the user asked to clear the safe-mode counter. Consumed.
    pub fn take_reset(&mut self) -> bool {
        core::mem::take(&mut self.reset_requested)
    }

    /// How many rows the List tab shows: every entry, plus the trailing "add" row.
    fn list_rows(&self) -> usize {
        self.cfg.entries.len() + 1
    }

    /// The entry the cursor is on, if it is not on the "add" row.
    fn selected_entry(&self) -> Option<usize> {
        let i = self.list.selected;
        (i < self.cfg.entries.len()).then_some(i)
    }

    /// Copy the focused entry into the Entry tab's widgets.
    fn load_entry(&mut self) {
        let Some(i) = self.selected_entry() else { return };
        let e = &self.cfg.entries[i];
        self.entry_enabled.set(e.enabled);
        self.entry_policy.set(e.policy.label_index());
        if let Policy::Times(n) = e.policy {
            self.entry_retries.set(n as i32);
        }
        self.entry_delay.set((e.delay_ms / HALF_SECOND_MS) as i32);
    }

    /// Copy the Entry tab's widgets back into the focused entry.
    fn store_entry(&mut self) {
        let Some(i) = self.selected_entry() else { return };
        let retries = self.entry_retries.value().clamp(1, 255) as u8;
        let policy = match self.entry_policy.selected() {
            1 => Policy::Times(retries),
            2 => Policy::Always,
            _ => Policy::Never,
        };
        let delay_ms = (self.entry_delay.value().max(0) as u32).saturating_mul(HALF_SECOND_MS);
        let enabled = self.entry_enabled.on();
        let e = &mut self.cfg.entries[i];
        // Switching an entry back on is the user overruling the auto-disarm, so the "why it is off"
        // marker goes with it. Leaving it set would keep showing "crash loop" next to a live entry.
        if enabled && !e.enabled {
            e.auto_disarmed = false;
        }
        e.enabled = enabled;
        e.policy = policy;
        e.delay_ms = delay_ms;
        self.changed = true;
    }

    fn store_setup(&mut self) {
        self.cfg.enabled = self.setup_enabled.on();
        self.cfg.first_delay_ms =
            (self.setup_first_delay.value().max(0) as u32) * FIRST_DELAY_STEP_MS;
        self.cfg.max_restarts = self.setup_ceiling.value().clamp(0, u16::MAX as i32) as u16;
        self.changed = true;
    }

    /// The apps offered by the picker: everything installed that is not already in the list, and
    /// never the boot manager's own two binaries — bootd relaunching itself forks forever, and
    /// relaunching this editor would put it over the user's screen every few seconds.
    fn picker_items(&self) -> Vec<PickerItem<'_>> {
        self.roster
            .iter()
            .filter(|a| a.uid3 != BOOTD_UID && a.uid3 != BOOTCTL_UID)
            .filter(|a| !self.cfg.entries.iter().any(|e| e.uid3 == a.uid3))
            .map(|a| PickerItem::with_tile(a.uid3, &a.caption, a.uid3))
            .collect()
    }

    fn add_entry(&mut self, uid3: u32) {
        let name = self
            .roster
            .iter()
            .find(|a| a.uid3 == uid3)
            .map(|a| a.caption.clone())
            .unwrap_or_else(|| format!("[{uid3:08X}]"));
        self.cfg.entries.push(Entry::new(uid3, name));
        self.list.selected = self.cfg.entries.len() - 1;
        self.changed = true;
        self.load_entry();
    }

    fn remove_selected(&mut self) -> Handled {
        let Some(i) = self.selected_entry() else { return Handled::Ignored };
        self.cfg.entries.remove(i);
        if self.list.selected >= self.list_rows() {
            self.list.selected = self.list_rows().saturating_sub(1);
        }
        self.move_mode = false;
        self.changed = true;
        self.load_entry();
        Handled::Consumed
    }

    /// One row's text on the List tab.
    fn row_label(&self, i: usize) -> String {
        if i >= self.cfg.entries.len() {
            return String::from("+  Add an app…");
        }
        let e = &self.cfg.entries[i];
        let name = if e.name.is_empty() { format!("[{:08X}]", e.uid3) } else { e.name.clone() };
        let policy = match e.policy {
            Policy::Never => String::from("once"),
            Policy::Times(n) => format!("retry {n}"),
            Policy::Always => String::from("always"),
        };
        let delay = e.delay_ms / 100;
        if e.auto_disarmed {
            format!("{}. {}  — off (crash loop)", i + 1, name)
        } else if !e.enabled {
            format!("{}. {}  — off", i + 1, name)
        } else {
            format!("{}. {}  · {} · +{}.{}s", i + 1, name, policy, delay / 10, delay % 10)
        }
    }

    fn regions(screen: Rect, theme: &Theme<'_>) -> (Rect, Rect, Rect, Rect) {
        let f = chrome::Frame::split(screen, theme, true, true);
        let (tabs, content) = f.content.split_top(theme.metrics.row_h);
        (f.title, tabs, content, f.softkeys)
    }

    // ---------------------------------------------------------------- key routing

    fn route_picker(&mut self, ev: KeyEvent) -> Handled {
        // Taken out and put back, because the item list borrows `self` and the picker lives in it.
        let Some(mut p) = self.picker.take() else { return Handled::Ignored };
        let (handled, action) = {
            let items = self.picker_items();
            p.handle_key(ev, &items)
        };
        match action {
            PickerAction::Picked(uid) => self.add_entry(uid),
            PickerAction::Cancelled => {}
            PickerAction::None => self.picker = Some(p),
        }
        handled
    }

    fn route_list(&mut self, ev: KeyEvent, content: Rect, theme: &Theme<'_>) -> Handled {
        // Move mode: Up/Down carry the row, not the cursor. The cursor follows the row it is
        // holding, which is what makes it read as dragging rather than as a swap-and-jump.
        if self.move_mode {
            match ev.key {
                Key::Up => {
                    self.list.selected = self.cfg.move_up(self.list.selected);
                    self.changed = true;
                    return Handled::Consumed;
                }
                Key::Down => {
                    if self.list.selected + 1 < self.cfg.entries.len() {
                        self.list.selected = self.cfg.move_down(self.list.selected);
                        self.changed = true;
                    }
                    return Handled::Consumed;
                }
                Key::Select | Key::Softkey(Softkey::Left) => {
                    self.move_mode = false;
                    return Handled::Consumed;
                }
                _ => {}
            }
        }

        let rows = Uniform { count: self.list_rows(), height: theme.metrics.row_h };
        if self.list.handle_key(ev, &rows, content.height()) == Handled::Consumed {
            self.load_entry();
            return Handled::Consumed;
        }
        match ev.key {
            Key::Select => {
                if self.selected_entry().is_none() {
                    self.picker = Some(AppPicker::new());
                } else {
                    // Select flips the switch in place; the policy and delay live on the Entry tab.
                    self.entry_enabled.set(!self.entry_enabled.on());
                    self.store_entry();
                }
                Handled::Consumed
            }
            Key::Backspace | Key::Delete => self.remove_selected(),
            Key::Softkey(Softkey::Left) => {
                if self.selected_entry().is_some() {
                    self.move_mode = true;
                }
                Handled::Consumed
            }
            _ => Handled::Ignored,
        }
    }

    fn route_entry(&mut self, ev: KeyEvent) -> Handled {
        if self.selected_entry().is_none() {
            return Handled::Ignored;
        }
        // An open dropdown is modal for as long as it is open.
        if self.entry_policy.is_open() {
            let (h, _) = self.entry_policy.handle_key(ev, &Policy::LABELS);
            if h == Handled::Consumed {
                self.store_entry();
            }
            return h;
        }
        match ev.key {
            Key::Up => {
                self.entry_focus = self.entry_focus.saturating_sub(1);
                Handled::Consumed
            }
            Key::Down => {
                self.entry_focus = (self.entry_focus + 1).min(ENTRY_ROWS - 1);
                Handled::Consumed
            }
            _ => {
                let h = match self.entry_focus {
                    0 => self.entry_enabled.handle_key(ev),
                    1 => self.entry_policy.handle_key(ev, &Policy::LABELS).0,
                    2 => self.entry_retries.handle_key(ev),
                    _ => self.entry_delay.handle_key(ev),
                };
                if h == Handled::Consumed {
                    self.store_entry();
                }
                h
            }
        }
    }

    fn route_setup(&mut self, ev: KeyEvent) -> Handled {
        match ev.key {
            Key::Up => {
                self.setup_focus = self.setup_focus.saturating_sub(1);
                Handled::Consumed
            }
            Key::Down => {
                self.setup_focus = (self.setup_focus + 1).min(SETUP_ROWS - 1);
                Handled::Consumed
            }
            _ => {
                let h = match self.setup_focus {
                    0 => self.setup_enabled.handle_key(ev),
                    1 => self.setup_first_delay.handle_key(ev),
                    _ => self.setup_ceiling.handle_key(ev),
                };
                if h == Handled::Consumed {
                    self.store_setup();
                }
                h
            }
        }
    }

    // ---------------------------------------------------------------- drawing

    fn draw_list(&mut self, c: &mut Canvas<'_>, content: Rect, theme: &Theme<'_>) {
        // Precompute the labels so the draw closure borrows a plain Vec, not `self`.
        let labels: Vec<String> = (0..self.list_rows()).map(|i| self.row_label(i)).collect();
        let rows = Uniform { count: labels.len(), height: theme.metrics.row_h };
        let sel = self.list.selected;
        let moving = self.move_mode;
        let p = &theme.palette;
        self.list.for_visible(&rows, content, |i, row| {
            if i == sel {
                chrome::selection(c, row, theme);
            }
            let col = if i == sel { p.selection_text } else { p.text };
            let cell = row.inset_xy(theme.metrics.pad, 0);
            c.draw_text_in(cell, &labels[i], theme.fonts.body, col, Align::Start);
            if i == sel && moving {
                c.draw_text_in(cell, "\u{2195}", theme.fonts.strong, col, Align::End);
            }
        });
        chrome::scrollbar(c, content, theme, self.list.scrollbar(&rows, content.height()));
    }

    fn draw_entry(&mut self, c: &mut Canvas<'_>, content: Rect, theme: &Theme<'_>) {
        if self.selected_entry().is_none() {
            chrome::placeholder(c, content, theme, "Pick a row on the List tab.");
            return;
        }
        let rh = theme.metrics.row_h;
        let (r0, rest) = content.split_top(rh);
        let (r1, rest) = rest.split_top(rh);
        let (r2, rest) = rest.split_top(rh);
        let (r3, _) = rest.split_top(rh);

        self.entry_enabled.draw(c, r0, theme, "Start at boot", self.entry_focus == 0);
        self.entry_policy.draw(c, r1, theme, &Policy::LABELS, self.entry_focus == 1);
        if self.entry_policy.selected() == 1 {
            self.entry_retries.draw(c, r2, theme, "Restart limit", self.entry_focus == 2);
        }
        let secs = self.entry_delay.value();
        let label = format!("Delay before launch: {}.{} s", secs / 2, (secs % 2) * 5);
        self.entry_delay.draw(c, r3, theme, &label, self.entry_focus == 3);

        // The dropdown floats over the rows beneath it, so it is drawn last.
        if self.entry_policy.is_open() {
            self.entry_policy.draw_popup(c, content, theme, &Policy::LABELS);
        }
    }

    fn draw_setup(&mut self, c: &mut Canvas<'_>, content: Rect, theme: &Theme<'_>) {
        let rh = theme.metrics.row_h;
        let (r0, rest) = content.split_top(rh);
        let (r1, rest) = rest.split_top(rh);
        let (r2, _) = rest.split_top(rh);
        self.setup_enabled.draw(c, r0, theme, "Boot manager enabled", self.setup_focus == 0);
        let first = format!("First launch delay: {} s", self.setup_first_delay.value() * 5);
        self.setup_first_delay.draw(c, r1, theme, &first, self.setup_focus == 1);
        let ceil = format!("Restart ceiling per boot: {}", self.setup_ceiling.value());
        self.setup_ceiling.draw(c, r2, theme, &ceil, self.setup_focus == 2);
    }

    fn draw_boot(&mut self, c: &mut Canvas<'_>, content: Rect, theme: &Theme<'_>) {
        let Some(st) = self.status.clone() else {
            chrome::placeholder(c, content, theme, "No boot recorded yet.");
            return;
        };
        let mut lines: Vec<String> = Vec::with_capacity(st.entries.len() + 1);
        lines.push(match st.mode {
            symbian_bootcfg::Mode::Normal => {
                format!("Last boot: normal · {} restarts", st.restarts_used)
            }
            symbian_bootcfg::Mode::Safe => {
                format!("SAFE MODE — {} boots never settled", st.boot_count)
            }
            symbian_bootcfg::Mode::ConfigError => String::from("Config unreadable — nothing ran"),
            symbian_bootcfg::Mode::Disabled => String::from("Boot manager is switched off"),
        });
        for e in &st.entries {
            let name = self
                .cfg
                .entries
                .iter()
                .find(|x| x.uid3 == e.uid3)
                .map(|x| x.name.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| format!("[{:08X}]", e.uid3));
            let mut line = format!("{name} — {}", e.state.describe());
            if e.state == State::LaunchFailed {
                line = format!("{line} ({})", e.last_rc);
            } else if e.restarts > 0 {
                line = format!("{line}, {} restarts", e.restarts);
            }
            lines.push(line);
        }

        let rows = Uniform { count: lines.len(), height: theme.fonts.body.line_height() + 2 };
        let p = &theme.palette;
        // A fresh ListState each draw: this tab is read-only, so there is no selection to keep and
        // nothing to scroll with — the rows either fit or the report is longer than the screen.
        let view = &mut ListState::new();
        view.for_visible(&rows, content, |i, row| {
            let col = if i == 0 { p.accent } else { p.text };
            let font = if i == 0 { theme.fonts.strong } else { theme.fonts.body };
            c.draw_text_in(row.inset_xy(theme.metrics.pad, 0), &lines[i], font, col, Align::Start);
        });
    }
}

impl App for BootScreen {
    fn title(&self) -> &str {
        "Boot manager"
    }

    fn handle_key(&mut self, ev: KeyEvent, theme: &Theme<'_>, screen: Rect) -> Handled {
        // The picker drawer is modal: while it is open it takes every key.
        if self.picker.is_some() {
            return self.route_picker(ev);
        }
        if let Key::Softkey(Softkey::Right) = ev.key {
            self.back = true;
            return Handled::Consumed;
        }
        // Left/Right switch tabs — except in move mode, where the softkey is "Done" and stealing
        // Left/Right would strand the user mid-drag.
        if !self.move_mode && self.tabs.handle_key(ev, TABS.len()) == Handled::Consumed {
            if self.tabs.active() != self.last_tab {
                self.last_tab = self.tabs.active();
                if self.last_tab == 1 {
                    self.load_entry();
                }
            }
            return Handled::Consumed;
        }
        let (_, _, content, _) = Self::regions(screen, theme);
        match self.tabs.active() {
            0 => self.route_list(ev, content, theme),
            1 => self.route_entry(ev),
            2 => self.route_setup(ev),
            _ => {
                if let Key::Softkey(Softkey::Left) = ev.key {
                    self.reset_requested = true;
                    return Handled::Consumed;
                }
                Handled::Ignored
            }
        }
    }

    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        let screen = Rect::from_size(c.size());
        let (title, tabs, content, softkeys) = Self::regions(screen, theme);
        chrome::clear(c, theme);

        let detail = if self.move_mode {
            String::from("Move: Up/Down")
        } else {
            format!("{} at boot", self.cfg.active().count())
        };
        chrome::title_bar(c, title, theme, "Boot manager", Some(&detail));
        self.tabs.draw(c, tabs, theme, &TABS);

        match self.tabs.active() {
            0 => self.draw_list(c, content, theme),
            1 => self.draw_entry(c, content, theme),
            2 => self.draw_setup(c, content, theme),
            _ => self.draw_boot(c, content, theme),
        }

        let left = match self.tabs.active() {
            0 if self.move_mode => Some("Done"),
            0 if self.selected_entry().is_some() => Some("Move"),
            3 => Some("Reset"),
            _ => None,
        };
        chrome::softkey_bar(c, softkeys, theme, [left, None, Some("Back")]);

        // Drawn over everything, including the softkey bar's row, because it is a drawer.
        if let Some(p) = self.picker.as_mut() {
            let items: Vec<PickerItem<'_>> = self
                .roster
                .iter()
                .filter(|a| a.uid3 != BOOTD_UID && a.uid3 != BOOTCTL_UID)
                .filter(|a| !self.cfg.entries.iter().any(|e| e.uid3 == a.uid3))
                .map(|a| PickerItem::with_tile(a.uid3, &a.caption, a.uid3))
                .collect();
            p.draw(c, content, theme, &items, "No apps left to add.");
        }
    }

    fn should_exit(&self) -> bool {
        self.back
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;
    use symbian_ui::gfx::Size;
    use symbian_ui::testing::{with_canvas, with_theme, SCREEN};
    use symbian_ui::Palette;

    fn app(uid: u32, caption: &str) -> AppInfo {
        AppInfo { uid3: uid, caption: caption.to_string(), hidden: false, system: false }
    }

    fn roster() -> Vec<AppInfo> {
        vec![
            app(0x1000_0001, "Calculator"),
            app(0x1000_0002, "Notes"),
            app(BOOTD_UID, "bootd"),
            app(BOOTCTL_UID, "Boot manager"),
        ]
    }

    fn cfg2() -> BootConfig {
        BootConfig {
            entries: vec![
                Entry::new(0x1000_0001, "Calculator".to_string()),
                Entry::new(0x1000_0002, "Notes".to_string()),
            ],
            ..Default::default()
        }
    }

    fn press(s: &mut BootScreen, k: Key) -> Handled {
        with_theme(Palette::DARK, |t| s.handle_key(KeyEvent::new(k), t, SCREEN))
    }

    fn draw_once(s: &mut BootScreen) {
        with_canvas(Size::new(320, 240), |c| {
            with_theme(Palette::DARK, |t| s.draw(c, t));
        });
    }

    #[test]
    fn move_mode_swaps_rows_and_carries_the_cursor() {
        let mut s = BootScreen::new(cfg2(), None, roster());
        press(&mut s, Key::Down); // cursor on "Notes"
        assert_eq!(s.list.selected, 1);
        press(&mut s, Key::Softkey(Softkey::Left)); // move mode
        press(&mut s, Key::Up);
        assert_eq!(s.cfg.entries[0].uid3, 0x1000_0002, "Notes moved up");
        assert_eq!(s.list.selected, 0, "the cursor travelled with the row");
        assert!(s.take_changed());
    }

    #[test]
    fn move_mode_does_not_switch_tabs() {
        let mut s = BootScreen::new(cfg2(), None, roster());
        press(&mut s, Key::Softkey(Softkey::Left));
        press(&mut s, Key::Right);
        assert_eq!(s.tabs.active(), 0, "Left/Right must not strand the user mid-move");
    }

    #[test]
    fn the_picker_excludes_the_boot_managers_own_binaries_and_anything_listed() {
        let s = BootScreen::new(cfg2(), None, roster());
        let ids: Vec<u32> = s.picker_items().iter().map(|i| i.id).collect();
        assert!(ids.is_empty(), "both real apps are already listed, both own UIDs are refused");

        let s = BootScreen::new(BootConfig::default(), None, roster());
        let ids: Vec<u32> = s.picker_items().iter().map(|i| i.id).collect();
        assert_eq!(ids, vec![0x1000_0001, 0x1000_0002]);
    }

    #[test]
    fn the_add_row_opens_the_picker_and_a_pick_appends() {
        let mut s = BootScreen::new(BootConfig::default(), None, roster());
        press(&mut s, Key::Select); // the list is empty, so row 0 is "+ Add an app"
        assert!(s.picker.is_some());
        s.add_entry(0x1000_0002);
        assert_eq!(s.cfg.entries.len(), 1);
        assert_eq!(s.cfg.entries[0].name, "Notes");
        assert_eq!(s.cfg.entries[0].policy, Policy::Times(3), "the safe default, not Always");
    }

    #[test]
    fn select_toggles_a_row_and_delete_removes_it() {
        let mut s = BootScreen::new(cfg2(), None, roster());
        press(&mut s, Key::Select);
        assert!(!s.cfg.entries[0].enabled);
        press(&mut s, Key::Backspace);
        assert_eq!(s.cfg.entries.len(), 1);
        assert_eq!(s.cfg.entries[0].uid3, 0x1000_0002);
    }

    #[test]
    fn re_enabling_an_auto_disarmed_entry_clears_the_marker() {
        let mut cfg = cfg2();
        cfg.entries[0].enabled = false;
        cfg.entries[0].auto_disarmed = true;
        let mut s = BootScreen::new(cfg, None, roster());
        press(&mut s, Key::Select);
        assert!(s.cfg.entries[0].enabled);
        assert!(!s.cfg.entries[0].auto_disarmed, "the crash-loop marker goes with the switch");
    }

    #[test]
    fn the_entry_tab_edits_the_focused_row_only() {
        let mut s = BootScreen::new(cfg2(), None, roster());
        press(&mut s, Key::Down); // focus "Notes"
        press(&mut s, Key::Right); // Entry tab
        assert_eq!(s.tabs.active(), 1);
        press(&mut s, Key::Down); // policy row
        press(&mut s, Key::Select); // open the dropdown
        press(&mut s, Key::Down);
        press(&mut s, Key::Select); // commit
        assert_ne!(
            s.cfg.entries[1].policy, s.cfg.entries[0].policy,
            "only the focused row changed"
        );
        assert_eq!(s.cfg.entries[0].policy, Policy::Times(3), "the other row is untouched");
    }

    #[test]
    fn setup_writes_the_global_fields() {
        let mut s = BootScreen::new(cfg2(), None, roster());
        press(&mut s, Key::Right);
        press(&mut s, Key::Right); // Setup tab
        assert_eq!(s.tabs.active(), 2);
        press(&mut s, Key::Select); // flip the master switch
        assert!(!s.cfg.enabled);
        press(&mut s, Key::Down);
        // Left/Right belong to the tab strip, so a stepper is driven by Select cycling.
        press(&mut s, Key::Select);
        assert_eq!(s.cfg.first_delay_ms, 10_000, "8 s rounded to the 5 s step, then one up");
    }

    #[test]
    fn the_boot_tab_reset_is_a_one_shot_request() {
        let mut s = BootScreen::new(cfg2(), None, roster());
        for _ in 0..3 {
            press(&mut s, Key::Right);
        }
        assert_eq!(s.tabs.active(), 3);
        press(&mut s, Key::Softkey(Softkey::Left));
        assert!(s.take_reset());
        assert!(!s.take_reset(), "consumed, so the caller acts once");
    }

    #[test]
    fn the_right_softkey_leaves() {
        let mut s = BootScreen::new(cfg2(), None, roster());
        press(&mut s, Key::Softkey(Softkey::Right));
        assert!(s.back());
        assert!(s.should_exit());
    }

    #[test]
    fn every_tab_draws_including_the_empty_and_safe_mode_cases() {
        let status = BootStatus {
            mode: symbian_bootcfg::Mode::Safe,
            boot_count: 3,
            restarts_used: 2,
            entries: vec![symbian_bootcfg::EntryStatus {
                uid3: 0x1000_0001,
                last_rc: -1,
                launch_at_s: 9,
                restarts: 1,
                state: State::LaunchFailed,
            }],
        };
        for cfg in [cfg2(), BootConfig::default()] {
            let mut s = BootScreen::new(cfg, Some(status.clone()), roster());
            for tab in 0..TABS.len() {
                s.tabs.set_active(tab, TABS.len());
                draw_once(&mut s);
            }
        }
        // And with no report at all, which is every phone before its first supervised boot.
        let mut s = BootScreen::new(cfg2(), None, roster());
        s.tabs.set_active(3, TABS.len());
        draw_once(&mut s);
    }
}
