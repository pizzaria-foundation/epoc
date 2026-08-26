//! Widget toolkit for keypad-driven Symbian devices.
//!
//! # Why this shape
//!
//! There is no retained widget tree here, and no `Box<dyn View>`. A screen is a
//! plain struct that owns its state, handles a key event, and draws — and the
//! toolkit supplies the pieces that are genuinely hard to get right: scrolling and
//! selection arithmetic ([`list`]), char-boundary-safe editing ([`edit`]), cursor
//! movement between unlike controls ([`focus`]), and the screen furniture
//! ([`chrome`]).
//!
//! That is a deliberate trade. A widget tree buys composition, and costs
//! allocation and indirection through trait objects.
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
//! # The key convention
//!
//! Every screen in this SDK uses the same three keys for the same three jobs. It is the native S60
//! arrangement, and that is the argument for it: the phone has trained its user for a decade, and a
//! screen that disagrees is the one that feels broken.
//!
//! ```text
//!   ┌──────────────────────────────────────────────┐
//!   │  Options            Open            Back     │
//!   └──────────────────────────────────────────────┘
//!      left softkey    D-pad centre   right softkey
//!      secondary       THE ACTION     way out
//! ```
//!
//! - **Centre of the D-pad is the action.** Open, send, confirm — whatever this screen is for.
//!   It arrives as [`Key::Select`]. It is *not* a softkey: S60 wires the middle slot of the bar to
//!   the selection key, so `Softkey::Middle` never arrives and a screen that waits for it waits for
//!   ever. Label the middle slot; handle `Select`.
//! - **Left softkey is options** — the secondary offer: refresh, a mode switch, a menu. Blank when
//!   there is nothing, which is common.
//! - **Right softkey is back**, and only ever back or exit. It is the one key a user presses
//!   without reading, so it must never become a second action.
//!
//! [`chrome::Softkeys`] builds the bar in that order, by name, so the three cannot be transposed
//! silently — `[a, b, c]` reads the same whichever meaning the author had in mind.
//!
//! # Sketch
//!
//! ```ignore
//! let frame = Frame::split(screen, &theme, true, true);
//! chrome::clear(&mut canvas, &theme);
//! chrome::title_bar(&mut canvas, frame.title, &theme, "Chats", Some("online"));
//!
//! let rows = Uniform { count: chats.len(), height: theme.metrics.row_h };
//! // `draw_visible`, not `for_visible`: it clips to the band, so the partially-visible top row
//! // is cut at the edge instead of painting on the title bar. See `ListState::for_visible`.
//! state.draw_visible(&mut canvas, &rows, frame.content, |c, i, r| draw_chat_row(c, r, &chats[i]));
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
pub mod app_picker;
pub mod calendar;
pub mod chip;
pub mod chrome;
pub mod clip;
pub mod device_screen;
pub mod drawer;
pub mod edit;
pub mod flow;
pub mod focus;
pub mod grid;
pub mod icon;
pub mod input;
pub mod list;
pub mod match_filter;
pub mod marquee;
pub mod paint;
pub mod menu;
pub mod meter;
pub mod modal;
pub mod prompt;
pub mod select;
pub mod sheet;
pub mod slider;
pub mod stepper;
pub mod tabs;
pub mod text_prompt;
pub mod tile;
pub mod theme;
pub mod tick;
pub mod toggle;
pub mod viewer;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod tokens;

pub use app::{App, RawEvent};
pub use app_picker::{AppPicker, IconRef, Item as PickerItem, PickerAction};
pub use calendar::{Part as DatePart, Stamp};
pub use chip::{Chip, Tone};
pub use chrome::{Frame, Softkeys};
pub use clip::{Clipboard, MemClipboard, NoClipboard};
pub use device_screen::{DeviceScreen, Entry as DeviceEntry};
pub use drawer::{Drawer, DrawerAction, Section};
pub use edit::TextField;
pub use flow::{Packer, Placed};
pub use focus::{EdgePolicy, FocusAxis, FocusEdge, FocusRing};
pub use grid::{GridEdge, GridShape, GridState};
pub use input::{Handled, Key, KeyEvent, Modifiers, Softkey};
pub use list::{ListState, Rows, Uniform};
pub use marquee::Pace;
pub use meter::Meter;
pub use modal::{Answer, Modal};
pub use prompt::{Prompt, PromptAction};
pub use select::{Select, SelectAction};
pub use sheet::{Row as SheetRow, Sheet, SheetAction};
pub use slider::Slid;
pub use stepper::Stepper;
pub use tabs::Tabs;
pub use text_prompt::{TextAnswer, TextPrompt};
pub use tile::{letter_tile, TILE_COLOURS};
pub use theme::{Fonts, Ground, Metrics, Palette, Theme};
pub use tick::{draw_mark, mark_box, mark_size, Mark};
pub use toggle::{draw_switch, switch_height, switch_track, Toggle, SWITCH_W};
pub use tokens::{Space, Surface};
pub use viewer::{Viewer, ViewerAction};

// Re-exported so an app needs only this crate in scope to draw.
pub use symbian_gfx as gfx;
pub use symbian_gfx::{
    Align, BitmapFont, Canvas, Color, Edges, Fitted, Font, Glyph, Point, Rect, Size,
    WithFallback,
};
