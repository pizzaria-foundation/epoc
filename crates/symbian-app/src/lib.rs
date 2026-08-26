//! The device side of an application, as one macro.
//!
//! ```ignore
//! #![no_std]
//! #![no_main]
//! extern crate alloc;
//!
//! symbian_app::entry!(MyApp::new());
//! ```
//!
//! That is the whole of a device crate. The macro supplies the allocator, the panic
//! handler, the three `extern "C"` entry points the shim calls, the event translation and
//! the theme.
//!
//! # Why a macro and not a function
//!
//! `#[global_allocator]` and `#[panic_handler]` are lang items, and a lang item can be
//! defined exactly once in a linked program. A library cannot provide them — the moment
//! two crates in a dependency graph both did, nothing would link. So they have to be
//! defined *in the final staticlib*, which is what a macro does: it expands in the
//! caller's crate, so the items land there while the code behind them lives here.
//!
//! Before this existed, every app carried its own copy of the allocator, the panic
//! handler, the key translation and the framebuffer setup — around 120 lines of `unsafe`
//! per app, duplicated three times across the reference app and two diagnostics. Three
//! copies is where the copies start drifting.
//!
//! # What it does not do
//!
//! Own the loop. Avkon calls `CActiveScheduler::Start()` and never returns until the app
//! exits, so `rust_step` is a callee that must return promptly: it runs on the GUI thread
//! from a `CIdle`, and a long one starves the window server, which freezes the whole
//! phone rather than just this app.

#![no_std]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;

use symbian_gfx::{BitmapFont, Canvas, Size, WithFallback};
use symbian_sys as sys;
use symbian_ui::{Fonts, Key, KeyEvent, Modifiers, Palette, Softkey, Theme};

// Re-exported so the macro's expansion can name everything through `$crate` and the
// caller needs no imports of its own.
pub use symbian_gfx;
pub use symbian_keys;
pub use symbian_sys;
pub use symbian_ui;

/// Re-exported so the macro can name `Box` without the caller having imported it.
pub use alloc::boxed::Box as __Box;

/// `RHeap` aligns to 8 bytes. Anything stricter has to be arranged by hand.
const NATIVE_ALIGN: usize = 8;

/// The allocator, over the shim's non-leaving `User::Alloc`.
///
/// Zero-sized, so it occupies no writable static data — which matters because elf2e32
/// rejects a DLL that has any. We ship EXEs, where it would be allowed, but keeping the
/// property costs nothing and makes the crate reusable if that ever changes.
pub struct Heap;

unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if l.align() <= NATIVE_ALIGN {
            return unsafe { sys::shim_alloc(l.size() as u32) } as *mut u8;
        }
        // Over-allocate and record the shift in the word below the aligned pointer, so
        // dealloc can find the original cell. `RHeap`'s 8-byte guarantee is documented
        // but not something to bet a heap on, and this path costs nothing for the 99% of
        // allocations that never need it.
        let total = l.size() + l.align() + core::mem::size_of::<usize>();
        let raw = unsafe { sys::shim_alloc(total as u32) } as usize;
        if raw == 0 {
            return ptr::null_mut();
        }
        let base = raw + core::mem::size_of::<usize>();
        let aligned = (base + l.align() - 1) & !(l.align() - 1);
        unsafe { *((aligned - core::mem::size_of::<usize>()) as *mut usize) = raw };
        aligned as *mut u8
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if l.align() <= NATIVE_ALIGN {
            unsafe { sys::shim_free(p as *mut _) };
        } else {
            let raw = unsafe { *((p as usize - core::mem::size_of::<usize>()) as *const usize) };
            unsafe { sys::shim_free(raw as *mut _) };
        }
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        if l.align() <= NATIVE_ALIGN {
            // Let RHeap grow the cell in place when it can; that is the whole reason to
            // forward realloc rather than alloc-copy-free.
            return unsafe { sys::shim_realloc(p as *mut _, new as u32) } as *mut u8;
        }
        let np = unsafe { self.alloc(Layout::from_size_align_unchecked(new, l.align())) };
        if !np.is_null() {
            unsafe {
                ptr::copy_nonoverlapping(p, np, core::cmp::min(l.size(), new));
                self.dealloc(p, l);
            }
        }
        np
    }
}

/// The panic handler's body. `shim_panic` calls `User::Panic` and never returns.
///
/// A Rust panic must not unwind into C++: we build `panic=abort`, so there are no
/// landing pads on either side of the boundary, and a throw crossing a Rust frame skips
/// every `Drop`.
pub fn panic_to_shim(info: &core::panic::PanicInfo<'_>) -> ! {
    match info.location() {
        Some(l) => unsafe { sys::shim_panic(l.file().as_ptr(), l.file().len() as u32, l.line()) },
        None => unsafe { sys::shim_panic(ptr::null(), 0, 0) },
    }
}

// The atlases every app links. Held here rather than in each app so a new project gets
// working text without deciding anything, and so all apps agree on what "body" and
// "small" mean. ~87 KB, against 250 MB of device storage.
static UI_BODY: &[u8] = include_bytes!("../../symbian-ui/assets/ui11.sbf");
static UI_STRONG: &[u8] = include_bytes!("../../symbian-ui/assets/ui11b.sbf");
static UI_SMALL: &[u8] = include_bytes!("../../symbian-ui/assets/ui9.sbf");
/// Emoji, chained behind body and strong rather than merged into either.
///
/// One atlas for both weights because emoji have no bold — Noto Emoji ships a single
/// weight, so merging would store two identical copies of every glyph in an image where
/// `.rodata` is already the largest section. Not chained behind `small`, which draws
/// timestamps and delivery ticks.
static UI_EMOJI: &[u8] = include_bytes!("../../symbian-ui/assets/uiemoji11.sbf");

/// The atlas bytes the device theme is built from.
///
/// Public because a `Theme` cannot cross a thread boundary — it borrows the `BitmapFont`s that
/// borrow these — and the browser's page layout runs on the **worker thread**, where it still has to
/// measure text. The bytes are `'static`, so a worker can build its own fonts from them and no
/// borrow escapes the thread that made it.
///
/// Roles, not sizes: `BODY`/`STRONG`/`SMALL` are what [`with_theme`] means by those words, and a
/// caller that picks a file by name instead would drift from the toolkit the first time either
/// changed.
/// Which palette this phone's applications use. See the module docs.
pub mod lang_pref;
pub mod theme_pref;

pub mod atlas {
    /// Body text. `ui11`.
    pub const BODY: &[u8] = super::UI_BODY;
    /// Emphasis, and what the title bar uses. `ui11b`.
    pub const STRONG: &[u8] = super::UI_STRONG;
    /// Smaller than body. `ui9`.
    pub const SMALL: &[u8] = super::UI_SMALL;
    /// Chained behind body and strong. Metrics always come from the text atlas.
    pub const EMOJI: &[u8] = super::UI_EMOJI;
}

