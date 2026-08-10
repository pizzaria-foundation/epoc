//! The dev bridge: this process talking to the machine on the desk.
//!
//! Live log streaming, file push and pull, and this app's own runtime telemetry, over two
//! sockets to `tools/epocadb serve`. Everything that knows `epocadb` exists is in this file.
//!
//! # One per process, so the app holds nothing
//!
//! There is one bridge and one host, so this is a module with state rather than a type an
//! app has to store, construct and thread through its own struct. An app calls
//! [`connect`] once its bearer is up and [`on_event`] from its event handler; that is the
//! whole interface, and a build without the `dev-bridge` feature compiles both to nothing.
//!
//! # It is also the log's second destination
//!
//! [`connect`] registers [`log_line`] with `symbian::log::set_sink`, so every
//! `symbian::log!` line reaches the host live as well as landing in
//! `C:\Data\logs_<app>.txt`. The app writes one kind of log line and gets both.
//!
//! Gating inside this module rather than at each call site is what keeps an app free of
//! `#[cfg]`.

#[cfg(feature = "dev-bridge")]
mod enabled {
    use alloc::string::String;

    use symbian::fs::{self, Utf16Path};
    use epocadb::{Bridge, Command};

    pub struct DevBridge {
        bridge: Option<Bridge>,
        /// Where an incoming push should land. Held across events, because a transfer
        /// spans as many as the payload needs.
        pending_push_path: Option<String>,
        /// Last time the present/step telemetry was streamed, for rate-limiting it to
        /// once a second — the same cheap-trace discipline the values themselves follow.
        last_stats_us: u64,
    }

    impl DevBridge {
        pub fn new() -> Self {
            DevBridge { bridge: None, pending_push_path: None, last_stats_us: 0 }
        }

        pub fn is_connected(&self) -> bool {
            self.bridge.is_some()
        }

        /// Open the bridge to the host named by `EPOCADB_HOST` at build time.
        ///
        /// Must not be called before a bearer is up: a socket opened on a connection
        /// that has not started panics esock rather than failing. A missing or
        /// unparseable address leaves the bridge closed and silent, which is the right
        /// answer for a build nobody is watching.
        pub fn connect(&mut self, bearer_handle: Option<i32>) {
            if self.bridge.is_some() {
                return;
            }
            let host_str = option_env!("EPOCADB_HOST").unwrap_or("");
            if host_str.is_empty() {
                return;
            }
            let Some(host) = symbian::net::Ipv4::parse(host_str) else { return };
            self.bridge = Bridge::connect(host, bearer_handle).ok();
        }

        pub fn log(&mut self, line: &str) {
            if let Some(b) = self.bridge.as_mut() {
                b.log(line);
            }
        }

        /// Drive both sockets and carry out whatever the host asked for.
        ///
        /// Returns true when the host asked the application to exit.
        pub fn on_event(&mut self, ev: &symbian_ui::RawEvent) -> bool {
            let Some(b) = self.bridge.as_mut() else { return false };

            let now = symbian::monotonic_us();
            b.on_event(ev, now);
            if !b.is_ready() {
                return false;
            }

            // Stream this app's own runtime health — how much of the blit the dirty-rect
            // present saved (`[gfx]`) and the worst `rust_step` this window (`[step]`) —
            // straight to the host. The values are published to P&S by
            // `symbian_app::entry!`; reading them back is free and does not reset anything,
            // so this never fights the publisher. Rate-limited to once a second.
            if now.wrapping_sub(self.last_stats_us) >= 1_000_000 {
                self.last_stats_us = now;
                let cat = symbian::own_uid3();
                if cat != 0 {
                    if let Ok(saved) = symbian::prop::get(cat, symbian_sys::PS_KEY_GFX) {
                        b.log(&alloc::format!("[gfx] present saved={}%", saved.clamp(0, 100)));
                    }
                    if let Ok(packed) = symbian::prop::get(cat, symbian_sys::PS_KEY_STEP) {
                        let handle_ms = (packed >> 16) & 0x7FFF;
                        let draw_ms = packed & 0xFFFF;
                        b.log(&alloc::format!("[step] handle={handle_ms}ms draw={draw_ms}ms"));
                    }
                }
            }

            // A transfer in progress owns the channel: finish it before asking for
            // more work, or the next request is read as payload.
            if self.pending_push_path.is_some() {
                if let Some(data) = b.read_data() {
                    let path = self.pending_push_path.take().unwrap_or_default();
                    write_push(b, &path, &data);
                }
                return false;
            }

            match b.poll(now) {
                Some(Command::Quit) => return true,
                // Install is a push to a public path the user then opens in File
                // Manager. Writing to the platform's own import directory needs
                // capabilities this bridge does not hold — see docs/spec-epocadb.md.
                Some(Command::Push { path, .. }) | Some(Command::Install { path, .. }) => {
                    b.expect_data_header();
                    self.pending_push_path = Some(path);
                }
                Some(Command::Pull { path }) => serve_pull(b, &path),
                // `CTL <line>`: application-defined, forwarded verbatim by the bridge
                // because epocadb deliberately does not know anyone's verbs. This client
                // defines none, so the honest answer is to say so rather than to drop the
                // line — a host tool that sent one would otherwise wait for a reply that
                // never comes, with the bridge looking hung.
                Some(Command::Control(_)) => b.reply("ERR no control verbs"),
                Some(Command::None) | None => {}
            }
            false
        }
    }

