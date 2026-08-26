//! The knobs, on a screen of their own.
//!
//! They were a tab called `Setup`, sitting between the boot list and the boot report as though a
//! setting were a third view of the same thing. It is not: the list and the report are two ways of
//! looking at *this boot*, and these are decisions that outlive it. So settings became a section in
//! the drawer, which is what the hierarchy is for — see `symbian_ui::drawer`.
//!
//! Two files' worth of settings live here, and the split is not arbitrary: the boot config
//! (`boot.cfg`) is what `apps/bootd` reads at boot, and the package database (`pkg.db`) is what the
//! package manager reads. Both are edited from one screen because a person thinks of them as one
//! subject, and both are handed back separately because the files have different owners and
//! different consequences — a `boot.cfg` a running bootd refuses means it launches **nothing**.

use alloc::format;
use alloc::string::String;

use symbian_decl_ui::widgets::Node;

use symbian_bootcfg::pkg::{self, PkgDb};
use symbian_bootcfg::BootConfig;
use symbian_ui::{
    chrome, Align, Canvas, Handled, Key, KeyEvent, Rect, Select, Softkey, Stepper, Theme, Toggle,
};

/// Delays edited in whole seconds, and the stepper's own number is those seconds. An earlier version
/// stored half-seconds and showed a row reading "2.0 s" beside a stepper reading `‹ 4 ›`; two numbers
/// for one value is worse than a longer range.
pub(crate) const SECOND_MS: u32 = 1_000;
pub(crate) const MAX_FIRST_DELAY_S: i32 = 60;
pub(crate) const MAX_CEILING: i32 = 20;

/// Auto-refresh intervals for the packages screen, and 0 means off.
pub(crate) const REFRESH_CHOICES: [u16; 6] = [0, 2, 5, 10, 30, 60];
pub(crate) const REFRESH_LABELS: [&str; 6] = ["off", "2 s", "5 s", "10 s", "30 s", "60 s"];

pub(crate) const ROW_ENABLED: usize = 0;
pub(crate) const ROW_FIRST: usize = 1;
pub(crate) const ROW_CEILING: usize = 2;
pub(crate) const ROW_REFRESH: usize = 3;
pub(crate) const ROWS: usize = 4;

fn choice_index(choices: &[u16], want: u16) -> usize {
    choices.iter().position(|&c| c >= want).unwrap_or(choices.len() - 1)
}

pub struct SettingsScreen {
    cfg: BootConfig,
    pkgs: PkgDb,
    // `pub(crate)` on the four the frozen `reference` copy reads. A field a sibling module must see
    // cannot stay module-private, and widening only these four keeps the rest of the state where it
    // was — see `crate::reference` for why the copy exists.
    pub(crate) focus: usize,
    pub(crate) enabled: Toggle,
    pub(crate) first_delay: Stepper,
    pub(crate) ceiling: Stepper,
    pub(crate) refresh: Select,
    changed: bool,
    pkgs_changed: bool,
    reset_requested: bool,
    back: bool,
}

impl SettingsScreen {
    pub fn new(cfg: BootConfig, pkgs: PkgDb) -> Self {
        Self {
            enabled: Toggle::new(cfg.enabled),
            first_delay: Stepper::new((cfg.first_delay_ms / SECOND_MS) as i32, 0, MAX_FIRST_DELAY_S),
            ceiling: Stepper::new(cfg.max_restarts as i32, 0, MAX_CEILING),
            refresh: Select::new(choice_index(&REFRESH_CHOICES, pkgs.refresh_s)),
            cfg,
            pkgs,
            focus: 0,
            changed: false,
            pkgs_changed: false,
            reset_requested: false,
            back: false,
        }
    }

    pub fn config(&self) -> &BootConfig {
        &self.cfg
    }

    pub fn packages(&self) -> &PkgDb {
        &self.pkgs
    }

    /// Whether `boot.cfg` needs writing. Consumed, so the caller writes once.
    pub fn take_changed(&mut self) -> bool {
        core::mem::take(&mut self.changed)
    }

    /// Whether `pkg.db` needs writing. A separate flag because it is a separate file with a separate
    /// owner.
    pub fn take_pkgs_changed(&mut self) -> bool {
        core::mem::take(&mut self.pkgs_changed)
    }

    /// Whether the user asked to clear the unsettled-boot counter.
    ///
    /// Deliberately an explicit action rather than something a screen does on open: safe mode exists
    /// because three boots in a row went wrong, and it should take a person to say that has been
    /// dealt with.
    pub fn take_reset(&mut self) -> bool {
        core::mem::take(&mut self.reset_requested)
    }