/// Build a theme and hand it to `f`.
///
/// A closure rather than a return value because `Theme` borrows its font atlases, so it
/// cannot outlive the `BitmapFont`s it points at. The atlases are parsed per call rather
/// than cached in a static — caching one would need a self-referential static, and
/// parsing is a header read plus a bounds check, which is nothing against a full repaint.
/// Adopt the phone's language, so every `strings!` table answers in it.
///
/// Called by [`entry!`] before the application is constructed, and that ordering is the point: a
/// constructor may build a title or a softkey label, and one built before this ran would be English
/// on a Portuguese phone and stay that way for the life of the screen. The same class of mistake as
/// resolving a palette before publishing it, which cost a release to find.
///
/// Fails open in the only way it can. `symbian::locale::language` maps everything it does not
/// recognise — including the error the host stub returns — onto English, so this cannot leave an
/// application with no language at all.
///
/// # An application that wants its own
///
/// This passes [`lang_pref::Choice::Follow`], which is what almost every application wants: the
/// launcher's choice, or the phone's if there is none. An application with a language setting of its
/// own calls [`lang_pref::load_system`] again with its own choice, after this has run.
/// Record which version of this application is running, where the package manager can read it.
///
/// # Why every application, rather than the ones that opt in
///
/// It *was* opt-in — `symbian::pkg::stamp()`, called by whoever remembered — and exactly one
/// application in seven remembered. The consequence was not a missing feature but a wrong one: the
/// package database's `stamps` flag drove which *proof* an update is held to, so six applications
/// were being proved by `Proof::Launch`, the weaker test that asks only whether the platform
/// accepted a launch.
///
/// The thing being opted into is being *manageable*, and that is a property of every application
/// installed from a package rather than a preference each author holds. So it happens here, once,
/// for all of them.
///
/// # It fails quietly
///
/// A stamp that cannot be written costs automatic updates, not the application. `apps/launcher`
/// said it first and it is right: nothing here is worth a word to the user, and a start-up that
/// refused to continue over a bookkeeping file would be worse than the bookkeeping being absent.
pub fn stamp_version() {
    if let Err(e) = symbian::pkg::stamp() {
        symbian::log!("version stamp err={e:?}");
    }
}

pub fn adopt_language() {
    lang_pref::load_system(lang_pref::Choice::Follow);
}

