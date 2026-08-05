//! Runs any [`symbian_ui::App`] in a window, driven by your keyboard.
//!
//! ```ignore
//! // examples/sim.rs in your app crate
//! fn main() {
//!     symbian_sim::run(MyApp::new()).unwrap();
//! }
//! ```
//!
//! Then `cargo run --example sim`. Declare this crate under `[dev-dependencies]` so the
//! device build never sees it — it pulls in a windowing library, which has no business
//! anywhere near a `no_std` staticlib.
//!
//! # Why this and not the phone
//!
//! A device round trip is: build, package, push over Bluetooth, accept a prompt, open
//! Messaging, install, launch. Call it two minutes, and it fails outright whenever the
//! phone's Bluetooth has gone to sleep. That is a fine loop for *confirming* something
//! and a terrible one for designing.
//!
//! This runs the same `tg::App`, the same `symbian_ui` widgets and the same
//! `symbian_gfx` rasterizer against the same 320x240 canvas, so what appears here is
//! what the device draws — the only thing the device adds is the RGB565 → XRGB8888
//! expansion at present time, which this does too, through the same function.
//!
//! # What it deliberately does not simulate
//!
//! Timing. `rust_step` on the device runs from a `CIdle` at idle priority on a 600 MHz
//! ARM1136 with soft float; here it runs at 60 fps on a desktop. So this tool will never
//! tell you that a repaint is too slow. It is a tool for *what the app looks like and
//! does*, and the moment a question is about speed it has to go back to the phone.

use minifb::{Key as MKey, KeyRepeat, Window, WindowOptions};
use symbian_gfx::{BitmapFont, Canvas, Rect, Size, E72_SCREEN};
use symbian_ui::{App, Fonts, Handled, Key, KeyEvent, Modifiers, Palette, Softkey, Theme};

/// 3x. At 1x a 320x240 window is a postage stamp on a modern display and every judgement
/// about legibility comes out wrong; at 3x the pixel grid is still visibly a grid, which
/// is the point — this is a 2009 QVGA screen and it should look like one.
const SCALE: usize = 3;

/// Where to look for the font atlases.
///
/// Searched in order rather than fixed, because the working directory depends on how
/// cargo was invoked — from the SDK root for the workspace, from the app's own directory
/// for a standalone project. Guessing one and failing with "no such file" would send
/// people looking for a missing asset rather than a wrong cwd.
const ATLAS_DIRS: &[&str] = &[
    "crates/symbian-ui/assets",
    "../../crates/symbian-ui/assets",
    "../crates/symbian-ui/assets",
    "assets",
];

fn load(name: &str) -> Result<Vec<u8>, String> {
    let mut tried = String::new();
    for dir in ATLAS_DIRS {
        let p = format!("{dir}/{name}.sbf");
        if let Ok(v) = std::fs::read(&p) {
            return Ok(v);
        }
        tried.push_str("\n  ");
        tried.push_str(&p);
    }
    Err(format!(
        "could not find {name}.sbf. Looked in:{tried}\n\
         Run tools/mkfonts.sh, or set SYMBIAN_ASSETS to the directory holding them."
    ))
}

fn load_all(name: &str) -> Result<Vec<u8>, String> {
    if let Ok(dir) = std::env::var("SYMBIAN_ASSETS") {
        let p = format!("{dir}/{name}.sbf");
        return std::fs::read(&p).map_err(|e| format!("{p}: {e}"));
    }
    load(name)
}

