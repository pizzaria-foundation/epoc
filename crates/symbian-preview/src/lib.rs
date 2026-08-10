//! Render a screen to a PNG on the host, so it can be looked at without a device.
//!
//! The unit tests prove the properties a machine can check — containment, symmetry,
//! clamping. They cannot tell you whether a 9-pixel bell reads as a bell, or whether a bevel
//! is visible at all. That is what a contact sheet is for, and this crate is the machinery
//! behind one: a pixel buffer the size of the handset's screen, the fonts loaded the way the
//! device chains them, and a PNG writer with no dependencies.
//!
//! What it deliberately does **not** contain is any particular screen. The SDK's own sheets
//! live in `tools/preview`; an application's live with the application, so its scenes travel
//! with the code they document.
//!
//! ```ignore
//! let atlases = Atlases::load(&sdk_root())?;
//! atlases.with_themes(|dark, _light| {
//!     let mut sheet = Sheet::new(E72_SCREEN);
//!     my_screen.draw(&mut sheet.canvas(), dark);
//!     sheet.save("out", "10-my-screen");
//! });
//! ```
//!
//! Host-only: it uses `std`, writes files, and never reaches the device build. Same standing
//! as `symbian-sim` — a crate apps pull in under `[dev-dependencies]`.

mod png;

use std::path::{Path, PathBuf};

use symbian_gfx::{BitmapFont, Canvas, Size, WithFallback};

/// How much each screenshot is magnified on the way out.
///
/// 2x, because a 320x240 PNG at 1:1 on a modern display is about the size of a postage stamp
/// and the whole point is to look at it. The magnification is nearest-neighbour, so a pixel
/// stays a pixel and nothing is invented.
pub const SCALE: usize = 2;

/// A screen-sized RGB565 buffer that can draw itself into a file.
pub struct Sheet {
    buf: Vec<u16>,
    size: Size,
}

impl Sheet {
    pub fn new(size: Size) -> Self {
        Self { buf: vec![0u16; (size.w * size.h) as usize], size }
    }

    pub fn canvas(&mut self) -> Canvas<'_> {
        Canvas::from_slice(&mut self.buf, self.size)
    }

    /// The raw pixels, for a sheet that magnifies part of another one.
    pub fn pixels(&self) -> &[u16] {
        &self.buf
    }

    pub fn size(&self) -> Size {
        self.size
    }

    /// Write `<dir>/<name>.png`, creating `dir` if needed, and print the path.
    ///
    /// Panics on an I/O error rather than returning one: this runs in a developer's terminal
    /// on purpose, and a preview that silently fails to write is worse than one that stops.
    pub fn save(&self, dir: impl AsRef<Path>, name: &str) {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{name}.png"));
        png::write_rgb565(
            path.to_str().expect("output path is not UTF-8"),
            &self.buf,
            self.size.w as usize,
            self.size.h as usize,
            self.size.w as usize,
            SCALE,
        )
        .unwrap();
        println!("{}  ({}x{} @{SCALE}x)", path.display(), self.size.w, self.size.h);
    }
}

/// The font atlases, loaded from the SDK's `crates/symbian-ui/assets/`.
///
/// Owns the bytes; the `BitmapFont`s that borrow them are built inside [`Atlases::with_fonts`]
/// rather than stored, which keeps this a plain struct instead of a self-referential one.
pub struct Atlases {
    body: Vec<u8>,
    strong: Vec<u8>,
    small: Vec<u8>,
    title: Vec<u8>,
    emoji: Vec<u8>,
}

