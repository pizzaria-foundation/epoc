//! tlsprobe — one HTTPS GET, then exit. Proves the vendored mbedTLS handshakes with a modern
//! server on the E72. Result goes to C:\Data\tlsprobe.txt (readable over ADBian).
#![no_std]
#![no_main]

extern crate alloc;
use symbian_sys;
use alloc::format;
use symbian::fs::{self, ShimFs, Utf16Path};

struct Probe {
    done: bool,
}

impl Probe {
    fn new() -> Self {
        // Bring-up waits for the first timer tick, like the devdump probes.
        let _ = symbian::timer_after(1);
        Probe { done: false }
    }

    fn run(&mut self) {
        let line = match symbian::tls::https_get("www.google.com", 443, "/", 4096) {
            Ok(resp) => {
                // The status line + how many bytes came back — enough to see a real HTTPS reply.
                let head = core::str::from_utf8(&resp).unwrap_or("<non-utf8>");
                let status = head.lines().next().unwrap_or("<no status line>");
                format!("OK {} bytes\r\n{}\r\n", resp.len(), status)
            }
            Err(e) => format!("FAIL {:?}\r\n", e),
        };
        let mut d = ShimFs;
        let _ = fs::Fs::mkdir(&mut d, &Utf16Path::new("C:\\Data\\").unwrap().as_units());
        if let Ok(p) = Utf16Path::new("C:\\Data\\tlsprobe.txt") {
            let _ = fs::write_atomic(&mut d, &p, line.as_bytes());
        }
        symbian::log!("[tlsprobe] {}", line.trim_end());
    }
}

impl symbian_app::DaemonApp for Probe {
    fn handle_raw(&mut self, ev: &symbian_sys::ShimEvent) {
        if self.done || ev.kind != symbian_sys::SHIM_EV_TIMER {
            return;
        }
        // Set done BEFORE run(). run() blocks inside a nested CActiveSchedulerWait for the whole
        // HTTPS call, during which the daemon's 200 ms pump keeps firing timer events; without this
        // guard the pump re-enters run() (a second SetActive on a live active object) and panics.
        self.done = true;
        self.run();
    }
    fn should_exit(&self) -> bool {
        self.done
    }
}

symbian_app::daemon_entry!(Probe::new());