/// The device's key layout, mapped onto a desktop keyboard.
///
/// The D-pad and softkeys have no desktop equivalent, so they get keys nobody types:
/// arrows and F1/F2/F3. Everything else is the character stream, which on the device
/// arrives already translated by the window server — so taking minifb's characters
/// directly is not a shortcut, it is the same contract.
fn nav_key(k: MKey) -> Option<Key> {
    Some(match k {
        MKey::Up => Key::Up,
        MKey::Down => Key::Down,
        MKey::Left => Key::Left,
        MKey::Right => Key::Right,
        MKey::Enter => Key::Select,
        // The E72's centre select is a press of the D-pad, which is a different physical
        // action from Enter. Both map here because a desktop has no D-pad to press.
        MKey::Space => Key::Select,
        MKey::F1 => Key::Softkey(Softkey::Left),
        MKey::F2 => Key::Softkey(Softkey::Middle),
        MKey::F3 => Key::Softkey(Softkey::Right),
        // Escape is the right softkey too: on the device that is Back, and Escape is
        // what a desktop user's hand reaches for.
        MKey::Escape => Key::Softkey(Softkey::Right),
        MKey::Backspace => Key::Backspace,
        MKey::Delete => Key::Delete,
        _ => return None,
    })
}

struct Sim<A: App> {
    app: A,
    /// Index into `Palette::ALL`, cycled with Tab so a theme can be judged against the
    /// real UI rather than against a swatch sheet.
    palette: usize,
    /// Frame buffer in RGB565, exactly as on the device.
    canvas: Vec<u16>,
    /// The expanded frame at 1:1, produced by the same conversion the shim uses.
    xrgb: Vec<u32>,
    /// The window's buffer: `xrgb` at SCALE, nearest-neighbour.
    out: Vec<u32>,
    dirty: bool,
}

impl<A: App> Sim<A> {
    fn new(app: A) -> Self {
        let n = (E72_SCREEN.w * E72_SCREEN.h) as usize;
        Sim {
            app,
            palette: 0,
            canvas: vec![0u16; n],
            xrgb: vec![0u32; n],
            out: vec![0u32; n * SCALE * SCALE],
            dirty: true,
        }
    }

    fn theme<'a>(&self, fonts: Fonts<'a>) -> Theme<'a> {
        Theme::new(Palette::ALL[self.palette].1, fonts)
    }

    /// Draw into the RGB565 canvas, then expand into the window buffer.
    fn render(&mut self, fonts: Fonts<'_>) {
        let theme = self.theme(fonts);
        {
            let mut c = Canvas::from_slice(&mut self.canvas, E72_SCREEN);
            self.app.draw(&mut c, &theme);
        }

        let (w, h) = (E72_SCREEN.w as usize, E72_SCREEN.h as usize);

        // The same expansion the device does, through the same function — so a colour
        // that comes out wrong here comes out wrong there, which is the whole point of
        // not reimplementing it. Notably it replicates each channel's high bits into the
        // low ones, so white is 0xFFFFFF and not 0xF8F8F8; a hand-rolled shift here
        // would make the simulator subtly brighter than the phone.
        symbian_gfx::present::rgb565_to_xrgb8888(&mut self.xrgb, w, &self.canvas, w, w, h);

        // Nearest-neighbour, never interpolated: a smoothed 3x view would hide exactly
        // the single-pixel errors this tool exists to surface.
        for y in 0..h {
            for x in 0..w {
                let px = self.xrgb[y * w + x];
                let base = y * SCALE * w * SCALE + x * SCALE;
                for dy in 0..SCALE {
                    let row = base + dy * w * SCALE;
                    self.out[row..row + SCALE].fill(px);
                }
            }
        }
    }
}

