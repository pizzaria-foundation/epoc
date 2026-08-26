//! A form, driven by keys from end to end.
//!
//! Every unit test in `switch.rs` and `checkbox.rs` presses one widget in isolation, which proves the
//! widget and not the assembly. This is the assembly: a [`FocusScope`] holding a switch, two radio
//! buttons, a checkbox and a button, walked with the D-pad, with the messages arriving in `update`
//! and the model changing there and nowhere else.
//!
//! It is the file that would have caught the two defects this phase found — a `Button` that consumed
//! its key and dropped the message, and a builder whose ink depended on the order its methods were
//! called in. Both were invisible to a test that pressed one widget.

extern crate alloc;

use symbian_decl_ui::layout::{self, CrossAlign};
use symbian_decl_ui::outbox::Outbox;
use symbian_decl_ui::slot::SlotTable;
use symbian_decl_ui::spacing::Gap;
use symbian_decl_ui::widget::KeyCtx;
use symbian_decl_ui::widgets::{
    Button, Checkbox, Column, Divider, FocusScope, FocusStops, ListItem, Node, SectionHeader, Switch,
};
use symbian_decl_ui::{Rect, UiCache};
use symbian_gfx::Size;
use symbian_ui::{testing, EdgePolicy, Handled, Key, KeyEvent, Palette};

/// How often the alarm repeats — the radio group's value.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Repeat {
    Daily,
    Weekdays,
}

/// The app's state. Every field here is changed by `update` and by nothing else.
struct Model {
    alarm: bool,
    repeat: Repeat,
    vibrate: bool,
    saved: bool,
    out: Outbox<Msg>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Msg {
    ToggleAlarm,
    SetRepeat(Repeat),
    ToggleVibrate,
    Save,
}

impl Model {
    fn new() -> Self {
        Self {
            alarm: false,
            repeat: Repeat::Daily,
            vibrate: false,
            saved: false,
            out: Outbox::new(),
        }
    }

