//! HTML and CSS into a [`StyledTree`], through the NetSurf libraries.
//!
//! # The thin half of a thick boundary
//!
//! Almost nothing happens here. The parse, the cascade and the walk are in `shim/csrc/dom_bridge.c`
//! and `shim/csrc/css_select.c`, and they are in C because libdom cannot be called from anything
//! else: every accessor is a `static inline` dispatching through a per-node vtable behind a macro of
//! the same name, so there is no `dom_node_get_first_child` symbol to link against. libcss adds a
//! handler of 36 function pointers, each of which is a DOM query.
//!
//! So this crate is one `extern "C"` call and a decode. The DOM never crosses the boundary; a buffer
//! does, in the format [`symbian_layout::wire`] validates on the way in.
//!
//! # What replaces what
//!
//! This is the real producer for [`symbian_layout::StyledTree`]. `symbian_layout::tagsoup` was the
//! scaffolding that let the browser be built and measured before it existed, and it says so in its
//! own first line. The difference is not size: it is a tolerant HTML5 tokeniser against a forgiving
//! one, and a real cascade — selector matching, specificity, inheritance, a UA stylesheet — against
//! no cascade at all.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use symbian_gfx::Color;
use symbian_layout::StyledTree;

/// Colours the document does not choose for itself.
///
/// Passed in rather than decided in C, because a theme is not something the bridge should know: the
/// UA stylesheet it carries has **no colours at all** for this reason, and answers `color` as black
/// so this substitutes.
#[derive(Copy, Clone, Debug)]
pub struct Palette {
    pub text: Color,
    pub dim: Color,
    pub link: Color,
}

impl Default for Palette {
    /// The web's defaults: white paper, dark ink, blue links. See [`symbian_layout::css`].
    fn default() -> Self {
        Palette {
            text: symbian_layout::css::INK,
            dim: symbian_layout::css::DIM,
            link: symbian_layout::css::LINK,
        }
    }
}

/// What went wrong. The codes are the bridge's, so a failure says which stage rather than only that
/// one happened.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// A null pointer or an empty document.
    Argument,
    NoMemory,
    /// libhubbub or libdom refused the document.
    Parse,
    /// The select context could not be built — a broken UA stylesheet, or libcss out of memory.
    Css,
    /// The tree did not fit in the buffer offered.
    TooLarge,
    /// The bridge answered, and what it wrote was not a tree this version understands.
    ///
    /// Its own variant because it means the two sides of the wire format disagree, which is a build
    /// problem rather than a page problem — and the only failure here that no page can cause.
    Malformed,
    Internal(i32),
    /// Only reachable on the host, where the bridge is a stub.
    NotAvailable,
}

fn error_of(code: i32) -> Error {
    match code {
        -1 => Error::Argument,
        -2 => Error::NoMemory,
        -3 => Error::Parse,
        -4 => Error::Css,
        -5 => Error::TooLarge,
        other => Error::Internal(other),
    }
}

/// How much room the tree gets.
///
/// Sized rather than negotiated, for the same reason the layout's output is: this runs on the worker
/// thread, and the only channel back is a byte count. A page that does not fit is refused with
/// [`Error::TooLarge`] instead of being truncated into something that decodes as nothing.
///
/// The measurement it is chosen against: a Wikipedia article is 701 KB of HTML and lays out to 6182
/// nodes. At 70 bytes a node that is 433 KB of records, plus the visible text.
pub const DEFAULT_CAP: usize = 2 * 1024 * 1024;

/// Parse `html` and resolve its styles.
///
/// `width` is the column the media query sees; the layout is done separately and reads its own.
pub fn parse(html: &[u8], width: i32, palette: Palette) -> Result<StyledTree, Error> {
    parse_with_cap(html, width, palette, DEFAULT_CAP)
}

/// The same, with the buffer size chosen by the caller.
pub fn parse_with_cap(
    html: &[u8],
    width: i32,
    palette: Palette,
    cap: usize,
) -> Result<StyledTree, Error> {
    if html.is_empty() || cap == 0 {
        return Err(Error::Argument);
    }
    stage("dom_rs_entry");
    let pal = ffi::DomPalette { text: palette.text.0, dim: palette.dim.0, link: palette.link.0 };
    // The buffer, on whichever thread this is. A megabytes-wide zeroed allocation is the first thing
    // here that can fail or take real time, so it gets its own breadcrumb: "died inside rust_work"
    // was as far as the worker's own stages could localise a fault, and this is one of the three
    // things inside that span.
    let mut out: Vec<u8> = vec![0u8; cap];
    stage("dom_rs_buf");

    // SAFETY: `html` and `out` are live for the call and their lengths are what is passed; the
    // bridge writes at most `cap` bytes and reports how many. It holds neither pointer afterwards —
    // the DOM it builds is destroyed before it returns, which is the contract that lets this run on
    // a thread whose heap is its own.
    let n = unsafe {
        ffi::dom_build(
            html.as_ptr(),
            html.len() as i32,
            width,
            &pal,
            out.as_mut_ptr(),
            cap as i32,
        )
    };
    stage("dom_rs_returned");
    if n < 0 {
        return Err(error_of(n));
    }
    let n = n as usize;
    if n > cap {
        return Err(Error::Malformed);
    }
    out.truncate(n);
    symbian_layout::wire::decode(&out).ok_or(Error::Malformed)
}