/// Open a window and run `app` until it asks to close or the window is closed.
///
/// Blocks. Returns `Err` only for setup failures — a missing atlas or a window that
/// could not be created — because once the loop is running there is nothing left that
/// can fail in a way the caller could act on.
pub fn run<A: App>(app: A) -> Result<(), String> {
    let (d11, d11b, d9) = (load_all("ui11")?, load_all("ui11b")?, load_all("ui9")?);
    let f11 = BitmapFont::new(&d11).map_err(|e| format!("ui11.sbf: {e:?}"))?;
    let f11b = BitmapFont::new(&d11b).map_err(|e| format!("ui11b.sbf: {e:?}"))?;
    let f9 = BitmapFont::new(&d9).map_err(|e| format!("ui9.sbf: {e:?}"))?;
    // The device links exactly these three and reuses the bold one for titles, so the
    // simulator does too. Using nicer fonts here would flatter the result.
    let fonts = Fonts { body: &f11, strong: &f11b, small: &f9, title: &f11b };

    let mut sim = Sim::new(app);

    let mut window = Window::new(
        "Nokia E72 — 320x240",
        E72_SCREEN.w as usize * SCALE,
        E72_SCREEN.h as usize * SCALE,
        WindowOptions::default(),
    )
    .map_err(|e| format!("could not open a window: {e}"))?;

    // 60 fps is a display refresh limit, not a simulation of the device's frame rate —
    // see the note at the top about what this tool does not tell you.
    window.set_target_fps(60);

    println!(
        "\
keys
  arrows        D-pad
  Enter/Space   select
  F1 F2 F3      left / middle / right softkey
  Esc           right softkey (Back)
  Tab           next theme
  letters       typed into the composer
  Ctrl+S        write sim-frame.png
  Ctrl+Q        quit"
    );

    let mut last_title = String::new();
    while window.is_open() {
        let screen = Rect::from_size(Size::new(E72_SCREEN.w, E72_SCREEN.h));
        let theme = sim.theme(fonts);

        let ctrl = window.is_key_down(MKey::LeftCtrl) || window.is_key_down(MKey::RightCtrl);
        let shift = window.is_key_down(MKey::LeftShift) || window.is_key_down(MKey::RightShift);
        let mods = Modifiers { shift, ctrl, func: false };

        if ctrl && window.is_key_pressed(MKey::Q, KeyRepeat::No) {
            break;
        }
        if ctrl && window.is_key_pressed(MKey::S, KeyRepeat::No) {
            save_png(&sim.canvas);
        }
        if window.is_key_pressed(MKey::Tab, KeyRepeat::No) {
            sim.palette = (sim.palette + 1) % Palette::ALL.len();
            sim.dirty = true;
        }

        // Navigation first, then text. Both go through the same `handle_key` the device
        // calls, so a key that does the wrong thing here does the wrong thing there.
        for k in window.get_keys_pressed(KeyRepeat::Yes) {
            if let Some(key) = nav_key(k) {
                let ev = KeyEvent { key, mods, repeat: false };
                if sim.app.handle_key(ev, &theme, screen) == Handled::Consumed {
                    sim.dirty = true;
                }
            }
        }
        // minifb hands over already-translated characters, which is the same stream the
        // window server produces on the device — including the shift layer.
        if !ctrl {
            let typed: Vec<char> = window
                .get_keys_pressed(KeyRepeat::Yes)
                .iter()
                .filter_map(|k| ascii_of(*k, shift))
                .collect();
            for ch in typed {
                let ev = KeyEvent { key: Key::Char(ch), mods, repeat: false };
                if sim.app.handle_key(ev, &theme, screen) == Handled::Consumed {
                    sim.dirty = true;
                }
            }
        }

        if sim.app.should_exit() {
            break;
        }

        // The window title carries the current theme, which is otherwise invisible when
        // two palettes are close.
        let title = format!(
            "{} — E72 320x240 — {} ({}/{})",
            sim.app.title(),
            Palette::ALL[sim.palette].0,
            sim.palette + 1,
            Palette::ALL.len()
        );
        if title != last_title {
            window.set_title(&title);
            last_title = title;
        }

        if sim.dirty {
            sim.render(fonts);
            sim.dirty = false;
        }
        window
            .update_with_buffer(
                &sim.out,
                E72_SCREEN.w as usize * SCALE,
                E72_SCREEN.h as usize * SCALE,
            )
            .map_err(|e| format!("window update failed: {e}"))?;
    }
    Ok(())
}

