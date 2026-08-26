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

pub mod settings;

// The frozen "before" the parity harness compares against. Public so `examples/parity.rs` can reach
// it; `#[doc(hidden)]` because it is an instrument, not part of what this crate offers a caller.
#[doc(hidden)]
pub mod reference;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use symbian::apps::AppInfo;
use symbian_bootcfg::config::{Entry, Policy};
use symbian_bootcfg::pkg::{Offer, Version};
use symbian_bootcfg::status::State;
use symbian_bootcfg::update::Proof;
use symbian_bootcfg::{BootConfig, BootStatus, BOOTCTL_UID, BOOTD_UID};
use symbian_decl_ui::cache::UiCache;
use symbian_decl_ui::constraints::Constraints;
use symbian_decl_ui::layout;
use symbian_decl_ui::theme::FontRole;
use symbian_decl_ui::widgets::Node;
use symbian_ui::Ground;
use symbian_ui::{
    chrome, modal::Answer, Align, App, AppPicker, Canvas, Handled, Key, KeyEvent, ListState, Modal,
    PickerAction, PickerItem, Rect, Select, Softkey, Stepper, Tabs, Theme, Toggle, Uniform,
};

/// Two views of one subject, which is what a tab strip is for.
///
/// It was five — `List`, `Entry`, `Setup`, `Boot`, `Pkgs` — and three of them did not belong. `Entry`
/// was the *detail of a row selected on another tab*, which is a child pretending to be a sibling and
/// cannot be guessed at from a label. `Setup` was settings, which are a section of their own. `Pkgs`
/// was a whole second subject, and grew four tabs of its own behind a door.
///
/// So the hierarchy is three levels now: sections in the drawer, tabs inside a section, and a pushed
/// screen for one entry. See `symbian_ui::drawer`.
/// What the removal question can be answered with.
///
/// An enum and not a bool, so the call site reads as the answer rather than as `true`. There are two
/// values today and the type is what keeps a third from arriving as a second bool.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Confirm {
    Remove,
}

const TABS: [&str; 2] = ["Order", "Last boot"];
const TAB_ORDER: usize = 0;
const TAB_LAST: usize = 1;

/// The Entry tab's rows, by identity rather than by position: "Restart limit" only means anything
/// under `Times`, so it is absent otherwise — both from the layout and from the cursor's path.
/// Focus is held as one of these rather than an index, so hiding a row cannot silently move the
/// cursor onto a different setting.
const ROW_ENABLED: usize = 0;
const ROW_POLICY: usize = 1;
const ROW_RETRIES: usize = 2;
const ROW_DELAY: usize = 3;

/// Both delays are edited in **whole seconds**, and the stepper's own number is those seconds.
/// An earlier version stored half-seconds and five-second jumps to keep the ranges short, and the
/// rendered screen showed the cost immediately: a row reading "Delay before launch: 2.0 s" beside
/// a stepper reading `‹ 4 ›`. Two numbers for one value is worse than a longer range.
const SECOND_MS: u32 = 1_000;
const MAX_ENTRY_DELAY_S: i32 = 30;
const MAX_RETRIES: i32 = 10;

pub struct BootScreen {
    cfg: BootConfig,
    status: Option<BootStatus>,
    roster: Vec<AppInfo>,

    tabs: Tabs,
    last_tab: usize,
    list: ListState,
    move_mode: bool,
    picker: Option<AppPicker>,

    /// Which row's detail is open, or `None` on the list.
    ///
    /// A pushed screen, not a tab. The widgets and the routing are the same ones the `Entry` tab
    /// used; what changed is that it is now reached from the row it describes and Back returns
    /// there — so the thing on screen and the thing selected can no longer disagree.
    entry_open: bool,
    entry_focus: usize,
    entry_enabled: Toggle,
    entry_policy: Select,
    entry_retries: Stepper,
    entry_delay: Stepper,



    changed: bool,
    reset_requested: bool,
    /// The "remove this entry?" question, while it is up.
    ///
    /// A `Modal` rather than a mode flag, because a choice must carry what it *means*: the label and
    /// the value are written together and cannot drift apart. It also draws its own softkey bar,
    /// which is the defect this project fixed by hand in three overlays last week — here it is
    /// structural.
    confirm: Option<Modal<Confirm>>,
    back: bool,
}

/// What the screen is asking `apps/bootctl` to do: stage this file, check its digest, close the
/// target, write the journal, and hand the `.sis` to an installer.
///
/// A request and not an action, for the same reason nothing else here touches a file — the screen
/// is host-tested and the device work is the app's. It also means the confirmation the user gave is
/// a value that can be asserted on in a test, rather than a side effect that happened somewhere.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct InstallRequest {
    pub uid3: u32,
    /// What we believe is installed now, for the journal's rollback bookkeeping.
    pub from: Option<Version>,
    pub to: Version,
    /// The candidate's full path, in the directory it was found in.
    pub sis: String,
    /// SHA-256 of the file, when the screen had it. The journal records it at commit, so the next
    /// comparison is against bytes rather than a version number — see
    /// `symbian_bootcfg::ManagedPkg::installed_sha`.
    ///
    /// `None` when nothing needed it computed, which is every case except an equal-version rebuild.
    /// The app hashes the file it stages regardless, so what is recorded is always the real digest;
    /// this field is only what the *screen* knew.
    pub sha256: Option<[u8; 32]>,
    /// What the row was offering when the user confirmed, so the log and the journal say *why* an
    /// install happened rather than only that it did.
    pub offer: Offer,
    /// The package's own name, for the row this install adopts into the database.
    pub name: String,
    /// What committing this install can honestly require of it.
    pub proof: Proof,
}


/// Measure, place and draw one declared row into `band`.
///
/// The three passes `draw_list` runs per row, lifted out because a second screen now needs them.
/// The cache is per row and dies with it, which is the cost these screens are choosing: an entry
/// detail is four rows and a boot list is five, not two hundred.
///
/// `chrome::selection` goes down first and full-bleed when the row is focused, before the node is
/// drawn. `ListItem::band(true)` would paint a band of its own, but only across the rect the row is
/// given, and this is the same order `draw_list` uses — the band is the only thing on a keypad
/// device saying where you are, so nothing a row paints may cover it.
///
/// `Ground::Band` on the focused row, or every `Ink::Dim` inside it resolves against the *page*
/// underneath the highlight and comes out unreadable on a palette whose band is bright. A row drawn
/// outside a `ScrollList` has to apply that rule itself.
fn draw_row(c: &mut Canvas<'_>, node: &Node, band: Rect, theme: &Theme<'_>, focused: bool) {
    if focused {
        chrome::selection(c, band, theme);
    }
    let t = theme.on(if focused { Ground::Band } else { Ground::Page });
    let mut cache = UiCache::with_capacity(node.slot_count() + 4);
    layout::measure_node(node, 0, Constraints::tight(band.width(), band.height()), &t, &mut cache);
    layout::layout_node(node, 0, band, &mut cache, &t);
    layout::draw_node(node, 0, &cache, c, &t);
}

