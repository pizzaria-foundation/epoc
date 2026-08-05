//! Software rasterizer for Symbian's 16bpp framebuffers.
//!
//! This is the bottom layer of the Rust Symbian SDK. It knows about pixels,
//! rectangles, clipping and glyphs, and nothing whatsoever about Symbian — which
//! is deliberate: the same code runs unchanged on the host, so the widget toolkit
//! above it can be developed and tested without a device in the loop.
//!
//! Everything here is `no_std`. There is no interior mutability and no `static
//! mut` anywhere in the crate, which is what keeps the compiled object free of
//! writable sections (see `docs/wsd.md` — Symbian rejects DLLs that have any).
//!
//! ```
//! use symbian_gfx::{Canvas, Color, Rect, Size};
//!
//! let mut pixels = vec![0u16; 320 * 240];
//! let mut c = Canvas::from_slice(&mut pixels, Size::new(320, 240));
//! c.clear(Color::hex(0x101418));
//! c.fill_round_rect(Rect::from_xywh(8, 8, 120, 32), 4, Color::hex(0x2A82DA));
//! ```

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod canvas;
pub mod color;
pub mod font;
pub mod geom;
pub mod present;

pub use canvas::{Align, Canvas, CanvasState};
pub use color::{blend565, Color, Rgb565};
pub use font::{BitmapFont, Fitted, Font, FontError, Glyph};
pub use geom::{Edges, Point, Rect, Size};
pub use present::{rgb565_to_xrgb8888, ScreenFormat};

/// The E72's panel, and the only geometry this SDK has been designed against.
/// Landscape QVGA, confirmed from the device's own SDK plugin, whose layout
/// tables are named `qvga2_landscape`, and then on the device itself.
pub const E72_SCREEN: Size = Size::new(320, 240);

/// What the E72 reports for `CWsScreenDevice::DisplayMode()`. Measured on the
/// device — it is `EColor16MU` at 32bpp, not the `EColor64K` the 16bpp canvas
/// would have matched. See [`present`] for why the canvas stays 16bpp anyway.
pub const E72_DISPLAY_MODE: i32 = 11;
