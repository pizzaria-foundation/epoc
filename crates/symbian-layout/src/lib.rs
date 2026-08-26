//! Page layout for a 320-pixel screen.
//!
//! A styled tree in, a resolved display list out. What makes this crate the product rather than the
//! cost is what it deliberately does **not** do: a faithful CSS 2.1 layout is the wrong behaviour
//! here. A page that declares 980 pixels of width gets 320, its columns collapse into one, and its
//! nine-pixel type comes up to something readable. That policy is the browser; conformance would be
//! a worse browser that took longer to write.
//!
//! # The two boundaries
//!
//! **In** is [`style::StyledTree`] — our own type, not libcss's. The producer that fills it from
//! libdom and libcss is a separate piece; keeping its types out of here is what lets this whole
//! crate run and be tested on a desktop.
//!
//! **Out** is [`ir::PageIr`] — a flat, serialisable display list whose every node is one call the
//! rasterizer already has. It covers the whole document, not the viewport, so scrolling never
//! reflows; it is one buffer, so a frozen tab keeps exactly one thing; and it is serialisable, so
//! the desktop preview can render a page and save-for-offline can store one.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod block;
pub mod css;
pub mod inline;
pub mod ir;
pub mod paint;
pub mod style;
pub mod tagsoup;
pub mod wire;

pub use block::layout;
pub use inline::{FontSet, Item, Line, Run};
pub use ir::{Node, PageIr};
pub use paint::{max_scroll, paint, visible_images};
pub use tagsoup::parse;
pub use style::{Display, FontRole, Marker, NodeKind, Span, Style, StyledTree};