impl BootScreen {
    pub fn new(cfg: BootConfig, status: Option<BootStatus>, roster: Vec<AppInfo>) -> Self {
        let mut me = Self {
            cfg,
            status,
            roster,
            tabs: Tabs::new(),
            last_tab: 0,
            list: ListState::new(),
            move_mode: false,
            picker: None,
            entry_open: false,
            entry_focus: 0,
            entry_enabled: Toggle::new(true),
            entry_policy: Select::new(0),
            entry_retries: Stepper::new(3, 1, MAX_RETRIES),
            entry_delay: Stepper::new(2, 0, MAX_ENTRY_DELAY_S),
            changed: false,
            reset_requested: false,
            confirm: None,
            back: false,
        };
        me.load_entry();
        me
    }

    /// Take a config edited elsewhere — the settings section, which owns the master switch, the first
    /// delay and the ceiling.
    ///
    /// The rows keep their own place: replacing the config must not move the cursor, or changing a
    /// global setting would silently reselect a different entry.
    pub fn replace_config(&mut self, cfg: BootConfig) {
        self.cfg = cfg;
        self.changed = true;
        if self.list.selected >= self.list_rows() {
            self.list.selected = self.list_rows().saturating_sub(1);
        }
    }

    /// The edited config, for the caller to persist.
    pub fn config(&self) -> &BootConfig {
        &self.cfg
    }

    /// True once the right softkey was pressed.
    pub fn back(&self) -> bool {
        self.back
    }

    /// The same, consumed.
    ///
    /// Back means *go up one level* now, and the level above this screen is the navigator — so the
    /// caller acts on it and this screen carries on existing. Left as a latch it would open the
    /// drawer again on every key that followed.
    pub fn take_back(&mut self) -> bool {
        core::mem::take(&mut self.back)
    }

    /// Whether anything was edited since the last call. Consumed, so the caller writes once.
    pub fn take_changed(&mut self) -> bool {
        core::mem::take(&mut self.changed)
    }

    /// Whether the user asked to clear the safe-mode counter. Consumed.
    pub fn take_reset(&mut self) -> bool {
        core::mem::take(&mut self.reset_requested)
    }

    // Everything about packages moved to `symbian_pkgui`: the catalogue, the repositories, the
    // queue, the install and the settings that belong to them. This crate is the boot list, one
    // entry's detail, and the last boot — and it holds none of that state any more, which is why the
    // Pkgs summary went with it.

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
        self.entry_delay.set((e.delay_ms / SECOND_MS) as i32);
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
        let delay_ms = (self.entry_delay.value().max(0) as u32).saturating_mul(SECOND_MS);
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

    /// The name of entry `i`, or its hex UID when it has none. What the detail screen is titled with,
    /// so it says which entry it is about rather than leaving that to be inferred.
    fn row_name(&self, i: usize) -> String {
        match self.cfg.entries.get(i) {
            Some(e) if !e.name.is_empty() => e.name.clone(),
            Some(e) => format!("[{:08X}]", e.uid3),
            None => String::from("Entry"),
        }
    }

    /// One row's text on the Order tab.
    /// One boot entry as a row: its position, its name, and one chip saying what it will do.
    ///
    /// # What this screen is for, which decides what a row holds
    ///
    /// You come here to change the **order** and to find out **why something did not start**.
    /// Everything else about an entry — the restart policy in full, the delay in tenths, the restart
    /// ceiling — has a screen of its own, one press away, and putting it here too meant every row
    /// carried a sentence:
    ///
    /// ```text
    /// 1. Telegram  · always · +2.0s · home
    /// ```
    ///
    /// Five facts, middot-separated, read left to right at body size. The first rewrite made it two
    /// lines — name over detail, the shape the packages tab uses — and that was worse for *this*
    /// screen, not better: two-line rows halve how many entries fit, and a list you reorder is a list
    /// whose whole point is seeing the neighbours you are moving between. Four rows and a sliced
    /// fifth is not an order you can see.
    ///
    /// So: one line, and the detail promoted to a **chip**, which is a state you recognise by shape
    /// and colour instead of a phrase you read. `crash loop` in the warning colour is the one thing
    /// on this screen somebody is looking for.
    /// One boot row's tree, for a test that has to measure it on the **real** atlases.
    ///
    /// `#[doc(hidden)]` and named for what it is. It exists because
    /// `the_order_tabs_chip_is_placed_narrower_than_it_asked_for` in `examples/parity.rs` needs the
    /// node, and the host test atlas cannot answer the question that test asks: every glyph in it
    /// has the same advance, so a width that depends on which letters a word contains is a width it
    /// cannot see. See the catalogue's note on what the one-glyph atlas can and cannot measure.
    #[doc(hidden)]
    pub fn row_node_for_test(&self, i: usize) -> Node {
        self.row_node(i)
    }