pub fn with_theme<R>(palette: Palette, f: impl FnOnce(&Theme<'_>) -> R) -> R {
    let body = BitmapFont::new(UI_BODY).expect("ui11 atlas is malformed");
    let strong = BitmapFont::new(UI_STRONG).expect("ui11b atlas is malformed");
    let small = BitmapFont::new(UI_SMALL).expect("ui9 atlas is malformed");
    let emoji = BitmapFont::new(UI_EMOJI).expect("uiemoji11 atlas is malformed");
    // Metrics still come from the text atlas, so chaining changes no layout anywhere: a
    // line with an emoji in it is the same height as one without.
    let body_e = WithFallback::new(body, emoji);
    let strong_e = WithFallback::new(strong, emoji);
    let fonts = Fonts { body: &body_e, strong: &strong_e, small: &small, title: &strong_e };
    f(&Theme::new(palette, fonts))
}

/// The screen size the shim reports, or the E72's panel if it is not ready yet.
pub fn screen_size() -> Size {
    let (mut w, mut h) = (0i32, 0i32);
    if unsafe { sys::shim_screen_size(&mut w, &mut h) } != sys::SHIM_OK || w <= 0 || h <= 0 {
        // Only reached before the surface exists, in which case nothing will be drawn
        // anyway — but a sane default beats a zero-sized canvas.
        return Size::new(320, 240);
    }
    Size::new(w, h)
}

/// The keys the shim gives an abstract id to, which are exactly the ones that carry no
/// character. `None` means "the shim had no name for this", which is not an error — it is
/// how a dead key arrives.
fn named_key(id: i32) -> Option<Key> {
    Some(match id {
        sys::key::UP => Key::Up,
        sys::key::DOWN => Key::Down,
        sys::key::LEFT => Key::Left,
        sys::key::RIGHT => Key::Right,
        sys::key::SELECT => Key::Select,
        sys::key::SOFT_LEFT => Key::Softkey(Softkey::Left),
        sys::key::SOFT_MIDDLE => Key::Softkey(Softkey::Middle),
        sys::key::SOFT_RIGHT => Key::Softkey(Softkey::Right),
        sys::key::BACKSPACE => Key::Backspace,
        sys::key::DELETE => Key::Delete,
        sys::key::ENTER => Key::Enter,
        sys::key::CALL => Key::Call,
        sys::key::END => Key::End,
        _ => return None,
    })
}

/// Up to two key events from one shim event.
///
/// Two, because a dead key followed by a letter it cannot combine with produces both
/// characters: `´` then `q` is `´q`. Dropping the mark would make a mistyped accent vanish
/// with no trace, which is the class of bug this whole path exists to remove. Iterate with
/// `.into_iter().flatten()`.
pub type KeyEvents = [Option<KeyEvent>; 2];

/// The process-wide keyboard: which layout, and any accent waiting for its letter.
///
/// A static because dead-key composition is state that has to live *between* two events,
/// and there is exactly one keyboard on the device. It lives here rather than in `entry!`'s
/// expansion so that [`set_keyboard_layout`] can reach it — a static inside the macro would
/// be private to the app crate it expanded in.
///
/// Defaults to the ABNT2 E72 layout, which is behaviour-preserving rather than
/// presumptuous: a key the table does not claim falls through to the character the window
/// server translated, exactly as before this existed.
static mut KEYBOARD: symbian_keys::Keyboard =
    symbian_keys::Keyboard::new(symbian_keys::Layout::Abnt2E72);

/// Choose a keyboard layout. Call it before the first keystroke — from the app's
/// constructor, or `rust_app_start`.
///
/// [`symbian_keys::Layout::PassThrough`] is the conservative choice for a handset nobody
/// has measured: it uses the character the window server produced and nothing else. There
/// is deliberately no auto-detection — the SDK has one handset to check against, and a
/// wrong guess here types the wrong character, which is the kind of bug that gets blamed
/// on the app for weeks.
pub fn set_keyboard_layout(layout: symbian_keys::Layout) {
    // Through a raw pointer rather than `&mut KEYBOARD`, because taking a reference to a mutable
    // static is what `static_mut_refs` exists to stop; the pointer is bound first so the `&raw`
    // and the `*` are not adjacent, which is all clippy's `deref_addrof` is looking at.
    let kb = &raw mut KEYBOARD;
    // SAFETY: single-threaded; every caller is the GUI thread via the shim.
    unsafe { (*kb).set_layout(layout) }
}

/// Translate one shim event using the process keyboard. What the event pump calls.
pub fn translate_keys(e: &sys::ShimEvent) -> KeyEvents {
    let kb = &raw mut KEYBOARD;
    // SAFETY: single-threaded; every caller is the GUI thread via the shim.
    to_key_events(unsafe { &mut *kb }, e)
}

/// Translate one shim event into toolkit key events, advancing the given keyboard's
/// composition state. Empty for events that are not keys, which is most of them.
///
/// Takes the keyboard explicitly so it is testable on the host; [`translate_keys`] is the
/// same thing against the process-wide one.
///
/// # Why this takes a keyboard
///
/// Because the answer depends on what was typed before it. On an ABNT2 keyboard the accent
/// keys are dead keys: they produce nothing themselves and modify the next character. That
/// is one bit of state, and it has to live somewhere between two events.
///
/// The alternative would have been to let Avkon's FEP do the composition, which is what
/// the platform normally does — but taking the FEP means handing it authority over a caret
/// and a text buffer the toolkit already owns. Two components holding one buffer is the
/// bug, not the wiring. So the composition is ours, and this is where it happens.
pub fn to_key_events(kb: &mut symbian_keys::Keyboard, e: &sys::ShimEvent) -> KeyEvents {
    let mods = Modifiers {
        shift: e.b & sys::modifier::SHIFT != 0,
        ctrl: e.b & sys::modifier::CTRL != 0,
        func: e.b & sys::modifier::FUNC != 0,
        func_held: e.b & sys::modifier::FUNC_HELD != 0,
    };
    let repeat = e.c > 0;
    let one = |key| [Some(KeyEvent { key, mods, repeat }), None];

    // A Ctrl chord, before anything else looks at the key.
    //
    // It has to come first, and the reason is the codes: Ctrl+letter arrives as the *control
    // character* for that letter — Ctrl+C is 0x03, and Ctrl+M is 0x0D, which is also Enter, and
    // Ctrl+H is 0x08, which is also Backspace. Read in the ordinary order, half the alphabet
    // would arrive as some other key entirely, and the keypad-overlay table would answer for the
    // rest (Ctrl+M typed `0`, because the layout treats Ctrl as another name for Fn).
    //
    // The chord is resolved here rather than in the shim because the shim reports what the
    // hardware sent and nothing more; what a key *means* has always been decided on this side.
    if mods.ctrl {
        if let Some(key) = ctrl_chord(e) {
            kb.cancel();
            return one(key);
        }
    }

    match e.kind {
        sys::SHIM_EV_KEY_CHAR => strokes(kb, e, mods, repeat, true).unwrap_or([None, None]),
        sys::SHIM_EV_KEY_DOWN => {
            if let Some(key) = named_key(e.a) {
                // A pending accent has no business surviving a keystroke that is not
                // text. The arrows are the exception: arming an accent and then scrolling
                // to where you meant to type it is reasonable, and the platform's own FEP
                // behaves the same way.
                if !matches!(key, Key::Up | Key::Down | Key::Left | Key::Right) {
                    kb.cancel();
                }
                return one(key);
            }
            // No name for it. The layout may still claim it, and this is the path a dead
            // key takes: the window server reports `~` as EKeyF21 (0xF82A), a
            // non-character code, so it can only be recognised by its scan code.
            //
            // The layout table only — never the character in `a`. This event carries no
            // character by construction (anything the window server translated arrives as
            // SHIM_EV_KEY_CHAR), so `a` is a key code that may merely *look* like text.
            // Falling back to it would make an unrecognised hardware key type whatever
            // letter its code happens to spell.
            if let Some(out) = strokes(kb, e, mods, repeat, false) {
                return out;
            }
            // Report it rather than drop it. A silently discarded key is how the E72's Fn
            // key stayed invisible through two rounds of on-device debugging.
            one(Key::Raw(e.a as u16))
        }
        _ => [None, None],
    }
}

/// The phone's clipboard, as the toolkit's [`symbian_ui::Clipboard`].
///
/// The join between a widget that must not know what a device is and a platform that keeps its
/// clipboard in a stream store on disk. It lives in this crate because this is the one that
/// already depends on both — the same reason the key pump and the allocator are here.
///
/// Zero-sized: there is no session to hold. `symbian::clipboard` opens and closes its own file
/// server session per call, since a copy happens a few times a day and a session held for the life
/// of the process to serve it would be the wrong trade.
///
/// ```ignore
/// // Every text field in an app, with paste and copy already in it:
/// self.composer.handle_key(ev, &mut symbian_app::SystemClipboard);
/// ```
///
/// An app built without `USE_CLIPBOARD=1` links a shim stub that answers "not supported", so this
/// degrades to doing nothing rather than failing to load — which is what makes it safe to pass
/// unconditionally.
#[derive(Copy, Clone, Debug, Default)]
pub struct SystemClipboard;

impl symbian_ui::Clipboard for SystemClipboard {
    fn get(&mut self) -> Option<alloc::string::String> {
        #[cfg(not(target_vendor = "symbian"))]
        return host_clip(None);
        // An empty clipboard reports NotFound, which is not an error worth showing anyone: there
        // is simply nothing to paste, and the platform's own Paste is silent about it too.
        #[cfg(target_vendor = "symbian")]
        symbian::clipboard::get_text().ok().filter(|s| !s.is_empty())
    }

    fn set(&mut self, text: &str) -> bool {
        #[cfg(not(target_vendor = "symbian"))]
        return host_clip(Some(text)).is_some();
        #[cfg(target_vendor = "symbian")]
        symbian::clipboard::set_text(text).is_ok()
    }
}

/// The simulator's clipboard: a `String` in this process.
///
/// Off the device every shim call answers "not ready", which would make copy and paste the two
/// features nobody could try without a handset — and they are exactly the features where the feel
/// of them is the thing to judge. So the host build keeps its own, and the simulator behaves like
/// the phone: copy in one field, paste in another.
///
/// `Some(text)` stores and returns it; `None` reads.
#[cfg(not(target_vendor = "symbian"))]
fn host_clip(set: Option<&str>) -> Option<alloc::string::String> {
    use alloc::string::ToString;
    static mut CLIP: Option<alloc::string::String> = None;
    let clip = &raw const CLIP;
    // SAFETY: single-threaded, GUI thread only — the same rule the keyboard state above follows.
    unsafe {
        if let Some(text) = set {
            CLIP = Some(text.to_string());
        }
        (*clip).clone()
    }
}

/// The letter of a Ctrl chord, when this event is one.
///
/// Two shapes, because two kinds of keyboard produce them:
///
/// - the handset, which sends the control character (`0x01..=0x1A` for A..Z) with the Ctrl bit
///   set — the phone's own editors read exactly this;
/// - a Bluetooth or emulated keyboard, which may send the letter itself with the bit set.
///
/// Anything else with Ctrl held — a digit, a symbol, an arrow — is left alone, and goes on to be
/// whatever it would have been without the modifier. Reporting those as chords would invent
/// bindings the platform does not have.
fn ctrl_chord(e: &sys::ShimEvent) -> Option<Key> {
    let code = e.a as u32;
    let letter = match code {
        0x01..=0x1A => char::from_u32(code + 0x60),
        _ => char::from_u32(code).filter(|c| c.is_ascii_alphabetic()).map(|c| c.to_ascii_lowercase()),
    }?;
    Some(Key::Ctrl(letter))
}

/// Run one event through the keyboard.
///
/// `None` means the keyboard had nothing to say about this event at all, which the caller
/// must distinguish from "it absorbed a dead key and deliberately produced nothing" —
/// reporting the latter as `Key::Raw` would put a stray 0xF82A into a text field.
///
/// The two are told apart by whether the pending mark changed, rather than by inspecting
/// the stroke: a dead key pressed while another is already armed emits the first and arms
/// the second, so "produced nothing" and "changed state" are genuinely independent.
/// `translated` says whether `e.a` holds a character the window server produced, or a key
/// code that is not text however much it may look like one.
fn strokes(
    kb: &mut symbian_keys::Keyboard,
    e: &sys::ShimEvent,
    mods: Modifiers,
    repeat: bool,
    translated: bool,
) -> Option<KeyEvents> {
    let press = symbian_keys::Press {
        // `e.a` is a UCS-2 code unit and `e.d` the scan code, both carried on every key
        // event the shim pushes, so the layout needs nothing new across the ABI.
        code: e.a as u16,
        scan: e.d as u16,
        shift: mods.shift,
        func: mods.func,
        ctrl: mods.ctrl,
    };
    let before = kb.pending();
    let ev = |c| Some(KeyEvent { key: Key::Char(c), mods, repeat });
    let stroke =
        if translated { kb.translate(press) } else { kb.translate_mapped(press) };
    match stroke {
        symbian_keys::Stroke::One(c) => Some([ev(c), None]),
        symbian_keys::Stroke::Two(a, b) => Some([ev(a), ev(b)]),
        symbian_keys::Stroke::None if kb.pending() != before => Some([None, None]),
        symbian_keys::Stroke::None => None,
    }
}

/// The default `work` handler: there is no worker.
///
/// A real function rather than an absent symbol, because the shim's C++ references
/// `rust_work` unconditionally and `--no-undefined` would refuse a link that left it
/// dangling.
pub fn no_work(_opcode: i32, _input: &[u8], _out: &mut [u8]) -> i32 {
    sys::SHIM_ERR_NOT_SUPPORTED
}

/// Copy a shim event into the toolkit's platform-independent view of one.
pub fn to_raw_event(e: &sys::ShimEvent) -> symbian_ui::RawEvent {
    symbian_ui::RawEvent {
        kind: e.kind,
        handle: e.handle,
        status: e.status,
        a: e.a,
        b: e.b,
        c: e.c,
        d: e.d,
        native: e.native,
    }
}

/// Lock the framebuffer, hand a `Canvas` over it to `f`, unlock and present the whole
/// screen. Kept for callers that always want a full present; [`present_damaged`] is the
/// dirty-rect version the app entry point uses.
///
/// Returns false if the surface was not available, in which case nothing was drawn.
pub fn present(f: impl FnOnce(&mut Canvas<'_>)) -> bool {
    present_damaged(true, f)
}

/// Draw through a `Canvas` and present only what actually changed.
///
/// The present (RGB565→native expand + `BitBlt`) is ~96% of the frame on the E72 and is
/// proportional to the area blitted, so presenting only the damaged rectangle — the union
/// of pixels whose value the frame changed, tracked by [`Canvas::damage`] — is the one
/// optimisation that moves frame time. The staging buffer persists between frames, so a
/// `clear` to the same background as last frame changes no pixels and presents nothing.
///
/// `force_full` overrides that and presents the whole screen: the first frame, and after a
/// redraw/resize/foreground event, the on-screen pixels may not match our staging buffer
/// (another app drew over the window), so the damage rectangle would under-present.
///
/// Returns false if the surface was not available.
pub fn present_damaged(force_full: bool, f: impl FnOnce(&mut Canvas<'_>)) -> bool {
    let mut fb = sys::ShimFb::default();
    if unsafe { sys::shim_fb_lock(&mut fb) } != sys::SHIM_OK || fb.pixels.is_null() {
        return false;
    }
    // stride is in bytes and the buffer is RGB565, so two bytes a pixel.
    let stride_px = (fb.stride / 2) as usize;
    let len = stride_px * fb.height as usize;

    // SAFETY: the shim guarantees the pointer is valid for stride*height bytes until
    // shim_fb_unlock, and that nothing else touches it meanwhile. The buffer is ordinary
    // memory the shim allocated, not the FBS chunk, so it does not move under us.
    let pixels: &mut [u16] =
        unsafe { core::slice::from_raw_parts_mut(fb.pixels as *mut u16, len) };
    let damage = {
        let mut c = Canvas::new(pixels, Size::new(fb.width, fb.height), stride_px);
        f(&mut c);
        c.damage()
    };

    // The rectangle to push. Partial (dirty-rect) present is temporarily forced off: it
    // destabilised the app on the device when the conversation view was drawn, and the
    // cause is not yet root-caused. Presenting the whole screen makes the draw path
    // identical to the pre-change build (which was stable), while `damage` is still tracked
    // so the optimisation can be switched back on once the crash is understood.
    let _ = (force_full, damage);
    let rect = Some((0, 0, fb.width, fb.height));

    unsafe {
        sys::shim_fb_unlock();
        if let Some((x, y, w, h)) = rect {
            sys::shim_present(x, y, w, h);
            // Instrument the saving: how many pixels we actually pushed versus what a
            // full-screen present would have cost. The E72 self-test reads this to turn
            // "dirty-rect present" from a claim into a measurement.
            PRESENT_PX += (w as u64) * (h as u64);
            PRESENT_FULL_PX += (fb.width as u64) * (fb.height as u64);
            PRESENT_FRAMES += 1;
        }
    }
    true
}

// Present statistics, GUI-thread only (like __SYMBIAN_PAINTED), so a plain static.
static mut PRESENT_PX: u64 = 0;
static mut PRESENT_FULL_PX: u64 = 0;
static mut PRESENT_FRAMES: u64 = 0;

/// Cumulative present accounting since the last call, then reset:
/// `(pixels_presented, pixels_if_every_frame_were_full, frames_presented)`.
///
/// `pixels_presented / pixels_if_full` is the fraction of the blit cost the dirty-rect
/// present actually paid — well under 1 means it is working, 1.0 means every frame went
/// full (all content changing, or `force_full` every time). A diagnostic
/// log reads this to measure the win rather than assume it.
pub fn present_stats() -> (u64, u64, u64) {
    // SAFETY: single-threaded; every caller is the GUI thread, as for the paint flag.
    unsafe {
        let out = (PRESENT_PX, PRESENT_FULL_PX, PRESENT_FRAMES);
        PRESENT_PX = 0;
        PRESENT_FULL_PX = 0;
        PRESENT_FRAMES = 0;
        out
    }
}

/// A monotonic timestamp in microseconds, for timing `rust_step`. Thin wrapper so the
/// `entry!` macro can reach it as `$crate::now_us()` without naming `symbian_sys`.
#[inline]
pub fn now_us() -> u64 {
    // SAFETY: no arguments; reads the nanokernel tick.
    unsafe { sys::shim_now_us() }
}

static mut GFX_LAST_PUBLISH_US: u64 = 0;
static mut GFX_DEFINED: bool = false;

/// How rarely present telemetry is published. The counters are free; publishing is a P&S
/// `Set` (microseconds, no disk I/O), but at most this often so the trace cannot distort the
/// frame time it measures — one clock read per step is its whole per-frame cost.
const GFX_PUBLISH_INTERVAL_US: u64 = 1_000_000;

/// Publish this app's present efficiency to Publish & Subscribe so the dev bridge can stream it as
/// `[gfx]`, at most once per second. Published in the app's own UID3 category (no capability
/// to write your own), key [`PS_KEY_GFX`]; the value is the percentage of blit cost the
/// dirty-rect present saved since the last publish (0–100).
///
/// Called automatically from `entry!` after each present, so every SDK app exposes the
/// metric with no per-app code. Deliberately *not* a file write: on this device file I/O on
/// the GUI thread would lengthen `rust_step`, which is exactly what the metric watches.
pub fn publish_present_stats() {
    // SAFETY: single-threaded (GUI thread), like the paint flag.
    let now = unsafe { sys::shim_now_us() };
    unsafe {
        if now.wrapping_sub(GFX_LAST_PUBLISH_US) < GFX_PUBLISH_INTERVAL_US {
            return;
        }
        GFX_LAST_PUBLISH_US = now;
    }

    let (px, full, frames) = present_stats();
    if frames == 0 || full == 0 {
        return; // nothing presented this window — leave the last value in place
    }
    let saved = 100u64.saturating_sub(px.saturating_mul(100) / full);

    let category = unsafe { sys::shim_own_uid3() };
    if category == 0 {
        return; // build did not set a UID3; nothing to publish under
    }
    // SAFETY: no pointers; define is idempotent and set writes one integer in our own
    // category, which needs no capability.
    unsafe {
        if !GFX_DEFINED {
            let _ = sys::shim_prop_define(category, sys::PS_KEY_GFX);
            GFX_DEFINED = true;
        }
        let _ = sys::shim_prop_set(category, sys::PS_KEY_GFX, saved as i32);
    }
}

// ---- the rust_step watchdog -------------------------------------------------

// Peak per-phase step time, GUI-thread only. A long `rust_step` freezes the whole phone;
// splitting the peak into the handle phase (draining events: input, socket completions,
// decode kickoffs) versus the draw phase says *which* half to chase.
static mut STEP_MAX_HANDLE_US: u64 = 0;
static mut STEP_MAX_DRAW_US: u64 = 0;
static mut STEP_LAST_PUBLISH_US: u64 = 0;
static mut STEP_DEFINED: bool = false;

/// Record one `rust_step`'s two phase durations (µs). Called from `entry!` on every step,
/// so each peak is a true maximum over the window. Pure integer compares — free next to the
/// work they measure.
pub fn record_step(handle_us: u64, draw_us: u64) {
    // SAFETY: single-threaded (GUI thread).
    unsafe {
        if handle_us > STEP_MAX_HANDLE_US {
            STEP_MAX_HANDLE_US = handle_us;
        }
        if draw_us > STEP_MAX_DRAW_US {
            STEP_MAX_DRAW_US = draw_us;
        }
    }
}

/// Worst handle-phase and draw-phase time (µs) since the last call, then reset.
pub fn step_stats() -> (u64, u64) {
    // SAFETY: single-threaded.
    unsafe {
        let out = (STEP_MAX_HANDLE_US, STEP_MAX_DRAW_US);
        STEP_MAX_HANDLE_US = 0;
        STEP_MAX_DRAW_US = 0;
        out
    }
}

/// Publish the step watchdog to P&S so the dev bridge can stream it as
/// `[step]`, at most once a second. The value packs the peak handle time (ms, high 16 bits)
/// and the peak draw time (ms, low 16 bits) — the split that says input-work vs rendering.
pub fn publish_step_stats() {
    let now = now_us();
    // SAFETY: single-threaded.
    unsafe {
        if now.wrapping_sub(STEP_LAST_PUBLISH_US) < GFX_PUBLISH_INTERVAL_US {
            return;
        }
        STEP_LAST_PUBLISH_US = now;
    }
    let (handle_us, draw_us) = step_stats();
    let handle_ms = (handle_us / 1000).min(0x7FFF) as i32;
    let draw_ms = (draw_us / 1000).min(0xFFFF) as i32;
    let packed = (handle_ms << 16) | draw_ms;

    let category = unsafe { sys::shim_own_uid3() };
    if category == 0 {
        return;
    }
    // SAFETY: no pointers; own-category define/set need no capability.
    unsafe {
        if !STEP_DEFINED {
            let _ = sys::shim_prop_define(category, sys::PS_KEY_STEP);
            STEP_DEFINED = true;
        }
        let _ = sys::shim_prop_set(category, sys::PS_KEY_STEP, packed);
    }
}

/// The contract a headless daemon implements — the `App` trait without a screen.
///
/// A daemon has no keys, no theme and nothing to draw; it exists to react to the shim's
/// event stream (timers firing, sockets completing, monitors reporting) and to know when it
/// has been told to stop. So its trait is two methods, and [`daemon_entry!`] drives it with
/// none of the framebuffer, font-atlas or key-translation machinery an `App` needs.
///
/// Kept here rather than in `symbian-ui` because it belongs to the device entry point, not
/// to the widget toolkit: a daemon links neither widgets nor fonts.
pub trait DaemonApp {
    /// A platform event arrived. Everything a daemon does happens here — there is no
    /// separate tick, because its periodic work is itself driven by shim timer events it
    /// arms and receives through this method.
    fn handle_raw(&mut self, ev: &symbian_sys::ShimEvent);

    /// True once the daemon wants to exit. The headless entry acts on it by stopping the
    /// active scheduler, which runs the shim's teardown and lets the process end cleanly —
    /// which is what frees `\sys\bin` so the package can be uninstalled.
    fn should_exit(&self) -> bool {
        false
    }
}

/// Define a headless daemon executable.
///
/// ```ignore
/// symbian_app::daemon_entry!(MyDaemon::new());
/// symbian_app::daemon_entry!(MyDaemon::new(), work = my_worker_fn);
/// ```
///
/// Like [`entry!`], but for an app with no UI: it expands to the allocator, the panic
/// handler and `rust_app_start` / `rust_step` / `rust_app_stop`, where `rust_step` drains
/// the event ring into [`DaemonApp::handle_raw`] and never draws. No fonts are linked, no
/// framebuffer is locked, and no key translation runs — the ~87 KB of font atlases the GUI
/// entry carries are simply absent. The C++ side is `shim_daemon.cpp` (a bare
/// `CActiveScheduler`, no Avkon) rather than `shim_app.cpp`.
///
/// The app must implement [`DaemonApp`].
#[macro_export]
macro_rules! daemon_entry {
    ($ctor:expr) => {
        $crate::daemon_entry!($ctor, work = $crate::no_work);
    };
    ($ctor:expr, work = $work:path) => {
        /// Same worker-thread entry as `entry!` — see there. Emitted here too because a
        /// daemon that sets `USE_NET=1` links `shim_work.cpp`, which references `rust_work`,
        /// and `--no-undefined` would refuse the link without it.
        #[no_mangle]
        pub extern "C" fn rust_work(
            opcode: i32,
            input: *const u8,
            in_len: i32,
            out: *mut u8,
            out_len: i32,
        ) -> i32 {
            let input: &[u8] = if in_len > 0 {
                unsafe { core::slice::from_raw_parts(input, in_len as usize) }
            } else {
                &[]
            };
            let out: &mut [u8] = if out_len > 0 {
                unsafe { core::slice::from_raw_parts_mut(out, out_len as usize) }
            } else {
                &mut []
            };
            $work(opcode, input, out)
        }

        #[global_allocator]
        static __SYMBIAN_HEAP: $crate::Heap = $crate::Heap;

        #[panic_handler]
        fn __symbian_panic(info: &core::panic::PanicInfo) -> ! {
            $crate::panic_to_shim(info)
        }

        // Single mutable static, allowed in an EXE (see the note in `entry!`). A boxed
        // trait object so the macro takes one expression rather than a type and a value.
        static mut __SYMBIAN_DAEMON:
            Option<$crate::__Box<dyn $crate::DaemonApp>> = None;

        #[no_mangle]
        pub extern "C" fn rust_app_start() {
            // The stamp, and **not** the language.
            //
            // A daemon is a daemon: what it writes is a log, and a log is English. Making every
            // headless binary read a preference file at start-up to pick a language for text nobody
            // reads is a cost with no reader.
            //
            // One of them is not that, and it calls `lang_pref::load_system` itself because it
            // knows it is the exception: `calsync` fills a status line the *calendar* draws. Text
            // that reaches a screen needs the screen's language whoever wrote it — and a daemon
            // knows whether it writes any far better than this macro does.
            //
            // `notifd` looked like a second one and is not: it forwards words an application wrote,
            // and never composes any of its own.
            //
            // The stamp has no such exception. A daemon installed from a package is a package that
            // can be proved, and one that never says which version is running is one no update of
            // it could ever commit.
            $crate::stamp_version();
            // SAFETY: called once, from shim_daemon.cpp's MainL, before the scheduler runs.
            unsafe {
                __SYMBIAN_DAEMON = Some($crate::__Box::new($ctor));
            }
        }

        #[no_mangle]
        pub extern "C" fn rust_app_stop() {
            // SAFETY: called once, after the scheduler has stopped.
            unsafe {
                __SYMBIAN_DAEMON = None;
            }
        }

        #[no_mangle]
        pub extern "C" fn rust_step() {
            use $crate::DaemonApp as _;

            // SAFETY: single-threaded; every caller is the daemon thread via the pump.
            let app: &mut dyn $crate::DaemonApp =
                match unsafe { (&raw mut __SYMBIAN_DAEMON).as_mut() } {
                    Some(slot) => match slot.as_mut() {
                        Some(b) => &mut **b,
                        None => return,
                    },
                    None => return,
                };

            let mut ev = $crate::symbian_sys::ShimEvent::default();
            // Drain the whole ring each pump tick. A daemon has no frame to coalesce into;
            // this is only to empty the 64-slot ring before it can overflow.
            while unsafe { $crate::symbian_sys::shim_poll_event(&mut ev) } == 1 {
                if ev.kind == $crate::symbian_sys::SHIM_EV_QUIT {
                    unsafe { $crate::symbian_sys::shim_request_exit() };
                    return;
                }
                app.handle_raw(&ev);
            }

            if app.should_exit() {
                unsafe { $crate::symbian_sys::shim_request_exit() };
            }
        }
    };
}

/// Define a device application.
///
/// ```ignore
/// symbian_app::entry!(MyApp::new());
/// symbian_app::entry!(MyApp::new(), palette = symbian_ui::Palette::S60);
/// symbian_app::entry!(MyApp::new(), work = my_worker_fn);
/// ```
///
/// Expands to the allocator, the panic handler and `rust_app_start` / `rust_step` /
/// `rust_app_stop`. The app must implement [`symbian_ui::App`].
///
/// # `work`
///
/// The function the worker thread calls, for computation too slow to run in `rust_step`
/// — see `shim_work_submit`. It must have the signature
///
/// ```ignore
/// fn(opcode: i32, input: &[u8], out: &mut [u8]) -> i32
/// ```
///
/// and it runs on **another thread with its own heap**. Nothing it allocates may
/// outlive it: a temporary is fine, but a value that escapes to the GUI thread would be
/// freed on the wrong heap, which is silent corruption rather than a clean failure.
/// That is why it takes an output slice rather than returning one.
///
/// It is a free function, not a method, precisely so it cannot reach app state.
///
/// Omitted, it becomes a stub returning `SHIM_ERR_NOT_SUPPORTED`, so an app that
/// submits no jobs needs no `work` and links no worker.
#[macro_export]
macro_rules! entry {
    ($ctor:expr) => {
        $crate::entry!(
            $ctor,
            palette = $crate::symbian_ui::Palette::DARK,
            work = $crate::no_work
        );
    };
    ($ctor:expr, palette = $palette:expr) => {
        $crate::entry!($ctor, palette = $palette, work = $crate::no_work);
    };
    ($ctor:expr, work = $work:path) => {
        $crate::entry!($ctor, palette = $crate::symbian_ui::Palette::DARK, work = $work);
    };
    ($ctor:expr, palette = $palette:expr, work = $work:path) => {
        /// Called on the worker thread. The slices are built from the caller's pointers,
        /// which the ABI requires to stay alive until SHIM_EV_WORK_DONE.
        #[no_mangle]
        pub extern "C" fn rust_work(
            opcode: i32,
            input: *const u8,
            in_len: i32,
            out: *mut u8,
            out_len: i32,
        ) -> i32 {
            // SAFETY: the shim validated that a non-zero length comes with a non-null
            // pointer, and the ABI requires both buffers to outlive the job.
            let input: &[u8] = if in_len > 0 {
                unsafe { core::slice::from_raw_parts(input, in_len as usize) }
            } else {
                &[]
            };
            let out: &mut [u8] = if out_len > 0 {
                unsafe { core::slice::from_raw_parts_mut(out, out_len as usize) }
            } else {
                &mut []
            };
            $work(opcode, input, out)
        }

        #[global_allocator]
        static __SYMBIAN_HEAP: $crate::Heap = $crate::Heap;

        #[panic_handler]
        fn __symbian_panic(info: &core::panic::PanicInfo) -> ! {
            $crate::panic_to_shim(info)
        }

        // A single mutable static, which is exactly the thing Symbian forbids in a DLL —
        // but this is an EXE, where writable static data is unrestricted (elf2e32's check
        // is inside `if (isDllp)`). Threading a context pointer through the C ABI instead
        // would buy nothing: the app is single-threaded by construction, because
        // RWsSession is not thread-safe and all drawing happens on the GUI thread.
        // Boxed as a trait object rather than stored by value, which is what lets
        // `entry!` take one expression instead of a type *and* an expression: a `static`
        // needs a concrete type written out, and `Option<impl App>` is not one.
        //
        // The cost is a vtable call on handle_key, draw and should_exit — three per
        // frame, against ~76,800 pixel writes in the same frame. It is not measurable.
        static mut __SYMBIAN_APP:
            Option<$crate::__Box<dyn $crate::symbian_ui::App>> = None;

        /// Set once the first frame has been drawn, so `rust_step` can tell a genuine
        /// no-op from "we have never painted".
        static mut __SYMBIAN_PAINTED: bool = false;

        /// Whether the app is in the background. Drawing is skipped while true to
        /// save battery and CPU, and to avoid competing with whatever is on screen.
        static mut __SYMBIAN_BACKGROUND: bool = false;

        #[no_mangle]
        pub extern "C" fn rust_app_start() {
            // Before the constructor, not after: a constructor that builds a label would build it
            // in the wrong language and keep it. See `adopt_language`.
            $crate::adopt_language();
            // And say which version this is, so the package manager has a witness that does not
            // depend on a daemon having been awake. See `stamp_version`.
            $crate::stamp_version();
            // SAFETY: called exactly once, from CShimAppUi::ConstructL, on the GUI thread.
            unsafe {
                let mut app = $crate::__Box::new($ctor);
                // Hand every app the system clipboard so text fields copy and paste out of the box.
                // Gated on the `clipboard` cargo feature, which symbuild turns on for USE_CLIPBOARD=1
                // (that same flag links the clipboard shim). Off, the line vanishes and nothing pulls
                // shim_clip in, so an app that has not asked for a clipboard links exactly as before.
                #[cfg(feature = "clipboard")]
                $crate::symbian_ui::App::install_clipboard(
                    &mut *app,
                    $crate::__Box::new($crate::SystemClipboard),
                );
                __SYMBIAN_APP = Some(app);
                __SYMBIAN_PAINTED = false;
            }
        }

        #[no_mangle]
        pub extern "C" fn rust_app_stop() {
            // Before the surface goes away, so nothing holds a pointer into it.
            unsafe {
                __SYMBIAN_APP = None;
            }
        }

        #[no_mangle]
        pub extern "C" fn rust_step() {
            use $crate::symbian_ui::App as _;

            // SAFETY: single-threaded; every caller is the GUI thread via the shim.
            let app: &mut dyn $crate::symbian_ui::App =
                match unsafe { (&raw mut __SYMBIAN_APP).as_mut() } {
                    Some(slot) => match slot.as_mut() {
                        Some(b) => &mut **b,
                        None => return,
                    },
                    None => return,
                };

            let size = $crate::screen_size();
            let screen = $crate::symbian_gfx::Rect::from_size(size);

            // Split the step into two timed phases — handling events vs drawing — so a long
            // `rust_step` (which freezes the whole phone) can be attributed to input work or
            // to rendering. No closure: every exit path falls through to the record below.
            let __t0 = $crate::now_us();
            // One theme for the whole step: the fonts are parsed once per frame rather
            // than once per key, and — the reason it has to be this shape — a Theme
            // borrows the atlases, so it cannot escape the closure that owns them.
            // `dirty` decides whether to draw at all; `force_full` decides whether the
            // present covers the whole screen or just the damaged rectangle. The first
            // frame and any redraw/resize/return-to-foreground must be full, because then
            // the on-screen pixels may not match our staging buffer and a dirty-rect
            // present would leave stale pixels behind.
            let (dirty, force_full) = $crate::with_theme($palette, |theme| {
                let first = unsafe { !__SYMBIAN_PAINTED };
                let mut dirty = first;
                let mut force_full = first;
                let mut ev = $crate::symbian_sys::ShimEvent::default();

                // Drain the whole queue before drawing. Coalescing several key presses
                // into one repaint is the difference between keeping up and falling
                // behind when someone holds a key down.
                while unsafe { $crate::symbian_sys::shim_poll_event(&mut ev) } == 1 {
                    match ev.kind {
                        $crate::symbian_sys::SHIM_EV_RESIZE
                        | $crate::symbian_sys::SHIM_EV_REDRAW => {
                            dirty = true;
                            force_full = true;
                        }
                        $crate::symbian_sys::SHIM_EV_FOCUS => {
                            let foreground = ev.a != 0;
                            unsafe { __SYMBIAN_BACKGROUND = !foreground; }
                            dirty = true;
                            // Coming back to the foreground: the window may have been
                            // drawn over, so repaint everything, not just our damage.
                            if foreground {
                                force_full = true;
                            }
                            // And the app hears it. Skipping the draw is all *this* loop can
                            // decide; whether a socket, a timer or a poll should still be
                            // running while nobody is looking is the application's policy,
                            // and until now no app could even ask. The return is ignored
                            // because `dirty` is already settled above.
                            let raw = $crate::to_raw_event(&ev);
                            let _ = app.handle_raw(&raw);
                        }
                        $crate::symbian_sys::SHIM_EV_QUIT => {
                            unsafe { $crate::symbian_sys::shim_request_exit() };
                            return (false, false);
                        }
                        _ => {
                            // Raw first. An app that consumes it does not also get a
                            // translated key, which is what lets a diagnostic see the
                            // numbers the platform sent rather than our reading of them.
                            let raw = $crate::to_raw_event(&ev);
                            if app.handle_raw(&raw) == $crate::symbian_ui::Handled::Consumed {
                                dirty = true;
                            } else {
                                // One event in, up to two keys out: a dead key followed by
                                // a letter it cannot combine with produces both characters,
                                // and the app should see them as two ordinary keystrokes.
                                for k in $crate::translate_keys(&ev).into_iter().flatten() {
                                    if app.handle_key(k, theme, screen)
                                        == $crate::symbian_ui::Handled::Consumed
                                    {
                                        dirty = true;
                                    }
                                }
                            }
                        }
                    }
                }
                (dirty, force_full)
            });
            let __handle_us = $crate::now_us().wrapping_sub(__t0);

            let mut __draw_us = 0u64;
            let __background = unsafe { __SYMBIAN_BACKGROUND };
            if app.should_exit() {
                unsafe { $crate::symbian_sys::shim_request_exit() };
            } else if dirty && !__background {
                // Skip drawing in the background: the user is not looking, and the write
                // competes with whatever is on screen. The buffer stays valid for the return.
                let __draw_t0 = $crate::now_us();
                $crate::with_theme($palette, |theme| {
                    $crate::present_damaged(force_full, |c| app.draw(c, theme));
                });
                __draw_us = $crate::now_us().wrapping_sub(__draw_t0);
                unsafe { __SYMBIAN_PAINTED = true };
                // Present efficiency for the bridge's `[gfx]` line; rate-limited inside.
                $crate::publish_present_stats();
            }

            // Watchdog: peak handle/draw time this window, published once a second over P&S.
            $crate::record_step(__handle_us, __draw_us);
            $crate::publish_step_stats();
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_atlases_the_macro_links_are_parseable() {
        // A malformed atlas would panic inside with_theme on the device, at the first
        // repaint, with no log — so it is checked here, where the failure is a test name.
        for (name, data) in [
            ("ui11", UI_BODY),
            ("ui11b", UI_STRONG),
            ("ui9", UI_SMALL),
            ("uiemoji11", UI_EMOJI),
        ] {
            let f = BitmapFont::new(data);
            assert!(f.is_ok(), "{name}.sbf is malformed: {:?}", f.err());
        }
    }

    #[test]
    fn the_emoji_atlas_aligns_with_the_text_atlases_it_is_chained_to() {
        // Bearings are measured from the ascent the atlas was built with, so a fallback
        // whose ascent disagrees with its primary draws every glyph off the baseline. It is
        // a one-flag mistake in mkfonts.sh (`--ascent 12`) and invisible except on a phone.
        use symbian_gfx::Font as _;
        let body = BitmapFont::new(UI_BODY).unwrap();
        let strong = BitmapFont::new(UI_STRONG).unwrap();
        let emoji = BitmapFont::new(UI_EMOJI).unwrap();
        assert_eq!(emoji.ascent(), body.ascent(), "emoji ascent must match ui11");
        assert_eq!(emoji.ascent(), strong.ascent(), "and ui11b");
    }

    #[test]
    fn the_chained_theme_can_draw_an_emoji_and_a_letter() {
        // What this guards: `mkfont.py`'s charset is a hand-maintained list, and dropping
        // the emoji block from it would leave a build that renders sticker labels and
        // display names as blank space — a missing glyph here paints nothing at all.
        with_theme(Palette::ALL[0].1, |theme| {
            let body = theme.fonts.body;
            assert!(body.glyph('a').is_some(), "ordinary text still works");
            // Grinning face, thumbs up, red heart: one from each block the subset covers.
            for ch in ['\u{1F600}', '\u{1F44D}', '\u{2764}'] {
                assert!(body.glyph(ch).is_some(), "body cannot draw U+{:04X}", ch as u32);
                assert!(
                    theme.fonts.strong.glyph(ch).is_some(),
                    "strong cannot draw U+{:04X} — a display name would go blank",
                    ch as u32
                );
            }
            // And an emoji outside the subset is still absent, so callers keep checking
            // rather than assuming full coverage.
            assert!(body.glyph('\u{1F6F8}').is_none(), "the subset is still a subset");
        });
    }

    #[test]
    fn an_emoji_actually_puts_ink_on_the_canvas() {
        // The precise failure this whole change exists to fix: a codepoint the atlas lacks
        // is not drawn as a box, it is not drawn at all — `mkfont.py` drops the glyph rather
        // than shipping `.notdef`. So "does the font have it" and "does anything appear" are
        // different questions, and only the second one is what a user sees. Asserting on
        // glyph() alone would pass against an atlas full of blank records.
        use symbian_gfx::{Canvas, Color, Point};

        let size = Size::new(64, 24);
        let ink_of = |s: &str| {
            let mut buf = alloc::vec![0u16; (size.w * size.h) as usize];
            with_theme(Palette::ALL[0].1, |theme| {
                let mut c = Canvas::from_slice(&mut buf, size);
                c.clear(Color::hex(0x000000));
                c.draw_text(Point::new(2, 16), s, theme.fonts.body, Color::hex(0xFFFFFF));
            });
            buf.iter().filter(|p| **p != 0).count()
        };

        let lit = ink_of("\u{1F44D}");
        assert!(lit > 20, "the thumbs-up drew {lit} lit pixels; it should be a glyph");

        // And the control, which is what every emoji looked like before this: a codepoint
        // outside the subset paints nothing whatsoever. Without this the assertion above
        // could pass on a threshold that a stray anti-aliased pixel would also clear.
        assert_eq!(ink_of("\u{1F6F8}"), 0, "an absent glyph must draw nothing at all");
    }

    #[test]
    fn every_symbol_the_toolkit_draws_is_in_some_atlas() {
        // The bug this guards against, which shipped and was only caught by looking at a
        // rendered screenshot: the voice-message label asked for U+266A EIGHTH NOTE, and
        // U+266A is in none of the three text fonts *and* not in Noto Emoji. It could never
        // have drawn. Because a missing glyph paints nothing rather than a box, the label
        // read "[ 0:07]" — a hole that looks like a spacing bug, not a font problem.
        //
        // So every non-ASCII codepoint the toolkit or the client puts in a label belongs
        // here. Adding one to a label without adding it here is the mistake; this test is
        // what turns it from a visual puzzle into a failure with a name.
        let wanted: &[(char, &str)] = &[
            ('\u{2026}', "ellipsis, from Font::ellipsis"),
            ('\u{2713}', "check, delivery Sent"),
            ('\u{2714}', "heavy check, delivery Read"),
            ('\u{2022}', "bullet"),
            ('\u{00B7}', "middot, the separator in media labels"),
            ('\u{20AC}', "euro"),
            ('\u{1F5BC}', "framed picture, the photo label"),
            ('\u{1F3A4}', "microphone, the voice-message label"),
            ('\u{1F3B5}', "musical note, the audio-file label"),
            ('\u{1F4CE}', "paperclip, the document label"),
        ];
        with_theme(Palette::ALL[0].1, |theme| {
            for (ch, what) in wanted {
                for (name, font) in [("body", theme.fonts.body), ("strong", theme.fonts.strong)] {
                    assert!(
                        font.glyph(*ch).is_some(),
                        "{name} cannot draw U+{:04X} ({what}) — it would render as a hole",
                        *ch as u32,
                    );
                }
            }
        });
    }

    /// A keyboard that applies no layout, so these tests exercise the translation and not
    /// the measured table. The table has its own tests in `symbian-keys`.
    fn plain() -> symbian_keys::Keyboard {
        symbian_keys::Keyboard::new(symbian_keys::Layout::PassThrough)
    }

    /// The single key an event produced, or `None`.
    fn only(kb: &mut symbian_keys::Keyboard, ev: &sys::ShimEvent) -> Option<KeyEvent> {
        let out = to_key_events(kb, ev);
        assert!(out[1].is_none(), "expected at most one key, got two");
        out[0]
    }

    fn char_event(c: char) -> sys::ShimEvent {
        sys::ShimEvent {
            kind: sys::SHIM_EV_KEY_CHAR,
            a: c as i32,
            // A scan code no layout claims, so the character in `a` is what is used.
            d: 0x0F01,
            ..Default::default()
        }
    }

    #[test]
    fn a_char_event_becomes_a_char_key() {
        let k = only(&mut plain(), &char_event('q')).expect("a printable char must translate");
        assert_eq!(k.key, Key::Char('q'));
        assert!(!k.mods.shift);
    }

    #[test]
    fn navigation_keys_translate_by_id() {
        for (id, want) in [
            (sys::key::UP, Key::Up),
            (sys::key::DOWN, Key::Down),
            (sys::key::SELECT, Key::Select),
            (sys::key::SOFT_LEFT, Key::Softkey(Softkey::Left)),
            (sys::key::SOFT_RIGHT, Key::Softkey(Softkey::Right)),
        ] {
            let ev = sys::ShimEvent { kind: sys::SHIM_EV_KEY_DOWN, a: id, ..Default::default() };
            assert_eq!(only(&mut plain(), &ev).unwrap().key, want);
        }
    }

    #[test]
    fn an_unknown_key_id_survives_as_raw() {
        // Rather than being dropped. A silently discarded key is how the E72's Fn key
        // stayed invisible through two rounds of on-device debugging.
        let ev = sys::ShimEvent { kind: sys::SHIM_EV_KEY_DOWN, a: 0x4242, ..Default::default() };
        assert_eq!(only(&mut plain(), &ev).unwrap().key, Key::Raw(0x4242));
    }

    #[test]
    fn modifiers_come_from_the_portable_summary() {
        let mut ev = char_event('a');
        ev.b = sys::modifier::SHIFT | sys::modifier::FUNC;
        let k = only(&mut plain(), &ev).unwrap();
        assert!(k.mods.shift && k.mods.func && !k.mods.ctrl);
    }

    /// Ctrl+`letter` as the handset sends it: the control character, with the Ctrl bit set.
    fn ctrl_event(letter: char) -> sys::ShimEvent {
        sys::ShimEvent {
            kind: sys::SHIM_EV_KEY_DOWN,
            a: (letter as i32) - 0x60,
            b: sys::modifier::CTRL,
            d: 0x0F01,
            ..Default::default()
        }
    }

    #[test]
    fn a_ctrl_chord_arrives_as_its_letter() {
        // Before this it arrived as Key::Raw(3) — a control byte with the letter thrown away, so
        // nothing downstream could tell Ctrl+C from Ctrl+anything.
        for letter in ['c', 'v', 'x', 'a'] {
            let k = only(&mut plain(), &ctrl_event(letter)).expect("a chord must translate");
            assert_eq!(k.key, Key::Ctrl(letter));
        }
    }

    #[test]
    fn a_chord_is_not_mistaken_for_the_key_that_shares_its_code() {
        // The trap this ordering exists for: Ctrl+M is 0x0D, which is also Enter, and Ctrl+H is
        // 0x08, which is also Backspace. Read in the ordinary order, a chord in a text field
        // would submit the form or delete a character instead.
        assert_eq!(only(&mut plain(), &ctrl_event('m')).unwrap().key, Key::Ctrl('m'));
        assert_eq!(only(&mut plain(), &ctrl_event('h')).unwrap().key, Key::Ctrl('h'));
        assert_eq!(only(&mut plain(), &ctrl_event('i')).unwrap().key, Key::Ctrl('i'));
    }

    #[test]
    fn a_keyboard_that_sends_the_letter_itself_produces_the_same_chord() {
        // A Bluetooth or emulated keyboard may report Ctrl+V as `v` with the modifier set,
        // rather than as 0x16. Both are the same chord to everything downstream.
        let mut ev = char_event('V');
        ev.b = sys::modifier::CTRL;
        assert_eq!(only(&mut plain(), &ev).unwrap().key, Key::Ctrl('v'));
    }

    #[test]
    fn ctrl_with_a_key_that_is_not_a_letter_is_left_alone() {
        // Only letters are chords. An arrow with Ctrl held is still an arrow, and inventing
        // Key::Ctrl for it would take the key away from the screen that wanted it.
        let ev = sys::ShimEvent {
            kind: sys::SHIM_EV_KEY_DOWN,
            a: sys::key::DOWN,
            b: sys::modifier::CTRL,
            ..Default::default()
        };
        let k = only(&mut plain(), &ev).unwrap();
        assert_eq!(k.key, Key::Down);
        assert!(k.mods.ctrl, "the modifier still rides along for whoever wants it");
    }

    #[test]
    fn non_key_events_do_not_translate() {
        for kind in [sys::SHIM_EV_REDRAW, sys::SHIM_EV_RESIZE, sys::SHIM_EV_TIMER] {
            let ev = sys::ShimEvent { kind, ..Default::default() };
            assert!(only(&mut plain(), &ev).is_none(), "kind {kind} should not be a key");
        }
    }

    #[test]
    fn a_lone_surrogate_is_rejected_rather_than_becoming_a_replacement_char() {
        // The shim carries a UCS-2 code unit, and a surrogate half is not a scalar.
        // Turning it into U+FFFD would put a visible box in someone's message; dropping
        // it loses one keystroke of a character that cannot be typed on this keyboard
        // anyway.
        let ev = sys::ShimEvent {
            kind: sys::SHIM_EV_KEY_CHAR,
            a: 0xD800,
            d: 0x0F01,
            ..Default::default()
        };
        assert!(only(&mut plain(), &ev).is_none());
    }

    #[test]
    fn repeat_is_reported() {
        let mut ev = char_event('x');
        ev.c = 3;
        assert!(only(&mut plain(), &ev).unwrap().repeat);
    }

    /// A dead key that composes yields one key event, and the *event that armed it* yields
    /// none — which is the whole behaviour, and the reason this returns an array.
    #[test]
    fn a_pending_accent_composes_with_the_next_letter() {
        let mut kb = plain();
        // Arm the mark the way a layout row would, then feed the letter through the pump.
        assert_eq!(kb.translate_resolved(symbian_keys::TILDE, true), symbian_keys::Stroke::None);
        let out = to_key_events(&mut kb, &char_event('a'));
        assert_eq!(out[0].unwrap().key, Key::Char('ã'));
        assert!(out[1].is_none());
    }

    /// The case the array exists for: a mark and a letter that cannot carry it become two
    /// events, in order. Dropping the mark instead would make a mistyped accent vanish
    /// with no trace.
    #[test]
    fn a_mark_the_letter_cannot_take_becomes_two_events() {
        let mut kb = plain();
        kb.translate_resolved(symbian_keys::ACUTE, true);
        let out = to_key_events(&mut kb, &char_event('q'));
        assert_eq!(out[0].unwrap().key, Key::Char(symbian_keys::ACUTE));
        assert_eq!(out[1].unwrap().key, Key::Char('q'));
    }

    /// Backspace clears a pending accent; an arrow key does not.
    ///
    /// Not symmetry for its own sake: arming an accent and then moving the cursor to where
    /// you meant to type it is reasonable and the platform's own FEP allows it, whereas an
    /// accent that survives a Backspace attaches itself to whatever is typed next, which
    /// looks like the keyboard inventing characters.
    #[test]
    fn backspace_clears_a_pending_accent_and_an_arrow_does_not() {
        let named = |id| {
            sys::ShimEvent { kind: sys::SHIM_EV_KEY_DOWN, a: id, ..Default::default() }
        };

        let mut kb = plain();
        kb.translate_resolved(symbian_keys::TILDE, true);
        only(&mut kb, &named(sys::key::LEFT));
        assert_eq!(kb.pending(), Some(symbian_keys::TILDE));
        assert_eq!(only(&mut kb, &char_event('o')).unwrap().key, Key::Char('õ'));

        let mut kb = plain();
        kb.translate_resolved(symbian_keys::TILDE, true);
        only(&mut kb, &named(sys::key::BACKSPACE));
        assert_eq!(kb.pending(), None);
        assert_eq!(only(&mut kb, &char_event('o')).unwrap().key, Key::Char('o'));
    }

    #[test]
    fn screen_size_falls_back_to_the_e72_panel() {
        // On the host every shim extern is a stub returning NOT_READY, so this exercises
        // exactly the path a device takes before its surface exists.
        assert_eq!(screen_size(), Size::new(320, 240));
    }
}