    impl Default for DevBridge {
        fn default() -> Self {
            Self::new()
        }
    }

    fn write_push(b: &mut Bridge, path: &str, data: &[u8]) {
        let p = match Utf16Path::new(path) {
            Ok(p) => p,
            Err(_) => {
                b.reply("ERR bad path");
                return;
            }
        };
        match fs::write_atomic(&mut symbian::ShimFs, &p, data) {
            Ok(()) => b.reply(&alloc::format!("OK wrote {} bytes", data.len())),
            Err(e) => b.reply(&alloc::format!("ERR {e:?}")),
        }
    }

    fn serve_pull(b: &mut Bridge, path: &str) {
        let p = match Utf16Path::new(path) {
            Ok(p) => p,
            Err(_) => {
                b.reply("ERR bad path");
                return;
            }
        };
        match fs::read(&mut symbian::ShimFs, &p) {
            Ok(Some(data)) => {
                // Status line first, then the payload — the host reads them in that
                // order and the queue preserves it.
                b.reply(&alloc::format!("OK {}", data.len()));
                if b.send_data(&data).is_err() {
                    b.log("epocadb: pull payload did not fit the outbound queue");
                }
            }
            Ok(None) => b.reply("ERR not found"),
            Err(e) => b.reply(&alloc::format!("ERR {e:?}")),
        }
    }
}

#[cfg(not(feature = "dev-bridge"))]
mod disabled {
    /// The bridge, compiled out. Every method is a no-op the optimiser removes.
    pub struct DevBridge;

    impl DevBridge {
        pub fn new() -> Self {
            DevBridge
        }
        pub fn is_connected(&self) -> bool {
            false
        }
        pub fn connect(&mut self, _bearer_handle: Option<i32>) {}
        pub fn log(&mut self, _line: &str) {}
        pub fn on_event(&mut self, _ev: &symbian_ui::RawEvent) -> bool {
            false
        }
    }

    impl Default for DevBridge {
        fn default() -> Self {
            Self::new()
        }
    }
}

#[cfg(feature = "dev-bridge")]
pub use enabled::DevBridge;
#[cfg(not(feature = "dev-bridge"))]
pub use disabled::DevBridge;

// ------------------------------------------------------------------ the singleton --

/// The one bridge. `static mut` and not a lock: this runs on the GUI thread (or the
/// daemon's single active scheduler), the same assumption the telemetry statics in this
/// crate already make, and the target has no atomics to build a lock out of.
static mut BRIDGE: Option<DevBridge> = None;

/// Borrow the bridge, creating it on first use.
///
/// SAFETY, once, for every caller below: single-threaded. Reached through `addr_of_mut!`
/// rather than by naming the static, because a `&mut` on a `static mut` is a warning today
/// and an error in edition 2024.
fn with<R>(f: impl FnOnce(&mut DevBridge) -> R) -> R {
    let slot = unsafe { &mut *core::ptr::addr_of_mut!(BRIDGE) };
    f(slot.get_or_insert_with(DevBridge::new))
}

/// Open the bridge to the host named by `EPOCADB_HOST` at build time, and route
/// `symbian::log!` through it as well as to the file.
///
/// Must not be called before a bearer is up: a socket opened on a connection that has not
/// started panics esock rather than failing. Calling it again once connected does nothing,
/// so an app may simply call it whenever its bearer comes up.
pub fn connect(bearer_handle: Option<i32>) {
    with(|b| b.connect(bearer_handle));
    symbian::log::set_sink(log_line);
}

/// Whether the bridge is open. An app uses this to decide whether to call [`connect`].
pub fn is_connected() -> bool {
    with(|b| b.is_connected())
}

/// Stream one line to the host, if the bridge is open. Registered as the log sink by
/// [`connect`]; there is rarely a reason to call it directly, since `symbian::log!` reaches
/// both destinations.
pub fn log_line(line: &str) {
    with(|b| b.log(line));
}

/// Drive both sockets and carry out whatever the host asked for.
///
/// Returns true when the host asked the application to exit. Call it from the app's raw
/// event handler; the bridge's sockets are its own and independent of the app's.
pub fn on_event(ev: &symbian_ui::RawEvent) -> bool {
    with(|b| b.on_event(ev))
}