    pub fn back(&self) -> bool {
        self.back
    }

    fn store(&mut self) {
        self.cfg.enabled = self.enabled.on();
        self.cfg.first_delay_ms = (self.first_delay.value().max(0) as u32) * SECOND_MS;
        self.cfg.max_restarts = self.ceiling.value().clamp(0, u16::MAX as i32) as u16;
        self.changed = true;

        let refresh = REFRESH_CHOICES[self.refresh.selected().min(REFRESH_CHOICES.len() - 1)];
        if refresh != self.pkgs.refresh_s {
            self.pkgs.refresh_s = refresh;
            self.pkgs_changed = true;
        }
    }

    pub fn handle_key(&mut self, ev: KeyEvent) -> Handled {
        // An open dropdown is modal, the same as every other one in this project.
        if self.refresh.is_open() {
            let (h, _) = self.refresh.handle_key(ev, &REFRESH_LABELS);
            if h == Handled::Consumed {
                self.store();
            }
            return h;
        }
        match ev.key {
            Key::Softkey(Softkey::Right) => {
                self.back = true;
                Handled::Consumed
            }
            Key::Softkey(Softkey::Left) => {
                self.reset_requested = true;
                Handled::Consumed
            }
            Key::Up => {
                self.focus = self.focus.saturating_sub(1);
                Handled::Consumed
            }
            Key::Down => {
                self.focus = (self.focus + 1).min(ROWS - 1);
                Handled::Consumed
            }
            _ => {
                let h = match self.focus {
                    ROW_ENABLED => self.enabled.handle_key(ev),
                    ROW_FIRST => self.first_delay.handle_key(ev),
                    ROW_CEILING => self.ceiling.handle_key(ev),
                    _ => self.refresh.handle_key(ev, &REFRESH_LABELS).0,
                };
                if h == Handled::Consumed {
                    self.store();
                }
                h
            }
        }
    }

    /// One settings row, declared: a caption and the control that carries its value.
    ///
    /// The same shape and the same reservations as `BootScreen::entry_row_node` — read the long note
    /// there. In short: `ListItem` owns the band, the caption, the margins and the gap; the control
    /// owns its own pixels and nothing else; and neither carries an outbox, because `handle_key`
    /// below still owns the keyboard and mutates the imperative widgets directly. The value handed
    /// to each control is read back from the widget that owns it, so there is one copy of every
    /// number on this screen.
    ///
    /// The padding is pinned to `theme.metrics.pad` rather than left at `ListItem`'s `Gap::Base`,
    /// which is a pixel wider. That is a real divergence in the toolkit and it is written up in
    /// `entry_row_node`; here it only means this migration moved nothing.
    fn row_node(&self, row: usize, focused: bool, theme: &Theme<'_>) -> Node {
        use symbian_decl_ui::spacing::{Gap, Pad};
        use symbian_decl_ui::widgets::{ListItem, Stepper as StepperWidget, Switch};

        let pad = Pad::xy(Gap::Exact(theme.metrics.pad), Gap::None);
        let gap = Gap::Exact(theme.metrics.pad);
        let item = |label: alloc::string::String| {
            ListItem::new(label).plain().pad(pad).gap(gap).selected(focused).band(true)
        };
        match row {
            ROW_ENABLED => item("Boot manager enabled".into())
                .trailing(Switch::<()>::new(self.enabled.on()).focused(focused))
                .build(),
            ROW_FIRST => item("First launch delay (s)".into())
                .trailing(
                    StepperWidget::<()>::new(self.first_delay.value(), 0, MAX_FIRST_DELAY_S)
                        .focused(focused),
                )
                .build(),
            // The caption repeats the number the stepper is already showing, which is the defect
            // this module's own header complains about — "two numbers for one value is worse than a
            // longer range". It is left exactly as it shipped because removing it moves pixels, and
            // this pass is a migration and not a redesign. It is the first thing to fix on the pass
            // that is allowed to.
            _ => item(format!("Restart ceiling per boot: {}", self.ceiling.value()))
                .trailing(
                    StepperWidget::<()>::new(self.ceiling.value(), 0, MAX_CEILING)
                        .focused(focused),
                )
                .build(),
        }
    }

    pub fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
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

