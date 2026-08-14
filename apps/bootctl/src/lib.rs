//! Boot manager — the editor for what `apps/bootd` does at boot.
//!
//! All the screen logic is `symbian_bootctl::BootScreen`, which is pure and host-tested. This file
//! is the device half: read the roster and the two files on the way in, write the config on the way
//! out. Nothing here launches, stops, or signals anything — an edit takes effect at the next boot,
//! which is the whole contract and the reason a mistake costs a reboot instead of a running phone.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use symbian::apps;
use symbian::fs::{self, Fs, ShimFs, Utf16Path};
use symbian_bootcfg::{BootConfig, BootStatus, CONFIG_PATH, COUNT_PATH, DATA_DIR};
use symbian_bootctl::BootScreen;
use symbian_ui::{App, Canvas, Handled, KeyEvent, Rect, Theme};

pub struct BootCtl {
    screen: BootScreen,
    fs: ShimFs,
    /// Set by any edit, cleared once written. The config is written on the way out rather than on
    /// every keystroke: bootd holds the file at boot, and one write beats twenty.
    dirty: bool,
    saved: bool,
}

impl BootCtl {
    pub fn new() -> Self {
        let mut fsx = ShimFs;
        if let Ok(dir) = Utf16Path::new(DATA_DIR) {
            let _ = fsx.mkdir(dir.as_units());
        }

        let cfg = read_bytes(&mut fsx, CONFIG_PATH)
            .and_then(|b| match BootConfig::decode(&b) {
                Ok(c) => Some(c),
                Err(e) => {
                    // Do not silently start from blank on top of a file we could not read: say so,
                    // and let the user rebuild the list knowing the old one was refused.
                    symbian::log!("[bootctl] config refused: {e:?}");
                    None
                }
            })
            .unwrap_or_default();

        let status = read_bytes(&mut fsx, symbian_bootcfg::STATUS_PATH)
            .and_then(|b| BootStatus::decode(&b).ok());

        let roster = apps::installed().unwrap_or_default();
        symbian::log!(
            "[bootctl] open entries={} roster={} status={}",
            cfg.entries.len(),
            roster.len(),
            status.is_some()
        );

        Self { screen: BootScreen::new(cfg, status, roster), fs: fsx, dirty: false, saved: false }
    }

    fn persist(&mut self) {
        let Ok(p) = Utf16Path::new(CONFIG_PATH) else { return };
        let bytes = self.screen.config().encode();
        match fs::write_atomic(&mut self.fs, &p, &bytes) {
            Ok(()) => symbian::log!("[bootctl] saved {} bytes", bytes.len()),
            Err(e) => symbian::log!("[bootctl] save err={e:?}"),
        }
    }

    /// Clear the unsettled-boot counter, which is what takes bootd out of safe mode. Deliberately
    /// an explicit action: safe mode exists because three boots in a row went wrong, and it should
    /// take a person to say that has been dealt with.
    fn reset_counter(&mut self) {
        let Ok(p) = Utf16Path::new(COUNT_PATH) else { return };
        let _ = fs::write_atomic(&mut self.fs, &p, &[0]);
        symbian::log!("[bootctl] safe-mode counter cleared");
    }
}

fn read_bytes(fs_: &mut ShimFs, path: &str) -> Option<alloc::vec::Vec<u8>> {
    let p = Utf16Path::new(path).ok()?;
    fs::read(fs_, &p).ok().flatten()
}

impl Default for BootCtl {
    fn default() -> Self {
        Self::new()
    }
}

impl App for BootCtl {
    fn title(&self) -> &str {
        "Boot manager"
    }

    fn handle_key(&mut self, ev: KeyEvent, theme: &Theme<'_>, screen: Rect) -> Handled {
        let handled = self.screen.handle_key(ev, theme, screen);
        self.dirty |= self.screen.take_changed();
        if self.screen.take_reset() {
            self.reset_counter();
        }
        if self.screen.back() && !self.saved {
            self.saved = true;
            if self.dirty {
                self.persist();
            }
        }
        handled
    }

    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        self.screen.draw(c, theme);
    }

    fn should_exit(&self) -> bool {
        self.screen.back()
    }
}