    /// The single place the model changes.
    fn update(&mut self, msg: Msg) {
        match msg {
            Msg::ToggleAlarm => self.alarm = !self.alarm,
            // Single selection resolved here, where the group is known. A radio button reports "set
            // this"; nothing about it could turn its siblings off.
            Msg::SetRepeat(r) => self.repeat = r,
            Msg::ToggleVibrate => self.vibrate = !self.vibrate,
            Msg::Save => self.saved = true,
        }
    }
}

/// The form. Five stops, two things no cursor lands on.
fn form(m: &Model, slots: &mut SlotTable) -> (Node, FocusStops) {
    let out = m.out.clone();
    let scope = FocusScope::vertical(slots)
        .policy(EdgePolicy::Stop)
        .gap(Gap::Snug)
        .stretch_width()
        // Not stops: a heading and a rule. Added through `fixed`, which is what keeps the cursor off
        // them — the count of stops is five, not seven.
        .fixed(Node::leaf(SectionHeader::new("Alarm")))
        .stop({
            let out = out.clone();
            move |f| {
                ListItem::new("Alarm")
                    .selected(f)
                    .trailing(Switch::new(m.alarm).focused(f).out(out, Msg::ToggleAlarm))
                    .build()
            }
        })
        .stop({
            let out = out.clone();
            let on = m.repeat == Repeat::Daily;
            move |f| {
                ListItem::new("Every day")
                    .selected(f)
                    .leading(Checkbox::radio(on).focused(f).out(out, Msg::SetRepeat(Repeat::Daily)))
                    .build()
            }
        })
        .stop({
            let out = out.clone();
            let on = m.repeat == Repeat::Weekdays;
            move |f| {
                ListItem::new("Weekdays")
                    .selected(f)
                    .leading(
                        Checkbox::radio(on).focused(f).out(out, Msg::SetRepeat(Repeat::Weekdays)),
                    )
                    .build()
            }
        })
        .fixed(Node::leaf(Divider::new().space(Gap::Snug)))
        .stop({
            let out = out.clone();
            move |f| {
                ListItem::new("Vibrate")
                    .selected(f)
                    .leading(Checkbox::checked(m.vibrate).focused(f).out(out, Msg::ToggleVibrate))
                    .build()
            }
        })
        .stop(move |f| Node::leaf(Button::new("Save", Msg::Save).focused(f).out(out)));
    let stops = scope.stops();
    (scope.build(), stops)
}

/// A screen: the form in a column, so it has somewhere to sit.
fn screen(m: &Model, slots: &mut SlotTable) -> (Node, FocusStops) {
    let (scope, stops) = form(m, slots);
    (Node::Group(Column::new().align(CrossAlign::Stretch).node(scope)), stops)
}

const BAND: Rect = Rect { x0: 0, y0: 0, x1: 320, y1: 240 };

/// One frame of the app: build the view, place it, press `key`, then drain the outbox into `update`.
///
/// The order matters and mirrors the bridge: the key walk runs against the rects the last frame laid
/// out, and the outbox is drained immediately after it — never later, or a press takes effect on the
/// press after it.
fn tick(m: &mut Model, slots: &mut SlotTable, key: Key) -> (Handled, usize) {
    slots.begin_frame();
    let (root, stops) = screen(m, slots);
    let cursor = stops.cursor();
    let handled = testing::with_theme(Palette::DARK, |theme| {
        let mut cache = UiCache::with_capacity(root.slot_count() + 8);
        // Placed before the press: `dispatch_key` reads the rects a frame laid out, and a tree that
        // has never been drawn takes no keys.
        layout::place_frame(&root, BAND, &mut cache, theme);
        let mut clip = symbian_ui::NoClipboard;
        let mut cx = KeyCtx::new(theme, &mut clip);
        layout::dispatch_key(&root, KeyEvent::new(key), &cache, &mut cx)
    });
    for msg in m.out.take() {
        m.update(msg);
    }
    (handled, cursor)
}

/// Walk the cursor down `n` stops, changing nothing.
fn walk_down(m: &mut Model, slots: &mut SlotTable, n: usize) {
    for _ in 0..n {
        tick(m, slots, Key::Down);
    }
}

#[test]
fn the_cursor_walks_the_stops_and_skips_the_heading_and_the_rule() {
    // Seven children, five stops. A scope told otherwise would park the cursor on a heading, where
    // `Select` does nothing and the D-pad looks broken.
    let mut m = Model::new();
    let mut slots = SlotTable::new();
    let mut seen = alloc::vec::Vec::new();
    for _ in 0..8 {
        let (_, cursor) = tick(&mut m, &mut slots, Key::Down);
        seen.push(cursor);
    }
    // The cursor starts at 0 and stops at 4 — `EdgePolicy::Stop` holds it there rather than wrapping.
    assert_eq!(seen, alloc::vec![0, 1, 2, 3, 4, 4, 4, 4], "{seen:?}");
}

#[test]
fn a_switch_is_flipped_by_the_model_and_not_by_itself() {
    let mut m = Model::new();
    let mut slots = SlotTable::new();
    // No walking: stop **0** is the switch. The heading above it went in through `fixed`, which does
    // not take an index — which is the whole point of `fixed` and was also the first thing this test
    // got wrong. It pressed stop 1 and watched `SetRepeat(Daily)` arrive from the radio button below.
    assert!(!m.alarm);
    let (handled, _) = tick(&mut m, &mut slots, Key::Select);
    assert_eq!(handled, Handled::Consumed);
    assert!(m.alarm, "the message reached update");
    tick(&mut m, &mut slots, Key::Select);
    assert!(!m.alarm, "and again, the other way");
}

#[test]
fn a_radio_group_moves_its_choice_without_any_widget_knowing_its_siblings() {
    // The point of "single selection is the caller's job": `SetRepeat(Weekdays)` is what arrives, and
    // `update` is where the other option stops being chosen.
    let mut m = Model::new();
    let mut slots = SlotTable::new();
    assert_eq!(m.repeat, Repeat::Daily);

    walk_down(&mut m, &mut slots, 2); // stop 2: "Weekdays"
    tick(&mut m, &mut slots, Key::Select);
    assert_eq!(m.repeat, Repeat::Weekdays);

    // And back, from the stop above it.
    tick(&mut m, &mut slots, Key::Up);
    tick(&mut m, &mut slots, Key::Select);
    assert_eq!(m.repeat, Repeat::Daily);
}

#[test]
fn pressing_the_already_chosen_option_leaves_the_model_with_a_value() {
    // A radio group whose chosen option could be pressed *off* would leave the model with nothing
    // selected and no way back. The message is "set this", so this is a no-op rather than a hole.
    let mut m = Model::new();
    let mut slots = SlotTable::new();
    walk_down(&mut m, &mut slots, 1); // stop 1: "Every day", already chosen
    tick(&mut m, &mut slots, Key::Select);
    assert_eq!(m.repeat, Repeat::Daily);
}

#[test]
fn a_checkbox_and_a_switch_on_one_screen_do_not_both_fire() {
    // One press, one effect. Without the focus flag on each control the broadcast walk would hand the
    // key to every one of them.
    let mut m = Model::new();
    let mut slots = SlotTable::new();
    walk_down(&mut m, &mut slots, 3); // stop 3: the vibrate checkbox
    tick(&mut m, &mut slots, Key::Select);
    assert!(m.vibrate);
    assert!(!m.alarm, "the switch two stops up did not move");
    assert!(!m.saved, "and neither did the button below");
}

#[test]
fn a_button_at_the_end_of_a_form_actually_fires() {
    // The defect this phase found: `Button::handle_key` computed its message and threw it away, so a
    // button in a tree took the key and did nothing. Every existing test missed it by calling
    // `press()` directly instead of pressing the widget the way the engine does.
    let mut m = Model::new();
    let mut slots = SlotTable::new();
    walk_down(&mut m, &mut slots, 4); // stop 4: Save
    assert!(!m.saved);
    let (handled, cursor) = tick(&mut m, &mut slots, Key::Select);
    assert_eq!(cursor, 4, "the last stop");
    assert_eq!(handled, Handled::Consumed);
    assert!(m.saved);
}

#[test]
fn the_controls_leave_the_navigation_keys_to_the_scope() {
    // What keeps a form navigable: a switch or a checkbox that consumed `Down` would trap the cursor
    // on itself. Asserted from the top *and* from a stop in the middle, since a control only sees a
    // key when it has the focus.
    let mut m = Model::new();
    let mut slots = SlotTable::new();
    for stop in 0..5 {
        let (handled, _) = tick(&mut m, &mut slots, Key::Down);
        if stop < 4 {
            assert_eq!(handled, Handled::Consumed, "the scope moved from stop {stop}");
        }
    }
    // Nothing was toggled on the way down.
    assert!(!m.alarm && !m.vibrate && !m.saved);
    assert_eq!(m.repeat, Repeat::Daily);
}

#[test]
fn the_horizontal_arrows_reach_the_focused_control_rather_than_the_scope() {
    // A vertical scope declines `Left` and `Right` so a stepper or a select can have them. None of
    // these controls wants them, so the press falls all the way through — which is the honest answer
    // and is what lets a screen bind them itself with `OnKey`.
    let mut m = Model::new();
    let mut slots = SlotTable::new();
    // Stop 0, the switch: it wants neither arrow, and neither does the scope.
    let (handled, _) = tick(&mut m, &mut slots, Key::Left);
    assert_eq!(handled, Handled::Ignored);
    let (handled, _) = tick(&mut m, &mut slots, Key::Right);
    assert_eq!(handled, Handled::Ignored);
    assert!(!m.alarm, "and nothing was flipped by an arrow");
}

#[test]
fn the_cursor_survives_every_rebuild_and_nothing_accumulates() {
    // The form is rebuilt from scratch on every frame — five presses is five trees. The cursor lives
    // in the slot table precisely so it does not go back to the first field mid-form.
    let mut m = Model::new();
    let mut slots = SlotTable::new();
    walk_down(&mut m, &mut slots, 3);
    let groups = slots.group_count();
    for _ in 0..12 {
        tick(&mut m, &mut slots, Key::Down);
    }
    assert_eq!(slots.type_mismatches(), 0);
    assert_eq!(slots.unbalanced_groups(), 0);
    assert_eq!(slots.group_count(), groups, "the slot table did not grow");
}

#[test]
fn the_form_draws_and_every_state_looks_different() {
    // A form that laid out correctly and painted nothing would pass every assertion above. This is
    // the one that requires ink — and requires it to *change* when the model does, which the
    // one-glyph test atlas can see because a switch and a mark are fills rather than text.
    let paint = |m: &Model| {
        let mut slots = SlotTable::new();
        let (root, _) = screen(m, &mut slots);
        let (_, buf) = testing::with_canvas(Size::new(320, 240), |c| {
            testing::with_theme(Palette::DARK, |theme| {
                c.clear(Palette::DARK.bg.mid());
                let mut cache = UiCache::with_capacity(root.slot_count() + 8);
                layout::draw_frame(&root, BAND, &mut cache, c, theme);
            });
        });
        buf
    };
    let base = Model::new();
    let blank = paint(&base);
    let bg = Palette::DARK.bg.mid().to_rgb565().0;
    assert!(blank.iter().any(|&p| p != bg), "the form drew something");

    let mut alarm_on = Model::new();
    alarm_on.alarm = true;
    assert_ne!(blank, paint(&alarm_on), "the switch moved");

    let mut vibrating = Model::new();
    vibrating.vibrate = true;
    assert_ne!(blank, paint(&vibrating), "the checkbox filled");

    let mut weekdays = Model::new();
    weekdays.repeat = Repeat::Weekdays;
    assert_ne!(blank, paint(&weekdays), "the chosen radio moved");
}