        // The three rows that are a caption and a control are declared; the dropdown below is not.
        // See `row_node` for what each of those two sentences costs and buys.
        for (rect, row) in [(r0, ROW_ENABLED), (r1, ROW_FIRST), (r2, ROW_CEILING)] {
            let focused = self.focus == row;
            crate::draw_row(c, &self.row_node(row, focused, theme), rect, theme, focused);
        }

        // The fourth row stays hand-drawn, for the reason written out in full at the policy row of
        // `BootScreen::draw_entry`: the declarative `Select` owns its open flag and its popup
        // highlight in the slot table and reports through an outbox, and this screen owns a
        // `symbian_ui::Select` that `handle_key` drives directly. Running both is two answers to
        // "is the popup open"; replacing this one is the declarative bridge, which is a different
        // change from this one.
        //
        // Which is why the caption is painted here, after the value and over the band: `Select::draw`
        // paints only the value, right-aligned.
        self.refresh.draw(c, r3, theme, &REFRESH_LABELS, self.focus == ROW_REFRESH);
        let col = if self.focus == ROW_REFRESH { p.selection_text } else { p.text };
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
        if self.refresh.is_open() {
            // Sized to its options rather than to the whole band — see the same call in `lib.rs`.
            let box_ = symbian_ui::select::popup_box(content, REFRESH_LABELS.len(), theme);
            self.refresh.draw_popup(c, box_, theme, &REFRESH_LABELS);
        }
    }
}

/// The label for a package's reopen setting: on or off, and nothing in between.
///
/// # Why this stopped being a number
///
/// It used to cycle through nine delays — 0, 5, 10, 20, 30, 45, 60, 90, 120 seconds — and every one
/// of them except the first was a lie about the mechanism. The supervisor does **not** sleep for
/// that long and then launch. It waits on an *observation*: `apps/bootctl` writes
/// [`INSTALLER_DONE_PATH`](symbian_bootcfg::INSTALLER_DONE_PATH) when it comes back to the front and
/// finds the platform's installer gone, and `update.rs` lifts both the floor and the hold the moment
/// it sees that — the wait ends when the install does, not when a clock says so.
///
/// So the number was only ever the **fallback floor**, for the case where the observation never
/// arrives. Which one of nine values that floor takes is not a judgement anybody can make per
/// package; it is a backstop. Offering it as the setting made a backstop look like the feature and
/// hid the feature entirely.
///
/// What is left is the only real question: reopen this one, or leave it where the installer left it.
///
/// # The cost, and why it is off by default
///
/// Turning it on keeps the supervisor in the update for longer: it stays in `Installing` until the
/// install is observed to end, then launches, then holds the journal open through `Proving` while
/// the new version stamps itself. Off, it commits as soon as the floor passes and stops thinking
/// about the package at all.
///
/// That is worth paying for a home screen, whose absence is the thing the user is staring at. It is
/// not worth paying for everything, which is why nothing has it until somebody asks.
pub fn reopen_label(db: &PkgDb, uid3: u32) -> String {
    String::from(if db.reopen(uid3).is_some() { "yes" } else { "no" })
}