impl Atlases {
    /// Load the preview set: the 12px pair, 10px small, 13px bold title, and the emoji atlas.
    ///
    /// Larger than the device's own ui11/ui9 set on purpose — a sheet is looked at on a
    /// desktop — but chained the same way, so what the fallback does here is what it does on
    /// the phone.
    ///
    /// `sdk_root` is the directory holding `crates/`. See [`sdk_root`].
    pub fn load(sdk_root: &Path) -> Self {
        let dir = sdk_root.join("crates/symbian-ui/assets");
        let one = |name: &str| {
            let p = dir.join(format!("{name}.sbf"));
            std::fs::read(&p)
                .unwrap_or_else(|e| panic!("{}: {e} (run tools/mkfont.py first)", p.display()))
        };
        Self {
            body: one("ui12"),
            strong: one("ui12b"),
            small: one("ui10"),
            title: one("ui13b"),
            emoji: one("uiemoji12"),
        }
    }

    /// Build the fonts and hand them to `f`.
    ///
    /// Each text font is chained behind the emoji atlas exactly as the device chains
    /// ui11/ui11b, so a display name with an emoji in it looks here the way it looks on the
    /// handset — the bug that motivated the chaining was a hole in a label, and a preview
    /// that did not reproduce it was worth nothing.
    pub fn with_fonts<R>(&self, f: impl FnOnce(symbian_ui::Fonts<'_>) -> R) -> R {
        let emoji = BitmapFont::new(&self.emoji).unwrap();
        let body = WithFallback::new(BitmapFont::new(&self.body).unwrap(), emoji);
        let strong = WithFallback::new(BitmapFont::new(&self.strong).unwrap(), emoji);
        let title = WithFallback::new(BitmapFont::new(&self.title).unwrap(), emoji);
        let small = BitmapFont::new(&self.small).unwrap();
        f(symbian_ui::Fonts { body: &body, strong: &strong, small: &small, title: &title })
    }

    /// Build both themes over those fonts and hand them to `f`.
    ///
    /// Both, because a palette bug shows up as a contrast that only fails in one of them —
    /// which is why every sheet that matters is rendered twice.
    pub fn with_themes<R>(
        &self,
        f: impl FnOnce(&symbian_ui::Theme<'_>, &symbian_ui::Theme<'_>) -> R,
    ) -> R {
        self.with_fonts(|fonts| {
            let dark = symbian_ui::Theme::dark(fonts);
            let light = symbian_ui::Theme::light(fonts);
            f(&dark, &light)
        })
    }
}

/// The SDK checkout root, found by walking up from the current directory until `crates/` is
/// there.
///
/// Not `CARGO_MANIFEST_DIR`: a preview may be run from an application's own repository, where
/// the SDK is a path dependency somewhere else entirely, and the atlases still have to be
/// found. Falls back to the current directory, which is what `cargo run` from a checkout
/// gives.
pub fn sdk_root() -> PathBuf {
    if let Ok(explicit) = std::env::var("EPOC_SDK") {
        return PathBuf::from(explicit);
    }
    let mut dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    loop {
        if dir.join("crates/symbian-ui/assets").is_dir() {
            return dir;
        }
        if !dir.pop() {
            return std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        }
    }
}

/// Copy `src` into `dst` at `at`, magnified `zoom` times.
///
/// Each source pixel becomes a `zoom-1` block with a one-pixel gutter in `grid`, so pixels
/// stay countable *without* the grid overwriting them. Drawing the grid on top instead — the
/// obvious way — clips a pixel off every block and turns three solid rules into what looks
/// like five stripes, which is exactly the kind of lie that makes a contact sheet worse than
/// no sheet.
pub fn blit_zoom(
    c: &mut Canvas,
    at: symbian_gfx::Point,
    src: &[u16],
    src_size: Size,
    zoom: i32,
    grid: symbian_gfx::Color,
) {
    use symbian_gfx::Rect;
    let block = (zoom - 1).max(1);
    c.fill_rect(Rect::from_xywh(at.x, at.y, src_size.w * zoom + 1, src_size.h * zoom + 1), grid);
    for y in 0..src_size.h {
        for x in 0..src_size.w {
            let color = symbian_gfx::Rgb565(src[(y * src_size.w + x) as usize]).to_color();
            c.fill_rect(
                Rect::from_xywh(at.x + x * zoom + 1, at.y + y * zoom + 1, block, block),
                color,
            );
        }
    }
}
