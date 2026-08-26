//! The abstract key model.
//!
//! Deliberately Symbian-free. The window server hands the shim a `TKeyEvent`
//! with a translated character in `iCode` (it has already applied Shift, Caps
//! Lock, Fn and dead keys), and the shim maps that onto these variants. Keeping
//! the mapping on the C++ side means this crate compiles and tests on the host.

/// Which of the three softkeys. FP2 added the middle one, so a toolkit that
/// assumes two will look wrong on this device.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Softkey {
    Left,
    Middle,
    Right,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Key {
    /// A translated character, ready to insert into a text field.
    Char(char),
    Up,
    Down,
    Left,
    Right,
    /// The D-pad centre press.
    Select,
    Softkey(Softkey),
    Backspace,
    Delete,
    Enter,
    /// Green key.
    Call,
    /// Red key. The system captures this to close the app; treat it as advisory.
    End,
    /// A Ctrl chord, carrying the letter in lower case: `Ctrl('v')` for Ctrl+V.
    ///
    /// A variant of its own rather than [`Key::Char`] with `mods.ctrl`, because a chord is not
    /// text and the type should say so. Every consumer of `Char` — a text field, a list's
    /// type-to-filter, a digits-only login field — would otherwise have to remember to check the
    /// modifier, and the one that forgot would silently type `v` when the user asked to paste.
    /// This way an old `match` arm simply stops matching, which the compiler and the user both
    /// notice immediately.
    Ctrl(char),
    /// A scan code we have no name for. Carried through so an app can special-case
    /// hardware keys the toolkit does not model.
    Raw(u16),
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    /// The Fn/Chr key, which on the E72 is how digits and symbols are reached.
    ///
    /// True however the layer was engaged — held, tapped to arm one keystroke, or locked. Use this
    /// for anything about *what character* a key produces.
    pub func: bool,
    /// The Fn key is physically down at this moment.
    ///
    /// Use this, not [`Modifiers::func`], for a shortcut. Arming and locking are stored state from
    /// an earlier press and will happily attach themselves to a key pressed much later; holding is
    /// a gesture the person is making right now. A destructive or irreversible shortcut wants the
    /// second and not the first.
    pub func_held: bool,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct KeyEvent {
    pub key: Key,
    pub mods: Modifiers,
    /// True for auto-repeat, so a list can accelerate and a text field can ignore.
    pub repeat: bool,
}

impl KeyEvent {
    pub const fn new(key: Key) -> Self {
        Self { key, mods: Modifiers { shift: false, ctrl: false, func: false, func_held: false }, repeat: false }
    }

    pub const fn with_mods(key: Key, mods: Modifiers) -> Self {
        Self { key, mods, repeat: false }
    }

    /// True when this event should move a selection up, for either the D-pad or
    /// the keys people reach for out of habit.
    pub fn is_prev(&self) -> bool {
        matches!(self.key, Key::Up)
    }

    pub fn is_next(&self) -> bool {
        matches!(self.key, Key::Down)
    }
}

/// Whether a widget consumed an event. Mirrors Symbian's `TKeyResponse`: a
/// widget that returns `Ignored` lets the event fall through to the app, and
/// ultimately back to Avkon, which matters for the End key.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Handled {
    Consumed,
    Ignored,
}

impl Handled {
    #[inline]
    pub fn is_consumed(self) -> bool {
        matches!(self, Handled::Consumed)
    }

    /// Try `other` only if this was ignored.
    #[inline]
    pub fn or_else(self, other: impl FnOnce() -> Handled) -> Handled {
        match self {
            Handled::Consumed => self,
            Handled::Ignored => other(),
        }
    }
}

impl From<bool> for Handled {
    /// `true` consumed the key, `false` left it for someone else.
    ///
    /// For the many handlers whose real answer is "did I do anything?" — a paste with an empty
    /// clipboard, a copy a masked field refused. Writing that as an `if` produced the same four
    /// lines at every one of them, and the shape it invites is `Consumed` unconditionally, which
    /// is how a key gets swallowed by a widget that ignored it.
    #[inline]
    fn from(did_something: bool) -> Self {
        if did_something {
            Handled::Consumed
        } else {
            Handled::Ignored
        }
    }
}

#[cfg(test)]
mod modifier_tests {
    use super::*;

    #[test]
    fn a_held_fn_is_also_the_fn_layer() {
        // Holding is one of the ways the layer is engaged, so a text field still sees `func`.
        let m = Modifiers { shift: false, ctrl: false, func: true, func_held: true };
        assert!(m.func && m.func_held);
    }

    #[test]
    fn an_armed_fn_is_the_layer_but_not_a_gesture() {
        // The state a tap leaves behind: a digit typed next gets the Fn layer, and a shortcut that
        // requires a deliberate hold must not fire.
        let m = Modifiers { shift: false, ctrl: false, func: true, func_held: false };
        assert!(m.func, "a text field still gets the Fn layer");
        assert!(!m.func_held, "but a shortcut sees no gesture");
    }
}
