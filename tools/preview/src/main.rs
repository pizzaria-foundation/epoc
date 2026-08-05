//! Renders SDK screens to PNG so they can be inspected without a device.
//!
//! `cargo run -p preview` writes into `preview-out/`.

mod png;

use symbian_gfx::{Align, BitmapFont, Canvas, Color, Font, Point, Rect, Size, E72_SCREEN};

const SCALE: usize = 2;

struct Screen {
    buf: Vec<u16>,
    size: Size,
}

impl Screen {
    fn new(size: Size) -> Self {
        Self { buf: vec![0u16; (size.w * size.h) as usize], size }
    }

    fn canvas(&mut self) -> Canvas<'_> {
        Canvas::from_slice(&mut self.buf, self.size)
    }

    fn save(&self, name: &str) {
        std::fs::create_dir_all("preview-out").unwrap();
        let path = format!("preview-out/{name}.png");
        png::write_rgb565(
            &path,
            &self.buf,
            self.size.w as usize,
            self.size.h as usize,
            self.size.w as usize,
            SCALE,
        )
        .unwrap();
        println!("{path}  ({}x{} @{SCALE}x)", self.size.w, self.size.h);
    }
}

fn load(name: &str) -> Vec<u8> {
    let p = format!("crates/symbian-ui/assets/{name}.sbf");
    std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e} (run tools/mkfont.py first)"))
}

fn main() {
    let (d12, d12b, d10, d13b) = (load("ui12"), load("ui12b"), load("ui10"), load("ui13b"));
    let f12 = BitmapFont::new(&d12).unwrap();
    let f12b = BitmapFont::new(&d12b).unwrap();
    let f10 = BitmapFont::new(&d10).unwrap();
    let f13b = BitmapFont::new(&d13b).unwrap();

    println!("fonts: ui12={:?}", f12);

    // ---- a smoke sheet that exercises the rasterizer and the atlases ----
    let mut s = Screen::new(E72_SCREEN);
    {
        let mut c = s.canvas();
        c.clear(Color::hex(0x101418));

        // Colour ramps: check RGB565 quantisation and the blend path.
        for i in 0..320 {
            let t = (i * 255 / 319) as u8;
            c.fill_rect(Rect::from_xywh(i, 0, 1, 8), Color::rgb(t, 0, 0));
            c.fill_rect(Rect::from_xywh(i, 8, 1, 8), Color::rgb(0, t, 0));
            c.fill_rect(Rect::from_xywh(i, 16, 1, 8), Color::rgb(0, 0, t));
            c.fill_rect(Rect::from_xywh(i, 24, 1, 8), Color::rgb(t, t, t));
        }

        // Alpha blending over a mid grey.
        c.fill_rect(Rect::from_xywh(0, 36, 320, 16), Color::hex(0x808080));
        for i in 0..16 {
            let a = (i * 255 / 15) as u8;
            c.fill_rect(Rect::from_xywh(i * 20, 36, 20, 16), Color::hex(0xFF3B30).with_alpha(a));
        }

        // Rounded rects at the radii a small UI actually uses.
        for (i, r) in [0, 1, 2, 3, 4, 6, 8, 12].iter().enumerate() {
            c.fill_round_rect(
                Rect::from_xywh(8 + i as i32 * 38, 58, 32, 28),
                *r,
                Color::hex(0x2A82DA),
            );
        }

        // Text: sizes, weights, and a script that is not Latin.
        let y = 96;
        c.draw_text(Point::new(8, y + f13b.ascent()), "Rust on Symbian", &f13b, Color::WHITE);
        c.draw_text(Point::new(8, y + 20 + f12.ascent()), "ui12 regular — Handgloves 0123", &f12, Color::hex(0xD0D6DC));
        c.draw_text(Point::new(8, y + 37 + f12b.ascent()), "ui12 bold — Handgloves 0123", &f12b, Color::hex(0xD0D6DC));
        c.draw_text(Point::new(8, y + 54 + f10.ascent()), "ui10 small — Handgloves 0123456789", &f10, Color::hex(0x8A9299));
        c.draw_text(Point::new(8, y + 70 + f12.ascent()), "Привет, мир! Ελληνικά. Ação", &f12, Color::hex(0xD0D6DC));

        // Ellipsis truncation and the three alignments, inside visible boxes.
        let box_ = Rect::from_xywh(8, 190, 140, 18);
        c.stroke_rect(box_, Color::hex(0x2A3138));
        c.draw_text_in(box_.inset_xy(3, 0), "truncated because it is long", &f12, Color::hex(0xD0D6DC), Align::Start);

        for (i, al) in [Align::Start, Align::Center, Align::End].iter().enumerate() {
            let b = Rect::from_xywh(160, 190 + i as i32 * 16, 152, 15);
            c.stroke_rect(b, Color::hex(0x2A3138));
            c.draw_text_in(b.inset_xy(3, 0), "align", &f10, Color::hex(0x8A9299), *al);
        }

        // Clip proof: this must not escape its box.
        let clip = Rect::from_xywh(8, 212, 140, 22);
        c.stroke_rect(clip, Color::hex(0x4A5158));
        c.with(clip.inset(1), |c| {
            c.fill_rect(Rect::from_xywh(-50, -50, 400, 400), Color::hex(0x1E5128));
            c.draw_text(Point::new(4, 14), "clipped overspill", &f12, Color::hex(0x7BE495));
        });
    }
    s.save("00-smoke");

    render_app();
}