    fn row_node(&self, i: usize) -> Node {
        use symbian_decl_ui::widgets::{Chip, ListItem, Text};

        let sel = i == self.list.selected;
        if i >= self.cfg.entries.len() {
            // No `Icon::Plus` in the set, and inventing one for a single row is how an icon catalogue
            // grows a long tail nobody draws twice. A `+` at strong weight is what the old label used
            // and it reads correctly at this size.
            return ListItem::new("Add an app…")
                .selected(sel)
                .band(true)
                .leading(Text::new("+").font(FontRole::Strong).dim())
                .build();
        }
        let e = &self.cfg.entries[i];
        let name = if e.name.is_empty() { format!("[{:08X}]", e.uid3) } else { e.name.clone() };

        // The position is the meaning on this screen — it *is* the boot order — so it stays visible,
        // but as its own quiet column rather than as two characters of the name. A dim number that
        // lines up down the left edge reads as an index; `1. ` inside the label reads as part of the
        // app's name, which is how `4. Web` came out looking like a product with a version in it.
        let index = Text::new(format!("{}", i + 1)).font(FontRole::Small).dim();
        let mut row = ListItem::new(name).selected(sel).band(true).leading(index);

        // One chip, and the order below is a priority rather than a preference. A row has room for
        // exactly one at 320 pixels once the name and the index have theirs — measured, not assumed.
        //
        // `.selected(sel)` on the chip is not optional: a calm chip fills with `divider`, a colour
        // chosen against the *page*, so on the selection band it is a pill-shaped hole. That is
        // `chrome::control_colors`' subject, one layer up.
        //
        // Moving outranks everything, because while the row is in the air what it does at boot is not
        // what the user is thinking about — where it lands is.
        let chip = if sel && self.move_mode {
            None
        } else if e.auto_disarmed {
            // Why the row is off beats the standing "home" mark: one is a fault to act on, the other
            // is a property that has been true since the phone was set up.
            Some(Chip::warn("crash loop"))
        } else if !e.enabled {
            Some(Chip::calm("off"))
        } else {
            Some(match e.policy {
                Policy::Never => Chip::calm("once"),
                Policy::Times(n) => Chip::calm(format!("retry {n}")),
                Policy::Always => Chip::fresh("always"),
            })
        };

        row = match chip {
            Some(chip) => row.trailing(chip.selected(sel)),
            // The arrows say which way the row can go, which is the question the mode raises and the
            // old screen answered with a single `↕` that never changed at either end of the list.
            None => row.trailing(Text::new(self.move_hint(i)).font(FontRole::Strong)),
        };

        // The critical mark is shown and never edited here. The flag is written by the system that
        // owns the entry — the launcher marks itself on a fresh phone — and a switch for it would let
        // someone quietly un-mark their home screen, or mark five apps and hand the global ceiling
        // back its teeth. What the user needs from this screen is to know why the phone is watching
        // this row closely; changing it is the owning app's business. It rides on the name rather
        // than taking the chip slot, which the row's state has a better claim to.
        if e.critical {
            row = row.leading(Text::new(format!("{}\u{2022}", i + 1)).font(FontRole::Small).dim());
        }
        row.build()
    }

