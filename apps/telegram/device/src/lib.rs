//! The device build of the Telegram client.
//!
//! Everything that used to be here — the allocator, the panic handler, the event
//! translation, the framebuffer setup, the three `extern "C"` entry points — is in
//! `symbian_app::entry!`. What is left is the app and the theme it wants.
//!
//! See `crates/symbian-app` for why the lang items have to be expanded into this crate
//! rather than provided by a library, and `apps/telegram/README.md` for why the app is
//! split into an rlib and this staticlib.

#![no_std]
#![no_main]

extern crate alloc;

/// The worker thread's entry point.
///
/// Runs on a second thread with its own heap, so nothing it allocates may outlive the job —
/// see `symbian::work`. Its only caller is `shim_work_submit`, and its only job today is the
/// 2048-bit modular exponentiation the MTProto handshake needs twice.
fn worker(opcode: i32, input: &[u8], out: &mut [u8]) -> i32 {
    tg::link::work(opcode, input, out)
}

/// Keeps the chat parser and its schema table in the ARM link.
///
/// `--gc-sections` sweeps anything unreferenced, and the app still draws `Store::mock()` --
/// so without this the 532-constructor table costs nothing and measures nothing. Referencing
/// it here is what makes the image size in the commit message a real number.
#[used]
static _CHATS_LINK: fn(&[u8]) -> Result<tg_proto::chats::Dialogs, tg_proto::walk::Error> =
    tg_proto::chats::parse_dialogs;

/// `App::login()` rather than `App::mock()`.
///
/// It opens the connection as it is constructed — attaching to whatever is already up takes
/// 263 ms on this handset — so the handshake is running before anyone finishes typing a
/// phone number. `mock()` still exists for the preview and the tests, which draw the same
/// screens with nothing behind them.
symbian_app::entry!(tg::App::login(), work = worker);