/// minifb reports physical keys, not characters, so the shift layer is applied here.
///
/// Only the characters a chat needs: letters, digits, space and a little punctuation.
/// This is the one place the simulator genuinely differs from the device, where the
/// window server has a full keymap — so an input bug that depends on an unusual
/// character has to be reproduced on the phone.
fn ascii_of(k: MKey, shift: bool) -> Option<char> {
    let base = match k {
        MKey::A => 'a', MKey::B => 'b', MKey::C => 'c', MKey::D => 'd',
        MKey::E => 'e', MKey::F => 'f', MKey::G => 'g', MKey::H => 'h',
        MKey::I => 'i', MKey::J => 'j', MKey::K => 'k', MKey::L => 'l',
        MKey::M => 'm', MKey::N => 'n', MKey::O => 'o', MKey::P => 'p',
        MKey::Q => 'q', MKey::R => 'r', MKey::S => 's', MKey::T => 't',
        MKey::U => 'u', MKey::V => 'v', MKey::W => 'w', MKey::X => 'x',
        MKey::Y => 'y', MKey::Z => 'z',
        MKey::Key0 => '0', MKey::Key1 => '1', MKey::Key2 => '2', MKey::Key3 => '3',
        MKey::Key4 => '4', MKey::Key5 => '5', MKey::Key6 => '6', MKey::Key7 => '7',
        MKey::Key8 => '8', MKey::Key9 => '9',
        MKey::Comma => ',', MKey::Period => '.', MKey::Slash => '/',
        MKey::Apostrophe => '\'', MKey::Minus => '-',
        // Space is Select in nav_key, so it cannot also be a space character. On the
        // device those are different physical keys; here Select wins, because navigating
        // is what you do more of.
        _ => return None,
    };
    Some(if shift { base.to_ascii_uppercase() } else { base })
}

/// Write the current frame next to the binary, at 1:1.
///
/// 1:1 and not 3x: a screenshot is for looking at pixels, and the scaling is a property
/// of the window rather than of the frame.
fn save_png(canvas: &[u16]) {
    let (w, h) = (E72_SCREEN.w as usize, E72_SCREEN.h as usize);
    let path = "sim-frame.png";
    match write_png(path, canvas, w, h) {
        Ok(()) => println!("wrote {path}"),
        Err(e) => eprintln!("{path}: {e}"),
    }
}

/// A minimal PNG writer: stored deflate blocks, so there is no compression dependency.
///
/// Duplicated from tools/preview rather than shared, because a shared crate for eighty
/// lines of no-op deflate would be a third crate in the workspace to justify.
fn write_png(path: &str, canvas: &[u16], w: usize, h: usize) -> std::io::Result<()> {
    use std::io::Write;

    fn crc32(data: &[u8]) -> u32 {
        let mut table = [0u32; 256];
        for (i, e) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
            }
            *e = c;
        }
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c = table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
        c ^ 0xFFFF_FFFF
    }

    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let mut with_kind = kind.to_vec();
        with_kind.extend_from_slice(body);
        out.extend_from_slice(&with_kind);
        out.extend_from_slice(&crc32(&with_kind).to_be_bytes());
    }

    // Expand through the shim's own converter first, so the file matches the screen.
    let mut xrgb = vec![0u32; w * h];
    symbian_gfx::present::rgb565_to_xrgb8888(&mut xrgb, w, canvas, w, w, h);

    // Raw scanlines: a filter byte then RGB triples.
    let mut raw = Vec::with_capacity(h * (1 + w * 3));
    for y in 0..h {
        raw.push(0);
        for x in 0..w {
            let px = xrgb[y * w + x];
            raw.push((px >> 16) as u8);
            raw.push((px >> 8) as u8);
            raw.push(px as u8);
        }
    }

    // zlib with stored blocks: adler32 over the data, then 65535-byte literal runs.
    let mut z = vec![0x78, 0x01];
    for (i, part) in raw.chunks(65535).enumerate() {
        let last = (i + 1) * 65535 >= raw.len();
        z.push(if last { 1 } else { 0 });
        z.extend_from_slice(&(part.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(part.len() as u16)).to_le_bytes());
        z.extend_from_slice(part);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &raw {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, truecolour
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &z);
    chunk(&mut png, b"IEND", &[]);

    std::fs::File::create(path)?.write_all(&png)
}
