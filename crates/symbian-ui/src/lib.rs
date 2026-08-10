//! Widget toolkit for keypad-driven Symbian devices.
//!
//! # Why this shape
//!
//! There is no retained widget tree here, and no `Box<dyn View>`. A screen is a
//! plain struct that owns its state, handles a key event, and draws — and the
//! toolkit supplies the pieces that are genuinely hard to get right: scrolling and
//! selection arithmetic ([`list`]), char-boundary-safe editing ([`edit`]), and the
//! screen furniture ([`chrome`]).
//!
//! That is a deliberate trade. A widget tree buys composition, and costs
//! allocation, indirection through trait objects, and a focus-traversal system.
//! On a 320x240 screen showing five rows at a time, with one D-pad driving
//! everything, composition is not the problem — arithmetic is. Splitting the
//! arithmetic out and unit-testing it catches the bugs that actually happen: a
//! scrollbar thumb one pixel past its track, a caret landing inside a Cyrillic
//! character, a list that scrolls to a row that no longer exists.
//!
//! It also happens to match Symbian: the framework owns
//! `CActiveScheduler::Start()`, so Rust is always a callee. A screen's
//! `handle_key` and `draw` are called from the shim, which is exactly the shape
//! this design assumes.
//!
//! # Sketch
//!
//! ```ignore
//! let frame = Frame::split(screen, &theme, true, true);
//! chrome::clear(&mut canvas, &theme);
//! chrome::title_bar(&mut canvas, frame.title, &theme, "Chats", Some("online"));
//!
//! let rows = Uniform { count: chats.len(), height: theme.metrics.row_h };
//! state.for_visible(&rows, frame.content, |i, r| draw_chat_row(&mut canvas, r, &chats[i]));
//! chrome::scrollbar(&mut canvas, frame.content, &theme,
//!                   state.scrollbar(&rows, frame.content.height()));
//!
//! chrome::softkey_bar(&mut canvas, frame.softkeys, &theme,
//!                     [Some("Options"), None, Some("Exit")]);
//! ```

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod app;
pub mod chrome;
pub mod edit;
pub mod icon;
pub mod input;
pub mod list;
pub mod paint;
pub mod theme;
pub mod viewer;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod tokens;

pub use app::{App, RawEvent};
pub use chrome::Frame;
pub use edit::TextField;
pub use input::{Handled, Key, KeyEvent, Modifiers, Softkey};
pub use list::{ListState, Rows, Uniform};
pub use theme::{Fonts, Metrics, Palette, Theme};
pub use tokens::{Space, Surface};
pub use viewer::{Viewer, ViewerAction};

// Re-exported so an app needs only this crate in scope to draw.
pub use symbian_gfx as gfx;
pub use symbian_gfx::{
    Align, BitmapFont, Canvas, Color, Edges, Fitted, Font, Glyph, Point, Rect, Size,
    WithFallback,
};