    /// Which way this row can move, as arrows, for the row that is in the air.
    ///
    /// A single `↕` on every row was the old answer and it lies at both ends of the list: the first
    /// entry cannot go up and the last cannot go down, and a control that offers a move it will
    /// refuse is the same defect `FocusRing`'s edge policies exist to avoid.
    fn move_hint(&self, i: usize) -> &'static str {
        let last = self.cfg.entries.len().saturating_sub(1);
        match (i > 0, i < last) {
            (true, true) => "\u{2195}",
            (true, false) => "\u{2191}",
            (false, true) => "\u{2193}",
            (false, false) => "",
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
                    // Opens the row's own screen. It used to flip the enabled switch in place, with
                    // the policy and the delay on a separate tab — which is how somebody ended up
                    // editing one row while looking at another.
                    self.load_entry();
                    self.entry_open = true;
                }
                Handled::Consumed
            }
            // Asked, not done. Deleting a boot entry is not undoable and the key that did it is not
            // written anywhere on the screen — a person who pressed Backspace meaning "go back" lost
            // the row they were standing on and had nothing to tell them why.
            Key::Backspace | Key::Delete => {
                let Some(i) = self.selected_entry() else { return Handled::Ignored };
                self.confirm = Some(
                    Modal::new(
                        format!("Remove {}?", self.row_name(i)),
                        "It will not be started at boot. The application itself is not touched.",
                    )
                    .choice("Remove", Confirm::Remove)
                    // No `Keep` choice: Back is how you decline, it is labelled, and a list of two
                    // where one of them is "do nothing" invites the cursor to sit on the dangerous
                    // one. `default_choice` would be the other answer and it is worse — a default
                    // that removes is a default nobody wants.
                    // Both labels, and neither is optional here: `Modal`'s defaults are Portuguese
                    // — they match `tg`, which is the app it was written for — and this screen is
                    // English. A shared widget carrying a language is a decision every caller
                    // inherits without being asked, so every caller has to answer it.
                    .action_label("Remove")
                    .back_label("Keep"),
                );
                Handled::Consumed
            }
            Key::Softkey(Softkey::Left) => {
                // Ignored, not consumed, when there is nothing to move. The bar is blank on the
                // "Add an app…" row — `softkeys()` returns `None` for it — so this was a labelled
                // nothing eating a key: whatever might have wanted `Softkey::Left` above this screen
                // never saw it, and the user pressed a blank key that reported success.
                //
                // The distinction is the whole of `Handled`: consuming says *I dealt with this*, and
                // a screen that says so about a key it did nothing with is lying to its host.
                if self.selected_entry().is_none() {
                    return Handled::Ignored;
                }
                self.move_mode = true;
                Handled::Consumed
            }
            _ => Handled::Ignored,
        }
    }

    /// The Entry rows that currently exist, in order. "Restart limit" is only one of them under
    /// `Times`.
    fn entry_rows(&self) -> Vec<usize> {
        let mut v = alloc::vec![ROW_ENABLED, ROW_POLICY];
        if self.entry_policy.selected() == 1 {
            v.push(ROW_RETRIES);
        }
        v.push(ROW_DELAY);
        v
    }

    /// Step the cursor through the rows that exist, so a hidden row is skipped rather than
    /// landed on.
    fn move_entry_focus(&mut self, down: bool) {
        let rows = self.entry_rows();
        let at = rows.iter().position(|&r| r == self.entry_focus).unwrap_or(0);
        let next = if down { (at + 1).min(rows.len() - 1) } else { at.saturating_sub(1) };
        self.entry_focus = rows[next];
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
                // Choosing a policy can add or remove the "Restart limit" row under the cursor.
                if !self.entry_rows().contains(&self.entry_focus) {
                    self.entry_focus = ROW_POLICY;
                }
            }
            return h;
        }
        match ev.key {
            Key::Up => {
                self.move_entry_focus(false);
                Handled::Consumed
            }
            Key::Down => {
                self.move_entry_focus(true);
                Handled::Consumed
            }
            _ => {
                let h = match self.entry_focus {
                    ROW_ENABLED => self.entry_enabled.handle_key(ev),
                    ROW_POLICY => self.entry_policy.handle_key(ev, &Policy::LABELS).0,
                    ROW_RETRIES => self.entry_retries.handle_key(ev),
                    _ => self.entry_delay.handle_key(ev),
                };
                if h == Handled::Consumed {
                    self.store_entry();
                }
                h
            }
        }
    }


    // ---------------------------------------------------------------- drawing

    fn draw_list(&mut self, c: &mut Canvas<'_>, content: Rect, theme: &Theme<'_>) {
        // Precompute the rows so the draw closure borrows a plain Vec, not `self`.
        let nodes: Vec<Node> = (0..self.list_rows()).map(|i| self.row_node(i)).collect();
        let rows = Uniform { count: nodes.len(), height: theme.metrics.row_h };
        let sel = self.list.selected;
        self.list.draw_visible(c, &rows, content, |c, i, row| {
            // The band goes down first and full-bleed, exactly as `ScrollList` does it: with no
            // pointer this is the only thing saying where you are, so a row drawing its own ground
            // must not cover it.
            if i == sel {
                chrome::selection(c, row, theme);
            }
            // Three passes on a subtree — measure, place, draw — the same ones the declarative bridge
            // runs on a whole screen. The cache is per row and dies with it, which is the cost this
            // screen is choosing: a boot list is five entries, not two hundred.
            //
            // `Ground::Band` on the selected row, or the row's `Ink::Dim` detail line keeps resolving
            // against the *page* underneath the highlight and comes out unreadable on a palette whose
            // band is bright. That is the ground rule, and a row that paints outside `ScrollList` has
            // to apply it itself.
            // The band is full-bleed but the *content* is not: the scrollbar owns a strip down the
            // right edge, and a row laid out to the full width puts its trailing chip underneath it.
            // The old code never noticed because it drew a left-aligned string; a chip is the first
            // thing on this screen that reaches the right edge.
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
        chrome::scrollbar(c, content, theme, self.list.scrollbar(&rows, content.height()));
    }

    /// One row of the entry detail, declared.
    ///
    /// Three of the four rows are `ListItem` + a control, which is the shape the catalogue calls a
    /// settings row and the shape `row_node` above already uses for the boot list. What that buys is
    /// not fewer lines — it is that the band, the caption, the margins and the gap stop being four
    /// separate decisions made here and become `ListItem`'s, with a parity test behind them.
    ///
    /// # The controls carry no outbox, and that is not an omission
    ///
    /// `Switch` and `Stepper` normally take `.out(outbox, Msg)` and report a press. These do not,
    /// because this screen never dispatches a key into the tree: `route_entry` owns the keyboard and
    /// mutates `entry_enabled`/`entry_retries`/`entry_delay` directly, exactly as it did before. The
    /// node is built, measured, placed and drawn, and then dropped — it is a *painter* here, not a
    /// control.
    ///
    /// That is the deliberate half-measure this crate is in the middle of, and it is worth naming:
    /// the widgets own the pixels, the screen still owns the state and the keys. Wiring the outbox
    /// would mean giving `BootScreen` a `SlotTable`, a message type and a drain — the declarative
    /// bridge — and that is a different change from this one, with a different risk.
    ///
    /// The value passed to each control is read from the imperative widget that owns it, so there is
    /// exactly one copy of every number on this screen. A `Switch` that held its own `bool` beside
    /// `entry_enabled`'s would be the second source of truth this comment exists to refuse.
    fn entry_row_node(&self, row: usize, focused: bool, theme: &Theme<'_>) -> Node {
        use symbian_decl_ui::widgets::{ListItem, Stepper as StepperWidget, Switch};

        // ### `metrics.pad` is 5 and `Gap::Base` is 6, and that is not a rounding error
        //
        // A hand-written row insets itself with `r.inset_xy(theme.metrics.pad, 0)`, which is **5**
        // pixels. `ListItem`'s default padding is `Gap::Base`, which `Space::default` puts at **6**.
        // Every row migrated without saying anything therefore moves one pixel right, its control
        // moves one pixel left, and the parity harness reported exactly that — 1363 pixels across
        // three rows, which is what a one-pixel horizontal shift of two strings and two controls
        // costs.
        //
        // That divergence is already in this crate and predates this change: `row_node` above is a
        // `ListItem` and sits at 6, while every hand-drawn row sits at 5. Nobody has seen it because
        // the two have never been on screen together — the boot list is one screen and the entry
        // detail is another.
        //
        // The pixels are pinned to `metrics.pad` here, so this migration moves nothing. Which of the
        // two numbers is *right* is a separate question and a real one: `Space::base` is documented
        // as "the default gap, and the side margin of list rows", which says the token intends to be
        // the answer, and `metrics.pad` intends to be the answer too. They should be one number, and
        // deciding that is a change to the whole SDK's margins rather than to this screen.
        let pad = symbian_decl_ui::spacing::Pad::xy(
            symbian_decl_ui::spacing::Gap::Exact(theme.metrics.pad),
            symbian_decl_ui::spacing::Gap::None,
        );
        // The same number for the gap, for the same reason: the gap is what the caption's flexed
        // width stops at, so a wider one truncates a long caption a character earlier.
        let gap = symbian_decl_ui::spacing::Gap::Exact(theme.metrics.pad);

        // `M = ()`: the message type of a control with nowhere to send anything. It is not a
        // placeholder for a real message — see above for why there is no channel to give it.
        match row {
            ROW_ENABLED => ListItem::new("Start at boot")
                .plain()
                .pad(pad)
                .gap(gap)
                .selected(focused)
                .band(true)
                .trailing(Switch::<()>::new(self.entry_enabled.on()).focused(focused))
                .build(),
            ROW_RETRIES => ListItem::new("Restart limit")
                .plain()
                .pad(pad)
                .gap(gap)
                .selected(focused)
                .band(true)
                .trailing(
                    StepperWidget::<()>::new(self.entry_retries.value(), 1, MAX_RETRIES)
                        .focused(focused),
                )
                .build(),
            _ => ListItem::new("Delay before launch (s)")
                .plain()
                .pad(pad)
                .gap(gap)
                .selected(focused)
                .band(true)
                .trailing(
                    StepperWidget::<()>::new(self.entry_delay.value(), 0, MAX_ENTRY_DELAY_S)
                        .focused(focused),
                )
                .build(),
        }
    }

    fn draw_entry(&mut self, c: &mut Canvas<'_>, content: Rect, theme: &Theme<'_>) {
        if self.selected_entry().is_none() {
            chrome::placeholder(c, content, theme, "Pick a row on the List tab.");
            return;
        }
        let rh = theme.metrics.row_h;
        let mut rest = content;
        for row in self.entry_rows() {
            let (r, below) = rest.split_top(rh);
            rest = below;
            let focused = self.entry_focus == row;
            match row {
                ROW_ENABLED | ROW_RETRIES | ROW_DELAY => {
                    let node = self.entry_row_node(row, focused, theme);
                    draw_row(c, &node, r, theme, focused);
                }
                _ => {
                    // ### The one row on this screen that is still hand-drawn, and why
                    //
                    // The other three rows above became `ListItem` + a control, and this one could
                    // not follow them. `symbian_decl_ui::Select` is not a painter the way `Switch`
                    // and `Stepper` are: it owns the open flag and the popup's highlight in the
                    // **slot table**, it hands back *two* nodes — a field and a popup that must be
                    // the last layer of a `Stack` over the content band — and it reports a committed
                    // choice through an `Outbox`.
                    //
                    // This screen owns a `symbian_ui::Select` and `route_entry` gives it keys
                    // directly. Adopting the declarative one means either running both, which is two
                    // answers to "is the popup open" and one of them wrong, or giving `BootScreen` a
                    // slot table, a message enum and an outbox drain — the declarative bridge, which
                    // is a different change with a different risk from moving a row's pixels.
                    //
                    // It would also not be pixel-neutral, which this pass is: the declarative popup
                    // is a `Stack` layer sized by the widget, and this one is `popup_box(content)`
                    // sized to its options against the bottom of the band.
                    //
                    // So it stays, including the part that reads as a hack: `Select::draw` paints
                    // only the value, right-aligned, with the selection band behind it, so the
                    // caption has to go on *afterwards*, over that band, on the left.
                    self.entry_policy.draw(c, r, theme, &Policy::LABELS, focused);
                    let p = &theme.palette;
                    let col = if focused { p.selection_text } else { p.text };
                    let cell = r.inset_xy(theme.metrics.pad, 0);
                    c.draw_text_in(cell, "When it stops", theme.fonts.body, col, Align::Start);
                }
            }
        }

        // The dropdown floats over the rows beneath it, so it is drawn last.
        if self.entry_policy.is_open() {
            // `popup_box` and not the raw band: handed the whole content area the popup filled it,
            // and a list that fills the screen reads as a place you navigated to rather than as a
            // list that opened over the row you are editing. Sized to its options it sits against
            // the bottom with the rows still visible above it, which is the S60 list query and what
            // the declarative `Select` has been doing all along — this call site simply never asked
            // for it.
            let box_ = symbian_ui::select::popup_box(content, Policy::LABELS.len(), theme);
            self.entry_policy.draw_popup(c, box_, theme, &Policy::LABELS);
        }
    }


    /// The last boot, one line per entry.
    ///
    /// # What was migrated, and what was deliberately left alone
    ///
    /// The **ink** is declared: each line is a `Text` with a font role and an [`Ink`] role, placed by
    /// the same three passes every other row on these two screens goes through. Nothing here reaches
    /// for a palette field or calls `draw_text_in` any more.
    ///
    /// The **content** is not. Every line is still a sentence built with `format!` —
    /// `"Web — auto-disabled: crash loop, 3 restarts"` — and that is exactly the shape `row_node`
    /// above argues against at length: five facts middot-separated, read left to right. The right
    /// answer is the one the Order tab already uses, `ListItem` with the name as the title and the
    /// state as a `Chip`, with `Chip::warn` on the crash loop so the one line somebody came here to
    /// find is the one line that is coloured.
    ///
    /// It is not done here because **it moves pixels, and this pass is a migration with a parity
    /// mandate**. `examples/parity.rs` compares today's render against a frozen copy of the code as
    /// it stood before this work started, and it reports `identical` — which is the only evidence
    /// that the rewrite of the entry detail and the settings screen changed nothing. A redesign of
    /// this tab in the same pass would turn that report red, and a red parity report cannot
    /// distinguish "the redesign we meant" from "the row that moved by accident". The two changes
    /// have to be separated to stay checkable, and the migration is the one with the mandate.
    ///
    /// What the redesign needs, written down so the next pass is cheap: a row height (these lines are
    /// `line_height() + 2` and a `ListItem` wants `RowHeight::Row`, which is 38 — five entries plus a
    /// heading would no longer fit the band, so the tab needs a real `ScrollList`), a `SectionHeader`
    /// or a `Card` for the first line, which is a summary and not an entry, and a decision about
    /// `last_rc`, which is a number a chip has no room for.
    fn draw_boot(&mut self, c: &mut Canvas<'_>, content: Rect, theme: &Theme<'_>) {
        use symbian_decl_ui::layout::CrossAlign;
        use symbian_decl_ui::spacing::{Gap, Pad};
        use symbian_decl_ui::widgets::{Ink, Row as TextRow, Text};

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

        // One line each, not a row each: these are `line_height() + 2` apart, which is a paragraph
        // rather than a list, and it is why nothing here is a `ListItem` yet. See above.
        let rows = Uniform { count: lines.len(), height: theme.fonts.body.line_height() + 2 };
        // A fresh ListState each draw: this tab is read-only, so there is no selection to keep and
        // nothing to scroll with — the rows either fit or the report is longer than the screen.
        let view = &mut ListState::new();
        let pad = Pad::xy(Gap::Exact(theme.metrics.pad), Gap::None);
        view.draw_visible(c, &rows, content, |c, i, row| {
            // The first line is the verdict and the rest are its evidence, so it takes the accent
            // and the strong face. `Ink::Accent` rather than `palette.accent` because a role
            // resolves against the ground it is drawn on, and a literal cannot — that is the whole
            // argument `symbian_ui::Ground` exists to make. Nothing here is ever on a band today,
            // and the role is what keeps that from mattering the day something is.
            let (font, ink) = if i == 0 {
                (FontRole::Strong, Ink::Accent)
            } else {
                (FontRole::Body, Ink::Text)
            };
            let line = Text::new(&lines[i]).font(font).ink(ink);
            // `flex(1)` so the text claims the width, and `CrossAlign::Stretch` so it is handed the
            // whole band to centre itself in rather than being anchored to its top — the trap this
            // catalogue names in `ListItem::line`, arriving through a plain `Row`.
            let node = Node::Group(
                TextRow::new().align(CrossAlign::Stretch).padding(pad).child(line.flex(1)),
            );
            // Never focused: there is no cursor on this tab, so no band and `Ground::Page`
            // throughout.
            draw_row(c, &node, row, theme, false);
        });
    }

}


impl App for BootScreen {
    fn title(&self) -> &str {
        "Boot manager"
    }

    fn handle_key(&mut self, ev: KeyEvent, theme: &Theme<'_>, screen: Rect) -> Handled {
        // A question in front of everything, including the picker: it covers the screen the user
        // can no longer see, so a key that leaked would act on something invisible.
        if let Some(m) = self.confirm.as_mut() {
            match m.handle_key(ev) {
                Some(Answer::Chosen(Confirm::Remove)) => {
                    self.confirm = None;
                    return self.remove_selected();
                }
                Some(Answer::Cancelled) => self.confirm = None,
                None => {}
            }
            return Handled::Consumed;
        }
        // The picker drawer is modal: while it is open it takes every key.
        if self.picker.is_some() {
            return self.route_picker(ev);
        }
        // The entry detail is a screen in front of the list, so Back returns to the list rather than
        // leaving the application. One level of pushing, and the only way out is the way in.
        if self.entry_open {
            if let Key::Softkey(Softkey::Right) = ev.key {
                self.entry_open = false;
                return Handled::Consumed;
            }
            return self.route_entry(ev);
        }
        if let Key::Softkey(Softkey::Right) = ev.key {
            self.back = true;
            return Handled::Consumed;
        }
        // Left/Right switch tabs — except in move mode, where the softkey is "Done" and stealing
        // Left/Right would strand the user mid-drag.
        if !self.move_mode && self.tabs.handle_key(ev, TABS.len()) == Handled::Consumed {
            self.last_tab = self.tabs.active();
            return Handled::Consumed;
        }
        let (_, _, content, _) = Self::regions(screen, theme);
        match self.tabs.active() {
            TAB_ORDER => self.route_list(ev, content, theme),
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
        self.draw_screen(c, theme);
        // Last, over everything, because it is a question about what is behind it — and it paints
        // its own scrim and its own softkey bar, so the screen underneath recedes and the labels
        // belong to whoever has the keys.
        if let Some(m) = self.confirm.as_mut() {
            m.draw(c, theme);
        }
    }

    /// This screen never asks the application to close.
    ///
    /// It used to return `self.back`, which is the same field `take_back` consumes — and those two
    /// mean opposite things. `take_back` means *go up one level*, and the level above this screen is
    /// the navigator; `should_exit` is `App`'s *close the application*. One field, two readings, and
    /// which one won depended on whether the host asked before or after consuming the latch.
    ///
    /// Leaving is the decision of whoever is above, which is the argument `symbian_decl_ui`'s bridge
    /// makes for `Cmd::PopScreen`: *popping the last screen is not exiting*, because a layer that
    /// guessed would close an application on the second of two quick presses — and the second press
    /// is the one a person makes when the first seemed not to take.
    fn should_exit(&self) -> bool {
        false
    }
}

impl BootScreen {
    fn draw_screen(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        let screen = Rect::from_size(c.size());
        let (title, tabs, content, softkeys) = Self::regions(screen, theme);
        chrome::clear(c, theme);

        // The entry detail owns the whole frame, titled with the row it is about — so the screen says
        // which entry is being edited instead of leaving it to be inferred from another tab.
        if self.entry_open {
            let name = self
                .selected_entry()
                .map(|i| self.row_name(i))
                .unwrap_or_else(|| String::from("Entry"));
            chrome::title_bar(c, title, theme, &name, Some("at boot"));
            // The strip's row is the detail's, since there is no strip on this screen — it is one
            // entry, and there is nothing to switch between.
            let area = Rect::new(tabs.x0, tabs.y0, content.x1, content.y1);
            self.draw_entry(c, area, theme);
            chrome::softkey_bar(
                c,
                softkeys,
                theme,
                chrome::Softkeys::new(None, None, Some("Back")),
            );
            return;
        }

        // The picker owns the whole frame too, and for the same reason: it is a different question.
        // It used to be painted *over* this screen, which left the `Order / Last boot` strip showing
        // above a list of applications — two tabs that do nothing while it is up, on a screen that is
        // not about either of them. Chrome belonging to the area you left is the same lie as a
        // softkey label belonging to the screen underneath.
        if let Some(p) = self.picker.as_mut() {
            chrome::title_bar(c, title, theme, "Add an app", Some("to the boot list"));
            let area = Rect::new(tabs.x0, tabs.y0, content.x1, content.y1);
            let items: Vec<PickerItem<'_>> = self
                .roster
                .iter()
                .filter(|a| a.uid3 != BOOTD_UID && a.uid3 != BOOTCTL_UID)
                .filter(|a| !self.cfg.entries.iter().any(|e| e.uid3 == a.uid3))
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

        let detail = if self.move_mode {
            String::from("Move: Up/Down")
        } else {
            format!("{} at boot", self.cfg.active().count())
        };
        chrome::title_bar(c, title, theme, "Boot", Some(&detail));
        self.tabs.draw(c, tabs, theme, &TABS);

        match self.tabs.active() {
            TAB_ORDER => self.draw_list(c, content, theme),
            _ => self.draw_boot(c, content, theme),
        }

        let left = match self.tabs.active() {
            TAB_ORDER if self.move_mode => Some("Done"),
            TAB_ORDER if self.selected_entry().is_some() => Some("Move"),
            TAB_LAST => Some("Reset"),
            _ => None,
        };
        // Left is this screen's mode switch; the action is the D-pad centre, handled in `handle_key`.
        chrome::softkey_bar(c, softkeys, theme, chrome::Softkeys::new(left, None, Some("Back")));

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

    /// The two side margins in this SDK are 5 and 6, and every migrated row has to pick one.
    ///
    /// `theme.metrics.pad` is what a hand-written row insets itself by; `Gap::Base` is what
    /// `ListItem` pads itself by, and `Space::base` is documented as "the default gap, and the side
    /// margin of list rows" — so both numbers believe they are the answer to the same question.
    ///
    /// The rows migrated in `entry_row_node` and `settings::row_node` pin themselves to
    /// `metrics.pad` with an explicit `.pad(...)`, which is the only reason the parity harness reports
    /// `identical`: without it every caption moved one pixel right and every control one pixel left,
    /// measured at 1363 differing pixels across three rows.
    ///
    /// **When this test fails, the two were unified.** That is good news and it means the explicit
    /// `.pad()`/`.gap()` on those rows can go — read them before deleting this, because the parity
    /// reference will have to be regenerated in the same breath.
    #[test]
    fn the_hand_written_inset_and_the_list_rows_padding_are_still_two_different_numbers() {
        with_theme(Palette::DARK, |t| {
            assert_eq!(t.metrics.pad, 5, "the hand-written inset");
            assert_eq!(symbian_decl_ui::spacing::Gap::Base.resolve(t), 6, "ListItem's own padding");
        });
    }

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
    fn select_opens_the_rows_own_screen_and_delete_removes_it() {
        // Select used to flip the enabled switch in place while the policy and delay lived on a
        // separate tab — which is how somebody ended up editing one row while looking at another.
        let mut s = BootScreen::new(cfg2(), None, roster());
        press(&mut s, Key::Select);
        assert!(s.entry_open, "the row's detail is in front now");
        press(&mut s, Key::Softkey(Softkey::Right));
        assert!(!s.entry_open, "and Back returns to the list rather than leaving");
        assert!(!s.back(), "the application is still open");

        // Backspace asks now. It used to remove on the spot, and this assertion was where that was
        // written down as if it were the contract — it was not, it was the defect: the key is not
        // labelled anywhere on screen, so a person pressing it to mean "go back" lost the row they
        // were standing on with nothing to say why, and nothing to undo it with.
        press(&mut s, Key::Backspace);
        assert!(s.confirm.is_some(), "it asks");
        assert_eq!(s.cfg.entries.len(), 2, "and has not touched anything yet");

        press(&mut s, Key::Select);
        assert!(s.confirm.is_none(), "answered");
        assert_eq!(s.cfg.entries.len(), 1);
        assert_eq!(s.cfg.entries[0].uid3, 0x1000_0002);
    }

    #[test]
    fn declining_the_removal_keeps_the_entry() {
        // The half that matters. A question whose only answer is yes is not a question, and the
        // decline path is the one a person actually takes after pressing Backspace by mistake.
        let mut s = BootScreen::new(cfg2(), None, roster());
        press(&mut s, Key::Backspace);
        press(&mut s, Key::Softkey(Softkey::Right));
        assert!(s.confirm.is_none(), "the question is gone");
        assert_eq!(s.cfg.entries.len(), 2, "and the entry is still there");
        assert!(!s.back(), "declining does not leave the screen either");
    }

    #[test]
    fn nothing_behind_the_question_can_act_while_it_is_up() {
        // It covers the screen, so a key that leaked would move a cursor the user cannot see — and
        // the next thing they answered would be about a different row.
        let mut s = BootScreen::new(cfg2(), None, roster());
        press(&mut s, Key::Backspace);
        let before = s.list.selected;
        for k in [Key::Down, Key::Up, Key::Right, Key::Char('x')] {
            assert_eq!(press(&mut s, k), Handled::Consumed, "{k:?}");
        }
        assert_eq!(s.list.selected, before, "the cursor did not move");
        assert!(s.confirm.is_some(), "and the question is still up");
    }

    #[test]
    fn a_critical_row_says_so_and_editing_it_does_not_clear_the_flag() {
        let mut cfg = cfg2();
        cfg.entries[0].critical = true;
        let mut s = BootScreen::new(cfg, None, roster());
        // The mark rides on the index now rather than on the label, so the assertion moved with it:
        // a critical row's number carries a bullet, an ordinary one's does not. Reaching into the
        // node's digest rather than a string is the price of the row being composed instead of
        // formatted — and the digest is enough here, because the two rows differ in exactly this.
        // The same row with and without the flag, not two different rows: row 0 and row 1 carry the
        // digits `1` and `2`, so their digests differ whatever the flag does and an assertion
        // between them would pass for the wrong reason.
        let marked = s.row_node(0).content_hash();
        let mut plain_cfg = cfg2();
        plain_cfg.entries[0].critical = false;
        let plain = BootScreen::new(plain_cfg, None, roster()).row_node(0).content_hash();
        assert_ne!(marked, plain, "a watched row has to look different from an ordinary one");
        // Open the row and flip its switch twice: the flag belongs to the owning app and must
        // survive somebody adjusting the rows around it.
        press(&mut s, Key::Select); // into the detail
        press(&mut s, Key::Select); // off
        press(&mut s, Key::Select); // and back on
        assert!(s.cfg.entries[0].critical, "the flag is not collateral damage of an edit");
    }

    #[test]
    fn re_enabling_an_auto_disarmed_entry_clears_the_marker() {
        let mut cfg = cfg2();
        cfg.entries[0].enabled = false;
        cfg.entries[0].auto_disarmed = true;
        let mut s = BootScreen::new(cfg, None, roster());
        press(&mut s, Key::Select); // into the row's detail
        press(&mut s, Key::Select); // flip the switch there
        assert!(s.cfg.entries[0].enabled);
        assert!(!s.cfg.entries[0].auto_disarmed, "the crash-loop marker goes with the switch");
    }

    #[test]
    fn the_entry_screen_edits_the_row_it_was_opened_from() {
        // The old shape made this test navigate to a *tab*, which is the whole reason it was
        // confusing: the tab and the selected row could disagree, and nothing on screen said so.
        let mut s = BootScreen::new(cfg2(), None, roster());
        press(&mut s, Key::Down); // focus "Notes"
        press(&mut s, Key::Select); // open its detail
        assert!(s.entry_open);
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
    fn the_last_boot_reset_is_a_one_shot_request() {
        let mut s = BootScreen::new(cfg2(), None, roster());
        press(&mut s, Key::Right);
        assert_eq!(s.tabs.active(), TAB_LAST, "two tabs now, both about this boot");
        press(&mut s, Key::Softkey(Softkey::Left));
        assert!(s.take_reset());
        assert!(!s.take_reset(), "consumed, so the caller acts once");
    }

    #[test]
    fn the_restart_limit_row_appears_only_under_times_and_the_cursor_skips_it() {
        let mut cfg = cfg2();
        cfg.entries[0].policy = Policy::Always;
        let mut s = BootScreen::new(cfg, None, roster());
        press(&mut s, Key::Select); // the row's own screen
        assert_eq!(s.entry_rows(), alloc::vec![ROW_ENABLED, ROW_POLICY, ROW_DELAY]);
        press(&mut s, Key::Down); // policy
        press(&mut s, Key::Down); // straight past the absent limit row, onto delay
        assert_eq!(s.entry_focus, ROW_DELAY, "a hidden row is never focused");

        // Choosing Times brings the row into existence.
        s.entry_policy.set(1);
        assert!(s.entry_rows().contains(&ROW_RETRIES));
    }

    #[test]
    fn the_right_softkey_reports_going_up_and_never_closes_the_application() {
        // This test used to assert `should_exit()` as well, and that was the defect rather than the
        // contract: `should_exit` read the very field `take_back` consumes, so one press meant "go up
        // a level" or "close the app" depending on which the host asked first. The screen reports
        // upward; what is above it decides.
        let mut s = BootScreen::new(cfg2(), None, roster());
        press(&mut s, Key::Softkey(Softkey::Right));
        assert!(s.back(), "the screen reports that Back was pressed");
        assert!(!s.should_exit(), "and asks nobody to close the application");
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
    // The Pkgs tab's tests moved with the screen: `symbian_pkgui` owns installing, the catalogue,
    // the repositories and the queue, and tests them there. What is left of this crate is the boot
    // list, one entry's detail, and the last boot — which is what a tab strip should have carried all
    // along.

    #[test]
    fn the_policy_popup_opens_over_the_row_it_edits_and_does_not_replace_the_screen() {
        // It was handed the whole content band, so it filled it — three options on an otherwise
        // empty screen, with nothing above them saying which setting they belonged to. That reads as
        // a place you navigated to rather than as a list that opened over the row you are editing,
        // and the row *is* the caption: `When it stops · Always restart` sitting right above it is
        // better than a heading repeating the same words.
        //
        // `popup_box` has existed all along and the declarative `Select` has always used it; this
        // call site simply never asked. So the assertion is that the top of the screen — the title
        // bar and the first row — is untouched by the popup opening.
        let shot = |open: bool| {
            let mut s = BootScreen::new(cfg2(), None, roster());
            press(&mut s, Key::Select); // into the entry detail
            press(&mut s, Key::Down); // onto the policy row
            if open {
                press(&mut s, Key::Select);
                assert!(s.entry_policy.is_open(), "the dropdown is up");
            }
            let (_, buf) = with_canvas(Size::new(320, 240), |c| {
                with_theme(Palette::DARK, |t| s.draw(c, t));
            });
            buf
        };
        let (open, shut) = (shot(true), shot(false));
        // Everything above the popup must be the same picture. `popup_box` puts a three-option list
        // against the bottom, so the title bar and the switch row are well clear of it.
        let top = (320 * 90) as usize;
        assert_eq!(open[..top], shut[..top], "the popup must not paint over the row it belongs to");
        assert_ne!(open, shut, "and it must actually be on screen");
    }

    #[test]
    fn the_picker_takes_the_whole_frame_and_leaves_no_tab_strip_behind_it() {
        // It used to be painted over this screen, so `Order / Last boot` stayed on show above a list
        // of applications — two tabs that do nothing while the picker is up, on a screen that is
        // about neither of them. Chrome belonging to the area you left is the same lie as a softkey
        // label belonging to the screen underneath, and both were true here at once.
        let shot = |open: bool| {
            let mut s = BootScreen::new(cfg2(), None, roster());
            if open {
                // Walk to the trailing "Add an app…" row the way a person does, rather than poking
                // the field — the row's index moves with the config and a constant would rot.
                for _ in 0..s.list_rows() {
                    if s.selected_entry().is_none() {
                        break;
                    }
                    press(&mut s, Key::Down);
                }
                press(&mut s, Key::Select);
                assert!(s.picker.is_some(), "the picker is up");
            }
            let (_, buf) = with_canvas(Size::new(320, 240), |c| {
                with_theme(Palette::DARK, |t| s.draw(c, t));
            });
            buf
        };
        let (open, shut) = (shot(true), shot(false));
        let (_, tabs, _, _) = with_theme(Palette::DARK, |t| {
            BootScreen::regions(Rect::from_xywh(0, 0, 320, 240), t)
        });
        let band = |b: &[u16]| {
            let mut out = alloc::vec::Vec::new();
            for y in tabs.y0..tabs.y1 {
                out.extend_from_slice(&b[(y * 320) as usize..(y * 320 + 320) as usize]);
            }
            out
        };
        // Against a render of the strip *itself*, not against the closed screen. A first version
        // compared open with shut and passed with the defect put back, because that band changes
        // either way — the picker's own panel starts at its top edge. Two pictures that differ is
        // not evidence about *what* is in them.
        let (_, strip) = with_canvas(Size::new(320, 240), |c| {
            with_theme(Palette::DARK, |t| {
                let tabs_state = symbian_ui::Tabs::default();
                tabs_state.draw(c, tabs, t, &TABS);
            });
        });
        assert_ne!(band(&open), band(&strip), "the strip's row must not still be a strip");
        assert_eq!(band(&shut), band(&strip), "and it still is one when the picker is not up");
    }

    #[test]
    fn a_blank_softkey_does_not_eat_the_key() {
        // `softkeys()` leaves the left slot empty on the "Add an app…" row, and the routing consumed
        // it anyway — a labelled nothing that reported success. Nothing above this screen could have
        // the key, and the user pressed a blank slot that appeared to work.
        let mut s = BootScreen::new(cfg2(), None, roster());
        while s.selected_entry().is_some() {
            press(&mut s, Key::Down);
        }
        // The label itself is computed inline in `draw_screen` — `TABS[TAB_ORDER]` with no selected
        // entry gives `None` — so this asserts the half that is reachable: what the key *does*.
        assert_eq!(
            press(&mut s, Key::Softkey(Softkey::Left)),
            Handled::Ignored,
            "so the key belongs to whoever is above"
        );
        assert!(!s.move_mode);

        // And the other half: on a real entry — where the slot reads "Move" — it is consumed.
        let mut s = BootScreen::new(cfg2(), None, roster());
        assert_eq!(press(&mut s, Key::Softkey(Softkey::Left)), Handled::Consumed);
        assert!(s.move_mode);
    }
}