// ---------------------------------------------------------------------------
// The app screens. Appended as a second entry point so the smoke sheet above
// stays independently runnable.

fn render_app() {
    let (d12, d12b, d10, d13b) = (load("ui12"), load("ui12b"), load("ui10"), load("ui13b"));
    let f12 = BitmapFont::new(&d12).unwrap();
    let f12b = BitmapFont::new(&d12b).unwrap();
    let f10 = BitmapFont::new(&d10).unwrap();
    let f13b = BitmapFont::new(&d13b).unwrap();

    let fonts = symbian_ui::Fonts { body: &f12, strong: &f12b, small: &f10, title: &f13b };
    let dark = symbian_ui::Theme::dark(fonts);
    let light = symbian_ui::Theme::light(fonts);
    let rect = Rect::from_size(E72_SCREEN);

    use symbian_ui::{Key, KeyEvent, Softkey};

    let shot = |name: &str, theme: &symbian_ui::Theme<'_>, keys: &[Key]| {
        let mut app = tg::App::mock();
        for k in keys {
            app.handle_key(KeyEvent::new(*k), theme, rect);
        }
        let mut s = Screen::new(E72_SCREEN);
        {
            let mut c = s.canvas();
            app.draw(&mut c, theme);
        }
        s.save(name);
    };

    shot("10-chats", &dark, &[]);
    shot("11-chats-scrolled", &dark, &[Key::Down, Key::Down, Key::Down, Key::Down, Key::Down]);
    shot("12-conversation", &dark, &[Key::Select]);
    shot("13-transcript-focus", &dark, &[Key::Select, Key::Up, Key::Up, Key::Up]);

    // Typing into the composer, to check the caret and the Send softkey appearing.
    let typed: Vec<Key> = "beleza, vou testar".chars().map(Key::Char).collect();
    let mut keys = vec![Key::Down, Key::Down, Key::Select];
    keys.extend(typed);
    shot("14-composing", &dark, &keys);

    shot("15-cyrillic", &dark, &[Key::Down, Key::Down, Key::Select]);
    shot("16-chats-light", &light, &[]);
    let _ = Softkey::Left;

    render_design_system();
}

// ---------------------------------------------------------------------------
// The design-system contact sheet.
//
// The unit tests prove containment, symmetry and balance — properties a machine can
// check. They cannot tell you whether a 9px bell reads as a bell. This sheet exists
// to be looked at, and the magnified strip is the point: at 1x an icon is too small
// to judge, and at 1x a bevel is too subtle to confirm it is there at all.