/// Turn a package's reopen-after-install on or off, adopting the package if it has no row yet —
/// otherwise the choice would have nowhere to be written and would silently do nothing.
///
/// On stores [`INSTALL_SETTLE_S`](symbian_bootcfg::update::INSTALL_SETTLE_S), which is the fallback
/// floor and *not* a delay the user chose — see [`reopen_label`]. Storing the same number the
/// supervisor would have used anyway means turning the toggle on changes exactly one thing: whether
/// the package is launched at the end.
///
/// Any non-zero value reads as on, so a database written by the version that cycled nine delays
/// keeps working and keeps its old floor. There is nothing to migrate.
pub fn step_reopen(db: &mut PkgDb, uid3: u32, name: String) {
    let on = db.reopen(uid3).is_some();
    if db.get(uid3).is_none() {
        db.ensure(pkg::ManagedPkg::new(uid3, name));
    }
    if let Some(p) = db.get_mut(uid3) {
        p.settle_s = if on { 0 } else { symbian_bootcfg::update::INSTALL_SETTLE_S as u16 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::testing::{with_canvas, with_theme, SCREEN};
    use symbian_ui::Palette;
    use symbian_ui::gfx::Size;

    fn screen() -> SettingsScreen {
        SettingsScreen::new(BootConfig::default(), PkgDb::default())
    }

    fn press(s: &mut SettingsScreen, k: Key) -> Handled {
        s.handle_key(KeyEvent::new(k))
    }

    #[test]
    fn the_two_files_are_flagged_separately() {
        // Different owners and different consequences: a boot.cfg a running bootd refuses means it
        // launches nothing.
        let mut s = screen();
        press(&mut s, Key::Down);
        press(&mut s, Key::Select); // the first-delay stepper
        assert!(s.take_changed(), "boot.cfg");
        assert!(!s.take_pkgs_changed(), "and pkg.db was not touched");

        for _ in 0..3 {
            press(&mut s, Key::Down);
        }
        press(&mut s, Key::Select); // open the dropdown
        press(&mut s, Key::Down);
        press(&mut s, Key::Select); // commit
        assert!(s.take_pkgs_changed(), "pkg.db");
        assert_eq!(s.packages().refresh_s, 2);
    }

    #[test]
    fn clearing_safe_mode_takes_a_person_and_is_consumed() {
        let mut s = screen();
        assert!(!s.take_reset());
        press(&mut s, Key::Softkey(Softkey::Left));
        assert!(s.take_reset());
        assert!(!s.take_reset(), "asked once, done once");
    }

    #[test]
    fn back_leaves() {
        let mut s = screen();
        press(&mut s, Key::Softkey(Softkey::Right));
        assert!(s.back());
    }

    #[test]
    fn the_focus_cannot_walk_off_either_end() {
        let mut s = screen();
        for _ in 0..9 {
            press(&mut s, Key::Down);
        }
        assert_eq!(s.focus, ROWS - 1);
        for _ in 0..9 {
            press(&mut s, Key::Up);
        }
        assert_eq!(s.focus, 0);
    }

    #[test]
    fn the_master_switch_reaches_the_config() {
        let mut s = screen();
        assert!(s.config().enabled);
        press(&mut s, Key::Select);
        assert!(!s.config().enabled);
        assert!(s.take_changed());
    }

    #[test]
    fn it_draws_in_both_palettes() {
        for palette in [Palette::DARK, Palette::LIGHT] {
            let mut s = screen();
            let (_, px) = with_canvas(Size::new(SCREEN.width(), SCREEN.height()), |c| {
                with_theme(palette, |t| s.draw(c, t));
            });
            let blank = with_canvas(Size::new(SCREEN.width(), SCREEN.height()), |_| {}).1;
            assert_ne!(px, blank, "{palette:?}");
        }
    }

    #[test]
    fn toggling_reopen_adopts_a_package_that_has_no_row() {
        // Otherwise the choice has nowhere to be written and does nothing, quietly.
        let mut db = PkgDb::default();
        step_reopen(&mut db, 0xE0AA_0000, String::from("Launcher"));
        assert!(db.get(0xE0AA_0000).is_some());
        assert_eq!(reopen_label(&db, 0xE0AA_0000), "yes");
        assert_eq!(reopen_label(&db, 0xDEAD), "no", "a package nobody asked about is off");
    }

    #[test]
    fn the_toggle_is_a_toggle_and_not_a_cycle() {
        // Two presses come back to where they started. The nine-delay cycler took *nine* to do
        // that, which is what a person turning something off has to sit through.
        let mut db = PkgDb::default();
        step_reopen(&mut db, 0xE0AA_0000, String::from("Launcher"));
        assert!(db.reopen(0xE0AA_0000).is_some(), "on");
        step_reopen(&mut db, 0xE0AA_0000, String::from("Launcher"));
        assert_eq!(db.reopen(0xE0AA_0000), None, "and off again");
        step_reopen(&mut db, 0xE0AA_0000, String::from("Launcher"));
        assert!(db.reopen(0xE0AA_0000).is_some(), "and on again");
    }

    #[test]
    fn a_database_from_the_nine_delay_version_still_reads() {
        // The wire format did not change — `settle_s` is still a `u16` — so every delay the old
        // cycler could store has to keep meaning *on*, with its own value left alone as the fallback
        // floor. A migration that rewrote them would be changing a stored number to say the same
        // thing.
        let mut db = PkgDb::default();
        db.ensure(pkg::ManagedPkg::new(0xE0AA_0000, String::from("Launcher")));
        for old in [5u16, 10, 20, 30, 45, 60, 90, 120] {
            db.get_mut(0xE0AA_0000).unwrap().settle_s = old;
            assert_eq!(reopen_label(&db, 0xE0AA_0000), "yes", "{old} s should read as on");
            assert_eq!(db.reopen(0xE0AA_0000), Some(old), "and keep its floor");
        }
        db.get_mut(0xE0AA_0000).unwrap().settle_s = 0;
        assert_eq!(reopen_label(&db, 0xE0AA_0000), "no");
    }
}
