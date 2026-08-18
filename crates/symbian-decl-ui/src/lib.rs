//! Screens described rather than drawn.
//!
//! [`symbian_ui`] gives a screen the pieces — a list's scroll arithmetic, a text field's caret, the
//! chrome — and leaves the screen to place them, measure them, and route keys to them by hand. That
//! is the right trade for a handful of screens and the wrong one for a hundred: the placement
//! arithmetic is the same every time, and the routing is where the same bug keeps being written.
//!
//! This crate is the layer above. A screen is a tree of [`Widget`]s built with plain method calls,
//! measured once per change and drawn every frame:
//!
//! ```ignore
//! Screen::new()
//!     .title("Recent")
//!     .content(Column::new()
//!         .child(Text::new(&header))
//!         .child(ScrollList::new(&rows).fill(1)))
//!     .on_options("Refresh", Msg::Refresh)
//!     .on_action("Open", Msg::Open)
//!     .on_back("Back", Msg::Back)
//! ```
//!
//! # What it does not do
//!
//! No virtual DOM, no diffing, no retained tree of element objects. The screen is 320x240 and draws
//! in a few hundred microseconds; the tree is a dozen nodes. Diffing a dozen nodes to avoid drawing
//! a dozen nodes is work that costs more than it saves, and every frame of it would allocate. What
//! *is* cached is [`measure`](Widget::measure) — text metrics and layout arithmetic are the
//! expensive part, and they only change when the content does.
//!
//! No touch, no gestures: there is no touchscreen. One D-pad and a keyboard, dispatched directly.
//!
//! # Softkeys are part of the screen, not of the drawing
//!
//! A screen declares its softkeys where it is built, as a label *and* the message that label
//! promises — see [`keys`]. That is the one structural fix this layer makes to a real defect: in
//! the imperative toolkit the bar is drawn in one function and the keys are routed in another, and
//! nothing checks that they agree. They did not, once, and the key did something other than what it
//! said.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod app;
pub mod bridge;
pub mod cache;
pub mod cmd;
pub mod constraints;
pub mod keys;
pub mod layout;
pub mod length;
pub mod outbox;
pub mod widget;

pub use app::DeclarativeApp;
pub use bridge::DeclarativeAppBridge;
pub use cache::UiCache;
pub use cmd::Cmd;
pub use constraints::Constraints;
pub use keys::{SoftkeyDef, Softkeys};
pub use layout::{draw_frame, draw_tree, layout_tree, measure_tree, Axis, CrossAlign, MainAlign};
pub use length::Length;
pub use outbox::Outbox;
pub use widget::{Widget, WidgetHash};

/// Re-exported so a screen needs one `use` rather than three.
pub use symbian_gfx::{Canvas, Point, Rect, Size};
pub use symbian_ui::{Handled, Key, KeyEvent, Softkey, Theme};

pub mod slot;
pub mod theme;
pub mod widgets;

pub use widgets::{Column, Group, Imperative, Node, Row, Spacer};
pub mod overflow;