/// Write a breadcrumb, through the bridge's own C helper.
///
/// Rust's own logging keeps a file session opened by the GUI thread, and a file server session
/// belongs to the thread that opened it — so it cannot be used from the worker. The C helper opens
/// its own per call, for exactly that reason.
fn stage(tag: &str) {
    // A fixed buffer and a NUL, because the C side takes a C string and this must not allocate: it
    // is called around the allocation it is there to diagnose.
    let mut buf = [0u8; 32];
    let n = tag.len().min(buf.len() - 1);
    buf[..n].copy_from_slice(&tag.as_bytes()[..n]);
    unsafe { ffi::dom_stage(buf.as_ptr()) };
}

/// The layers the self test walks, smallest first.
///
/// Names for reporting; the numbers are the `DOM_SELF_*` constants in `shim/inc/dom_bridge.h` and
/// the order is the bisect order, so the last one that returns is the boundary.
pub const SELFTEST_STEPS: &[&str] = &[
    "malloc",
    "snprintf",
    "strtod",
    "lwc_intern",
    // The inside of hubbub_create, before hubbub_create itself: the order here *is* the bisect
    // order, and a step that kills the thread ends the run, so a finer step placed after a coarser
    // one would never run.
    "hubbub_parts",
    "hubbub_create",
    "dom_create",
];

/// Run one primitive on the calling thread.
///
/// `Ok(())` if it completed. `Err` if the bridge reported failure — and *no return at all* if the
/// thread died, which is the case this exists to catch: the breadcrumb left in `domstage.txt` names
/// the layer. See the header for why a bisect replaced a fifth theory.
pub fn selftest(step: usize) -> Result<(), Error> {
    let rc = unsafe { ffi::dom_selftest(step as i32) };
    if rc == 0 {
        Ok(())
    } else {
        Err(error_of(rc))
    }
}

#[cfg(target_vendor = "symbian")]
mod ffi {
    #[repr(C)]
    pub struct DomPalette {
        pub text: u32,
        pub dim: u32,
        pub link: u32,
    }

    extern "C" {
        pub fn dom_build(
            html: *const u8,
            html_len: i32,
            width: i32,
            palette: *const DomPalette,
            out: *mut u8,
            out_cap: i32,
        ) -> i32;
        pub fn dom_stage(tag: *const u8);
        pub fn dom_selftest(step: i32) -> i32;
    }
}

/// The host has no NetSurf archives and no cross compiler, so the bridge is absent.
///
/// A stub rather than a `cfg` at every call site: the browser's own logic is host-testable and only
/// the parse is not, so the call has to exist and answer honestly.
#[cfg(not(target_vendor = "symbian"))]
mod ffi {
    #[repr(C)]
    pub struct DomPalette {
        pub text: u32,
        pub dim: u32,
        pub link: u32,
    }

    pub unsafe fn dom_stage(_tag: *const u8) {}

    pub unsafe fn dom_selftest(_step: i32) -> i32 {
        -100
    }

    pub unsafe fn dom_build(
        _html: *const u8,
        _html_len: i32,
        _width: i32,
        _palette: *const DomPalette,
        _out: *mut u8,
        _out_cap: i32,
    ) -> i32 {
        // Distinct from every bridge error, so a host test cannot mistake "no bridge here" for a
        // page the bridge rejected.
        -100
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On the host the bridge is absent, and that must be its own answer — not `Parse`, which would
    /// read as a document the parser refused.
    #[test]
    fn the_host_has_no_bridge_and_says_so() {
        // `matches!`, not `assert_eq!`: a StyledTree is not comparable, and making it so for the
        // benefit of one test would put an equality on a type whose identity is its shape.
        assert!(matches!(
            parse(b"<p>hi</p>", 320, Palette::default()),
            Err(Error::Internal(-100))
        ));
    }

    /// Refusals that do not need the bridge happen before the call.
    #[test]
    fn an_empty_document_is_refused_here() {
        assert!(matches!(parse(b"", 320, Palette::default()), Err(Error::Argument)));
        assert!(matches!(
            parse_with_cap(b"<p>x</p>", 320, Palette::default(), 0),
            Err(Error::Argument)
        ));
    }

    /// Every bridge code maps to something a caller can act on, and an unknown one keeps its number
    /// rather than becoming a generic failure.
    #[test]
    fn error_codes_map_to_stages() {
        assert_eq!(error_of(-1), Error::Argument);
        assert_eq!(error_of(-2), Error::NoMemory);
        assert_eq!(error_of(-3), Error::Parse);
        assert_eq!(error_of(-4), Error::Css);
        assert_eq!(error_of(-5), Error::TooLarge);
        assert_eq!(error_of(-99), Error::Internal(-99));
    }

    /// The default palette is the web's, not a theme's. Asserted because the UA stylesheet carries
    /// no colours on purpose and this is what fills the gap.
    #[test]
    fn the_default_palette_is_the_webs() {
        let p = Palette::default();
        assert_eq!(p.text, symbian_layout::css::INK);
        assert_eq!(p.link, symbian_layout::css::LINK);
    }
}
