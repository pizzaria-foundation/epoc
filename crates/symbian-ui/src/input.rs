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
    /// A scan code we have no name for. Carried through so an app can special-case
    /// hardware keys the toolkit does not model.
    Raw(u16),
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    /// The Fn/Chr key, which on the E72 is how digits and symbols are reached.
    pub func: bool,
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
        Self { key, mods: Modifiers { shift: false, ctrl: false, func: false }, repeat: false }
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