/// Copy `src` into `dst` at `at`, magnified `zoom` times.
///
/// Each source pixel becomes a `zoom-1` block with a one-pixel gutter in `grid`, so
/// pixels stay countable *without* the grid overwriting them. Drawing the grid on
/// top instead — the obvious way — clips a pixel off every block and turns three
/// solid rules into what looks like five stripes, which is exactly the kind of lie
/// that makes a contact sheet worse than no sheet.
fn blit_zoom(c: &mut Canvas, at: Point, src: &[u16], src_size: Size, zoom: i32, grid: Color) {
    let block = (zoom - 1).max(1);
    c.fill_rect(
        Rect::from_xywh(at.x, at.y, src_size.w * zoom + 1, src_size.h * zoom + 1),
        grid,
    );
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

fn icon_tile(icon: symbian_ui::icon::Icon, size: i32) -> (Vec<u16>, Size) {
    let w = symbian_ui::icon::width_for(icon, size);
    let sz = Size::new(w, size);
    let mut buf = vec![0u16; (sz.w * sz.h) as usize];
    {
        let mut c = Canvas::from_slice(&mut buf, sz);
        symbian_ui::icon::draw(&mut c, Rect::from_xywh(0, 0, sz.w, sz.h), icon, Color::WHITE);
    }
    (buf, sz)
}

fn render_design_system() {
    use symbian_ui::icon::{self, Icon};
    use symbian_ui::paint;
    use symbian_ui::tokens::Surface;

    let (d10, d12b) = (load("ui10"), load("ui12b"));
    let f10 = BitmapFont::new(&d10).unwrap();
    let f12b = BitmapFont::new(&d12b).unwrap();

    const ALL: &[(Icon, &str)] = &[
        (Icon::ChevronLeft, "chev.l"),
        (Icon::ChevronRight, "chev.r"),
        (Icon::ChevronUp, "chev.u"),
        (Icon::ChevronDown, "chev.d"),
        (Icon::Check, "check"),
        (Icon::CheckDouble, "check2"),
        (Icon::Pending, "pending"),
        (Icon::Warning, "warn"),
        (Icon::Pinned, "pinned"),
        (Icon::Muted, "muted"),
        (Icon::Lock, "lock"),
        (Icon::Group, "group"),
        (Icon::Channel, "chan"),
        (Icon::Attach, "attach"),
        (Icon::Photo, "photo"),
        (Icon::Search, "search"),
        (Icon::Menu, "menu"),
        (Icon::Pencil, "pencil"),
        (Icon::Send, "send"),
        (Icon::Dot, "dot"),
    ];

    // ---- sheet 1: every icon at the four sizes that matter, 1:1 ----
    let mut s = Screen::new(Size::new(320, 300));
    {
        let mut c = s.canvas();
        c.clear(Color::hex(0x101418));
        let dim = Color::hex(0x7E8A94);
        c.draw_text(Point::new(6, 12), "iconography — 9 / 11 / 16 / 24 px", &f12b, Color::WHITE);

        let mut y = 24;
        for chunk in ALL.chunks(5) {
            let mut x = 8;
            for &(ic, name) in chunk {
                c.draw_text(Point::new(x, y + 8), name, &f10, dim);
                let mut iy = y + 14;
                for &size in &[9, 11, 16, 24] {
                    icon::draw(
                        &mut c,
                        Rect::from_xywh(x, iy, icon::width_for(ic, size), size),
                        ic,
                        Color::WHITE,
                    );
                    iy += size + 3;
                }
                x += 62;
            }
            y += 84;
        }
    }
    s.save("20-icons");

    // ---- sheet 2: the small sizes magnified 6x, where legibility is decided ----
    let mut s = Screen::new(Size::new(600, 500));
    {
        let mut c = s.canvas();
        c.clear(Color::hex(0x101418));
        let grid = Color::hex(0x2A3138);
        c.draw_text(Point::new(6, 12), "9px (left) and 11px (right), 6x", &f12b, Color::WHITE);
        let mut y = 20;
        let mut col = 0;
        for &(ic, name) in ALL {
            let x = 8 + col * 196;
            c.draw_text(Point::new(x, y + 10), name, &f10, Color::hex(0x7E8A94));
            let (b9, s9) = icon_tile(ic, 9);
            blit_zoom(&mut c, Point::new(x + 48, y), &b9, s9, 6, grid);
            let (b11, s11) = icon_tile(ic, 11);
            blit_zoom(&mut c, Point::new(x + 122, y), &b11, s11, 6, grid);
            col += 1;
            if col == 3 {
                col = 0;
                y += 76;
            }
        }
    }
    s.save("21-icons-zoom");

    // ---- sheet 3: the surface primitives ----
    let mut s = Screen::new(Size::new(320, 300));
    {
        let mut c = s.canvas();
        let page = Color::hex(0x20262C);
        c.clear(page);
        let ink = Color::WHITE;
        let dim = Color::hex(0x8E9AA4);
        c.draw_text(Point::new(6, 12), "surfaces", &f12b, ink);

        let base = Color::hex(0x3A4C5E);
        let rows: &[(&str, Surface)] = &[
            ("flat", Surface::flat(base)),
            ("gradient", Surface::gradient(Color::hex(0x4E6478), Color::hex(0x2C3A48))),
            ("raised 40", Surface::raised(base, 40)),
            ("raised 90", Surface::raised(base, 90)),
            ("sunken 40", Surface::sunken(base, 40)),
        ];
        let mut y = 20;
        for (name, surf) in rows {
            c.draw_text(Point::new(8, y + 12), name, &f10, dim);
            let r = Rect::from_xywh(80, y, 150, 20);
            paint::band(&mut c, r, surf);
            // The same band magnified, so the 1px edges are actually visible.
            let mut tile = vec![0u16; (40 * 20) as usize];
            {
                let mut tc = Canvas::from_slice(&mut tile, Size::new(40, 20));
                paint::band(&mut tc, Rect::from_xywh(0, 0, 40, 20), surf);
            }
            // 8 columns of the band, magnified, so the 1px edge lines are visible.
            let strip: Vec<u16> = (0..20)
                .flat_map(|row| (0..8).map(move |col| (row, col)))
                .map(|(row, col)| tile[(row * 40 + col) as usize])
                .collect();
            blit_zoom(&mut c, Point::new(238, y - 2), &strip, Size::new(8, 20), 4, page);
            y += 26;
        }

        y += 6;
        c.draw_text(Point::new(6, y + 12), "separators and frames", &f12b, ink);
        y += 22;
        c.draw_text(Point::new(8, y + 10), "engraved, dark bg", &f10, dim);
        paint::separator_for(&mut c, y + 14, 8, 200, page);
        y += 22;
        let light_patch = Rect::from_xywh(8, y, 192, 18);
        c.fill_rect(light_patch, Color::hex(0xD8DEE4));
        c.draw_text(Point::new(12, y + 12), "engraved, light bg", &f10, Color::hex(0x40484F));
        paint::separator_for(&mut c, y + 16, 8, 200, Color::hex(0xD8DEE4));
        y += 26;
        paint::frame_raised(&mut c, Rect::from_xywh(8, y, 90, 22), Color::hex(0x6E7E8E), Color::hex(0x141A20));
        c.draw_text(Point::new(14, y + 15), "raised", &f10, dim);
        paint::frame_sunken(&mut c, Rect::from_xywh(108, y, 90, 22), Color::hex(0x6E7E8E), Color::hex(0x141A20));
        c.draw_text(Point::new(114, y + 15), "sunken", &f10, dim);

        y += 32;
        c.draw_text(Point::new(6, y + 12), "scrollbar: 100 rows, 5 visible", &f12b, ink);
        y += 20;
        for (i, off) in [0, 24, 48, 72, 95].iter().enumerate() {
            let g = Rect::from_xywh(10 + i as i32 * 22, y, 4, 60);
            paint::scrollbar(&mut c, g, 100, 5, *off, Color::hex(0x161C22), Color::hex(0x7E8A94));
        }
        // And the degenerate case: everything fits.
        let g = Rect::from_xywh(10 + 5 * 22, y, 4, 60);
        paint::scrollbar(&mut c, g, 3, 5, 0, Color::hex(0x161C22), Color::hex(0x7E8A94));
        c.draw_text(Point::new(136, y + 30), "last: all fits", &f10, dim);
    }
    s.save("22-surfaces");
}
