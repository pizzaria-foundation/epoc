//! Iconography drawn as geometry, not as glyphs or bitmaps.
//!
//! # Why not a font, and why not emoji
//!
//! Emoji are wrong here for three separate reasons, and only the first is
//! aesthetic. They are colour images the theme cannot recolour, so a muted-chat
//! marker stays the same in every theme including the high-contrast one. They are
//! 60-odd KB of atlas each for a handful of shapes. And at 11px an emoji is a
//! four-colour smudge — the E72's panel has 169 ppi, which sounds like a lot until
//! you remember that a 12px em box is 12 actual pixels no matter the density.
//!
//! A glyph font is better but still anti-aliased: `mkfont.py` renders with
//! grayscale coverage, so a 9px chevron arrives as three rows of grey. The era's
//! own icons were hand-pixelled precisely because nothing else is sharp at this
//! size.
//!
//! So these are procedural. Every shape is built from axis-aligned runs and 45°
//! diagonals, the two things that are exactly crisp on a pixel grid at any size,
//! and each takes a colour — so the theme owns the appearance and there is nothing
//! to ship.
//!
//! # The size contract
//!
//! `draw` centres the icon in the rect it is given and works from 7px up. Shapes
//! are derived from the box rather than scaled from a fixed design, which is what
//! keeps a 9px tick and a 16px tick both sharp instead of one being a resampled
//! version of the other.

use symbian_gfx::{Canvas, Color, Rect};

/// The set the chat client actually draws. Deliberately small: every icon here has
/// a call site, because an icon nobody uses is a shape nobody checked at 9px.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Icon {
    /// Navigation: "there is more this way". Also the back affordance.
    ChevronLeft,
    ChevronRight,
    ChevronUp,
    ChevronDown,
    /// Sent. One tick.
    Check,
    /// Delivered and read. Two overlapping ticks — the second offset, not a
    /// separate glyph, so they read as a pair at 9px.
    CheckDouble,
    /// Sending. An hourglass, not a clock: a clock face at 9px has a 3px interior,
    /// which is not enough for two hands, so it renders as a plain donut.
    Pending,
    /// Failed to send.
    Warning,
    /// Pinned to the top of the list. A bookmark, not a pushpin — a pushpin at this
    /// size is a head, a neck and a needle in 9 rows, and reads as a dagger.
    Pinned,
    /// Notifications off. A speaker with a stroke through it, not a bell: a bell is
    /// a dome over a flared body, and at 9px both taper into the same triangle.
    Muted,
    /// Secret chat, or anything encrypted.
    Lock,
    /// A group rather than a person: two overlapping figures.
    Group,
    /// A broadcast channel: stacked arcs, the era's "signal going out" idiom.
    Channel,
    /// Attachment. A page with a folded corner rather than a paperclip: a clip is two
    /// nested U-turns, and at 9px the inner one closes up into a solid ring.
    Attach,
    /// An image, collapsed. The IRC-mode placeholder.
    Photo,
    /// Search.
    Search,
    /// The options menu: three stacked rules.
    Menu,
    /// Compose or edit.
    Pencil,
    /// Send. A paper plane, matching the app icon.
    Send,
    /// A filled dot: unread, or online.
    Dot,
}

/// Draw `icon` centred in `r`, in `color`.
///
/// The height is snapped to the largest odd value that fits, because a symmetric
/// shape on an even grid has no centre column and ends up a pixel off-balance —
/// visible on a chevron, glaring on a dot. Width comes from [`width_for`], so a
/// non-square icon gets the room it needs instead of being squeezed into a square.
pub fn draw(c: &mut Canvas, r: Rect, icon: Icon, color: Color) {
    if r.width() < 5 || r.height() < 5 {
        return;
    }
    // Shrink the nominal height until the icon's natural width also fits. One step
    // per iteration and a floor of 5, so this is bounded and cannot spin.
    let mut s = r.height().min(r.width());
    if s % 2 == 0 {
        s -= 1;
    }
    while s >= 5 && width_for(icon, s) > r.width() {
        s -= 2;
    }
    if s < 5 {
        return;
    }
    let w = width_for(icon, s);
    let x = r.x0 + (r.width() - w) / 2;
    let y = r.y0 + (r.height() - s) / 2;
    let b = Rect::from_xywh(x, y, w, s);
    match icon {
        Icon::ChevronLeft => chevron(c, b, color, Dir::Left),
        Icon::ChevronRight => chevron(c, b, color, Dir::Right),
        Icon::ChevronUp => chevron(c, b, color, Dir::Up),
        Icon::ChevronDown => chevron(c, b, color, Dir::Down),
        Icon::Check => tick(c, b, color),
        Icon::CheckDouble => {
            // Two full-size ticks in sub-boxes, each the height of the icon, offset
            // by half. Each stays inside its own box, so containment is structural
            // rather than something the arm arithmetic has to remember — and each is
            // the same shape as a single tick, so "sent" and "read" are recognisably
            // the same mark counted once or twice.
            //
            // The rear one is drawn first so the front overlaps it. The overlap is
            // what makes the pair read as one symbol rather than as a stutter.
            tick(c, Rect::from_xywh(b.x0 + (b.width() - s), b.y0, s, s), color);
            tick(c, Rect::from_xywh(b.x0, b.y0, s, s), color);
        }
        Icon::Pending => hourglass(c, b, color),
        Icon::Warning => warning(c, b, color),
        Icon::Pinned => bookmark(c, b, color),
        Icon::Muted => muted(c, b, color),
        Icon::Lock => lock(c, b, color),
        Icon::Group => group(c, b, color),
        Icon::Channel => channel(c, b, color),
        Icon::Attach => page(c, b, color),
        Icon::Photo => photo(c, b, color),
        Icon::Search => search(c, b, color),
        Icon::Menu => menu(c, b, color),
        Icon::Pencil => pencil(c, b, color),
        Icon::Send => send(c, b, color),
        Icon::Dot => dot(c, b, color),
    }
}

/// The natural width of `icon` at height `h`.
///
/// Ask rather than assume square. A double tick is genuinely wider than a single
/// one — squeezing it into the same box means two half-size ticks 3px apart, which
/// at 9px merge into one indistinct blot. Half again is the narrowest offset at
/// which the two marks stay separately countable.
pub fn width_for(icon: Icon, h: i32) -> i32 {
    match icon {
        Icon::CheckDouble => h * 3 / 2,
        _ => h,
    }
}

// ------------------------------------------------------------------- helpers --

enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// A 45° run of `len` pixels from (x, y), stepping by (dx, dy) which must both be
/// ±1 or 0. Exactly crisp: one pixel per step, no coverage, no rounding.
fn diag(c: &mut Canvas, x: i32, y: i32, dx: i32, dy: i32, len: i32, color: Color) {
    for i in 0..len {
        c.fill_rect(Rect::from_xywh(x + dx * i, y + dy * i, 1, 1), color);
    }
}

/// A 2px-thick 45° run: the same line drawn twice, offset along whichever axis is
/// perpendicular to it. Used where a 1px diagonal would look faint next to the
/// 1px-but-solid horizontal strokes around it.
fn diag2(c: &mut Canvas, x: i32, y: i32, dx: i32, dy: i32, len: i32, color: Color) {
    diag(c, x, y, dx, dy, len, color);
    diag(c, x + 1, y, dx, dy, len, color);
}

/// Integer midpoint circle, outline only.
fn circle(c: &mut Canvas, cx: i32, cy: i32, rad: i32, color: Color) {
    if rad < 1 {
        return;
    }
    let mut x = rad;
    let mut y = 0;
    let mut err = 1 - rad;
    let put = |c: &mut Canvas, px: i32, py: i32| {
        c.fill_rect(Rect::from_xywh(px, py, 1, 1), color);
    };
    while x >= y {
        put(c, cx + x, cy + y);
        put(c, cx + y, cy + x);
        put(c, cx - y, cy + x);
        put(c, cx - x, cy + y);
        put(c, cx - x, cy - y);
        put(c, cx - y, cy - x);
        put(c, cx + y, cy - x);
        put(c, cx + x, cy - y);
        y += 1;
        if err < 0 {
            err += 2 * y + 1;
        } else {
            x -= 1;
            err += 2 * (y - x) + 1;
        }
    }
}

/// Filled circle, by horizontal runs so there is no double-writing.
fn disc(c: &mut Canvas, cx: i32, cy: i32, rad: i32, color: Color) {
    for dy in -rad..=rad {
        let w = isqrt(rad * rad - dy * dy);
        c.hline(cy + dy, cx - w, cx + w + 1, color);
    }
}

fn isqrt(v: i32) -> i32 {
    if v <= 0 {
        return 0;
    }
    let mut x = v;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + v / x) / 2;
    }
    x
}

// -------------------------------------------------------------------- shapes --

/// Two 45° runs meeting at a point. 2px thick, because a 1px chevron next to 11px
/// text reads as a speck.
fn chevron(c: &mut Canvas, b: Rect, color: Color, dir: Dir) {
    let arm = b.width() / 2;
    let cx = b.x0 + b.width() / 2;
    let cy = b.y0 + b.height() / 2;
    match dir {
        // Apex at the left, arms opening to the right.
        Dir::Left => {
            let ax = cx - arm / 2;
            diag(c, ax, cy, 1, -1, arm, color);
            diag(c, ax, cy, 1, 1, arm, color);
            diag(c, ax + 1, cy, 1, -1, arm, color);
            diag(c, ax + 1, cy, 1, 1, arm, color);
        }
        Dir::Right => {
            let ax = cx + arm / 2;
            diag(c, ax, cy, -1, -1, arm, color);
            diag(c, ax, cy, -1, 1, arm, color);
            diag(c, ax - 1, cy, -1, -1, arm, color);
            diag(c, ax - 1, cy, -1, 1, arm, color);
        }
        Dir::Up => {
            let ay = cy - arm / 2;
            diag(c, cx, ay, -1, 1, arm, color);
            diag(c, cx, ay, 1, 1, arm, color);
            diag(c, cx, ay + 1, -1, 1, arm, color);
            diag(c, cx, ay + 1, 1, 1, arm, color);
        }
        Dir::Down => {
            let ay = cy + arm / 2;
            diag(c, cx, ay, -1, -1, arm, color);
            diag(c, cx, ay, 1, -1, arm, color);
            diag(c, cx, ay - 1, -1, -1, arm, color);
            diag(c, cx, ay - 1, 1, -1, arm, color);
        }
    }
}

/// A tick filling `b`: a short down-right stroke into a long up-right one.
///
/// Both arm lengths are derived from the room actually left in the box rather than
/// from a fraction of its size. `diag2` puts a second run one pixel to the right of
/// the first, so a length taken from the size alone overshoots by that pixel — and
/// the double tick, which draws into a narrower box, overshoots by more.
fn tick(c: &mut Canvas, b: Rect, color: Color) {
    let (w, h) = (b.width(), b.height());
    // Arm lengths first, then place the elbow so the result is centred. Doing it the
    // other way round — elbow at a fixed fraction, arms filling what is left — puts
    // the whole shape in the upper half, because both arms rise from the elbow.
    let short = (w / 3).max(2);
    let long = (w * 2 / 3).max(3).min(w - short);
    // Ink spans `short + long` across and `long` down (the long arm sets the top,
    // the elbow the bottom), plus one column for diag2's second run.
    let ex = b.x0 + (w - short - long) / 2 + short - 1;
    let ey = b.y0 + (h + long) / 2 - 1;
    diag2(c, ex - short + 1, ey - short + 1, 1, 1, short, color);
    diag2(c, ex, ey, 1, -1, long, color);
}

/// An hourglass: two triangles apex to apex, capped top and bottom.
///
/// Legible at 9px in a way a clock is not, because every row is either a full-width
/// cap or a symmetric pair of runs — no interior detail to lose.
fn hourglass(c: &mut Canvas, b: Rect, color: Color) {
    let s = b.width();
    let cx = b.x0 + s / 2;
    let half = s / 2 - 1;
    // Caps top and bottom; they are what make the silhouette an hourglass rather
    // than a bowtie.
    c.hline(b.y0, cx - half, cx + half + 1, color);
    c.hline(b.y1 - 1, cx - half, cx + half + 1, color);
    let inner = s - 2;
    for i in 0..inner {
        let y = b.y0 + 1 + i;
        // Distance from the waist, normalised so the widest row sits just inside the
        // caps and the narrowest is a single column.
        let d = (i * 2 - (inner - 1)).abs();
        let w = (d * half) / (inner - 1).max(1);
        c.hline(y, cx - w, cx + w + 1, color);
    }
}

/// A solid triangle, point up.
///
/// Solid, with no exclamation mark inside. An interior mark would have to be
/// punched out, which needs the background colour, and at 9px the punch would be a
/// single pixel row that reads as a rendering fault rather than as a "!". The
/// silhouette alone is unambiguous, which is why road signs work at distance.
fn warning(c: &mut Canvas, b: Rect, color: Color) {
    let s = b.width();
    let cx = b.x0 + s / 2;
    for i in 0..s {
        let half = (i * (s / 2)) / (s - 1);
        c.hline(b.y0 + i, cx - half, cx + half + 1, color);
    }
}

/// A bookmark: a rectangle with a V notched out of its foot.
///
/// The notch is built into the row spans rather than punched out afterwards, since
/// an "erase" needs a background colour this function does not have.
fn bookmark(c: &mut Canvas, b: Rect, color: Color) {
    let s = b.width();
    let w = (s * 2 / 3) | 1; // odd, so the notch has a centre column
    let x0 = b.x0 + (s - w) / 2;
    let cx = x0 + w / 2;
    let notch = (w / 2).max(1);
    let body = s - notch;
    for i in 0..s {
        let y = b.y0 + i;
        if i < body {
            c.hline(y, x0, x0 + w, color);
        } else {
            // Inside the notch the shape splits into two tapering legs.
            let cut = notch - (s - 1 - i);
            c.hline(y, x0, cx - cut + 1, color);
            c.hline(y, cx + cut, x0 + w, color);
        }
    }
}

/// A speaker beside a cross.
///
/// Beside, not struck through. A diagonal drawn *across* the speaker in the same
/// colour merges with it into one blob — there is no second colour to separate them
/// with, because the icon is given one. Two shapes side by side stay two shapes at
/// any size, and "speaker + cross" is as unambiguous as "speaker with a slash".
fn muted(c: &mut Canvas, b: Rect, color: Color) {
    let s = b.width();
    let cy = b.y0 + s / 2;
    // Speaker in the left three fifths, cross in the right two.
    let spk_w = (s * 3 / 5).max(3);
    let drv_w = (spk_w / 3).max(1);
    let drv_h = (s / 3).max(3);
    let cone_w = spk_w - drv_w;
    c.fill_rect(Rect::from_xywh(b.x0, cy - drv_h / 2, drv_w, drv_h), color);
    for i in 0..cone_w {
        // Widening rightward from the driver's height to the full box.
        let half = (drv_h / 2) + ((i + 1) * (s / 2 - drv_h / 2)) / cone_w;
        c.vline(b.x0 + drv_w + i, cy - half, cy + half + 1, color);
    }
    // The cross fills whatever is left to the right of the speaker — measured, not
    // assumed. A `.max(3)` floor instead would overrun the box at 7px, where there
    // are only two columns to spare.
    let sx = b.x0 + spk_w + 1;
    let arm = (b.x1 - sx).min(s - 2).max(2);
    let sy = cy - arm / 2;
    diag(c, sx, sy, 1, 1, arm, color);
    diag(c, sx, sy + arm - 1, 1, -1, arm, color);
}

fn lock(c: &mut Canvas, b: Rect, color: Color) {
    let s = b.width();
    let body_h = (s / 2).max(3);
    let rad = (s / 3).max(2);
    // Height comes out of the parts, then the whole thing is placed — rather than
    // pinning the body to the bottom edge and letting the shackle land wherever it
    // lands, which leaves the lock sitting low with a gap above it.
    let total = rad + 1 + body_h;
    let top = b.y0 + (s - total).max(0) / 2;
    let body = Rect::from_xywh(b.x0 + s / 6, top + rad + 1, s - s / 3, body_h);
    c.fill_rect(body, color);
    // The shackle: an arc, drawn as a circle emitting only the rows above the body.
    let cx = b.x0 + s / 2;
    let cy = body.y0 - 1;
    let mut x = rad;
    let mut y = 0;
    let mut err = 1 - rad;
    while x >= y {
        for (px, py) in [
            (cx + x, cy - y),
            (cx - x, cy - y),
            (cx + y, cy - x),
            (cx - y, cy - x),
        ] {
            if py >= top {
                c.fill_rect(Rect::from_xywh(px, py, 1, 1), color);
            }
        }
        y += 1;
        if err < 0 {
            err += 2 * y + 1;
        } else {
            x -= 1;
            err += 2 * (y - x) + 1;
        }
    }
}

/// Two heads over one shared body.
///
/// Two full head-and-shoulders figures is the obvious drawing and the wrong one: at
/// 9px two overlapping bodies merge into a single lumpy blob. Two separated heads
/// above one wide body keeps the countable part — the heads — clear of each other,
/// which is the only detail that has to survive.
fn group(c: &mut Canvas, b: Rect, color: Color) {
    let s = b.width();
    let head = (s / 6).max(1);
    let gap = (head + 1).max(2);
    let cx = b.x0 + s / 2;
    let hy = b.y0 + head;
    disc(c, cx - gap, hy, head, color);
    disc(c, cx + gap, hy, head, color);
    // One body spanning both, starting a row below the heads.
    let by = hy + head + 2;
    let bw = s - 1;
    let bh = (b.y1 - by).max(2);
    c.fill_round_rect(Rect::from_xywh(cx - bw / 2, by, bw, bh), bw / 3, color);
}

fn channel(c: &mut Canvas, b: Rect, color: Color) {
    let s = b.width();
    let (ox, oy) = (b.x0, b.y1 - 1);
    c.fill_rect(Rect::from_xywh(ox, oy - 1, 2, 2), color);
    let reach = s - 1;
    for k in 1..=3 {
        let rad = (reach * k / 3).max(1);
        let mut x = rad;
        let mut y = 0;
        let mut err = 1 - rad;
        while x >= y {
            // Both octants of the quadrant, so the arc is continuous through 45°.
            for (px, py) in [(ox + x, oy - y), (ox + y, oy - x)] {
                if px < b.x1 && py >= b.y0 {
                    c.fill_rect(Rect::from_xywh(px, py, 1, 1), color);
                }
            }
            y += 1;
            if err < 0 {
                err += 2 * y + 1;
            } else {
                x -= 1;
                err += 2 * (y - x) + 1;
            }
        }
    }
}

/// A page with its top-right corner folded away.
///
/// The outline is three straight sides plus a 45° corner, and the fold is the same
/// diagonal drawn again inside — all of it crisp, and the notched silhouette alone
/// says "document" even when the interior detail is one pixel.
fn page(c: &mut Canvas, b: Rect, color: Color) {
    let s = b.width();
    let w = (s * 3 / 4).max(5);
    let x0 = b.x0 + (s - w) / 2;
    let x1 = x0 + w - 1;
    let fold = (w / 3).max(2);
    // Left, bottom and the lower part of the right edge.
    c.vline(x0, b.y0, b.y1, color);
    c.hline(b.y1 - 1, x0, x1 + 1, color);
    c.vline(x1, b.y0 + fold, b.y1, color);
    // Top edge, stopping where the fold begins.
    c.hline(b.y0, x0, x1 - fold + 1, color);
    // The cut corner, and the fold line just inside it.
    diag(c, x1 - fold, b.y0, 1, 1, fold + 1, color);
    c.hline(b.y0 + fold, x1 - fold, x1 + 1, color);
}

fn photo(c: &mut Canvas, b: Rect, color: Color) {
    let s = b.width();
    let f = Rect::from_xywh(b.x0, b.y0 + s / 6, s, s - s / 3);
    c.stroke_rect(f, color);
    // A horizon and a sun: the two marks that say "picture" at 9px. A mountain
    // outline needs three diagonals and stops being legible below 13.
    let hz = f.y1 - (f.height() / 3);
    c.hline(hz, f.x0 + 1, f.x1 - 1, color);
    disc(c, f.x0 + f.width() / 3, f.y0 + f.height() / 3, (s / 8).max(1), color);
}

fn search(c: &mut Canvas, b: Rect, color: Color) {
    let s = b.width();
    let rad = (s * 2 / 5).max(2);
    let cx = b.x0 + rad + 1;
    let cy = b.y0 + rad + 1;
    circle(c, cx, cy, rad, color);
    // The handle leaves at 45°, which is both the conventional angle and the only
    // one that is crisp. Its length is whatever room is left inside the box rather
    // than a fraction of the size: diag2 draws a second run one pixel to the right,
    // so a length derived from `s` alone overshoots by that pixel at small sizes.
    let (sx, sy) = (cx + rad - 1, cy + rad - 1);
    let len = (b.x1 - 1 - sx).min(b.y1 - sy).max(1);
    diag2(c, sx, sy, 1, 1, len, color);
}

fn menu(c: &mut Canvas, b: Rect, color: Color) {
    let s = b.height();
    // Three rules at quarter points, so the gaps are equal and the block is
    // vertically centred whatever the odd size is.
    for k in 1..=3 {
        let y = b.y0 + (s * k) / 4;
        c.hline(y, b.x0, b.x1, color);
    }
}

fn pencil(c: &mut Canvas, b: Rect, color: Color) {
    let s = b.width();
    // A 2px diagonal body from bottom-left to top-right, with the tip left as a
    // 1px point and the tail squared off.
    diag2(c, b.x0 + 1, b.y1 - 2, 1, -1, s - 2, color);
    diag(c, b.x0 + 3, b.y1 - 2, 1, -1, s - 4, color);
    c.hline(b.y1 - 1, b.x0, b.x0 + 3, color);
}

/// A paper plane: a right-pointing wedge whose tail is notched.
///
/// The notch is built into the spans rather than punched out afterwards. Punching
/// would need the background colour, and `Color` with alpha 0 is a documented no-op
/// in `fill_rect`, so an "erase" pass silently does nothing — which is exactly the
/// bug the shape-inside-the-box test caught.
///
/// Below 9px there is no room for a notch, and the plain wedge is the better
/// reading anyway.
fn send(c: &mut Canvas, b: Rect, color: Color) {
    let s = b.width();
    let cy = b.y0 + s / 2;
    let notch = if s >= 9 { s / 4 } else { 0 };
    for i in 0..s {
        let half = ((s - 1 - i) * (s / 2)) / (s - 1);
        if half == 0 {
            // The tip: one pixel, since a zero-height vline draws nothing.
            c.fill_rect(Rect::from_xywh(b.x0 + i, cy, 1, 1), color);
            continue;
        }
        // Inside the tail the wedge splits into an upper and a lower wing, with the
        // notch widening towards the very back.
        let gap = if i < notch { notch - i } else { 0 };
        if gap == 0 {
            c.vline(b.x0 + i, cy - half, cy + half + 1, color);
        } else {
            c.vline(b.x0 + i, cy - half, cy - gap + 1, color);
            c.vline(b.x0 + i, cy + gap, cy + half + 1, color);
        }
    }
}

fn dot(c: &mut Canvas, b: Rect, color: Color) {
    let rad = (b.width() / 2 - 1).max(1);
    disc(c, b.x0 + b.width() / 2, b.y0 + b.height() / 2, rad, color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_gfx::Size;

    const ALL: &[Icon] = &[
        Icon::ChevronLeft,
        Icon::ChevronRight,
        Icon::ChevronUp,
        Icon::ChevronDown,
        Icon::Check,
        Icon::CheckDouble,
        Icon::Pending,
        Icon::Warning,
        Icon::Pinned,
        Icon::Muted,
        Icon::Lock,
        Icon::Group,
        Icon::Channel,
        Icon::Attach,
        Icon::Photo,
        Icon::Search,
        Icon::Menu,
        Icon::Pencil,
        Icon::Send,
        Icon::Dot,
    ];

    /// Draw into a padded canvas so an out-of-bounds write shows up as ink in the
    /// margin rather than being silently clipped.
    ///
    /// The box is `width_for(icon, size)` wide, which is the contract callers are
    /// told to honour. Handing every icon a square box instead would silently shrink
    /// the wide ones — the double tick would come back at height 5 in a 9px box, and
    /// every assertion about it would be measuring the wrong picture.
    fn render(icon: Icon, size: i32) -> Rendered {
        let pad = 4;
        let w = width_for(icon, size);
        let dim = Size::new(w + pad * 2, size + pad * 2);
        let mut buf = alloc::vec![0u16; (dim.w * dim.h) as usize];
        {
            let mut c = Canvas::from_slice(&mut buf, dim);
            draw(&mut c, Rect::from_xywh(pad, pad, w, size), icon, Color::hex(0xFFFFFF));
        }
        Rendered { buf, dim, pad, w, h: size }
    }

    struct Rendered {
        buf: alloc::vec::Vec<u16>,
        dim: Size,
        pad: i32,
        w: i32,
        h: i32,
    }

    impl Rendered {
        fn at(&self, x: i32, y: i32) -> u16 {
            self.buf[(y * self.dim.w + x) as usize]
        }
        fn has(&self, x: i32, y: i32) -> bool {
            self.at(x, y) != 0
        }
        fn ink(&self) -> usize {
            self.buf.iter().filter(|&&v| v != 0).count()
        }
        fn cols(&self) -> alloc::vec::Vec<i32> {
            (0..self.dim.w).filter(|&x| (0..self.dim.h).any(|y| self.has(x, y))).collect()
        }
        fn rows(&self) -> alloc::vec::Vec<i32> {
            (0..self.dim.h).filter(|&y| (0..self.dim.w).any(|x| self.has(x, y))).collect()
        }
    }

    #[test]
    fn every_icon_draws_something_at_every_size() {
        // 9 and 11 are the sizes the chat actually uses; 7 and 16 are the ends of
        // the documented range.
        for &size in &[7, 9, 11, 16] {
            for &icon in ALL {
                assert!(render(icon, size).ink() > 0, "{icon:?} at {size}px drew nothing");
            }
        }
    }

    #[test]
    fn every_icon_stays_inside_its_box() {
        for &size in &[7, 9, 11, 16] {
            for &icon in ALL {
                let r = render(icon, size);
                for y in 0..r.dim.h {
                    for x in 0..r.dim.w {
                        let inside = x >= r.pad
                            && x < r.pad + r.w
                            && y >= r.pad
                            && y < r.pad + r.h;
                        if !inside {
                            assert!(!r.has(x, y), "{icon:?} at {size}px leaked to ({x},{y})");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn nothing_is_drawn_below_the_minimum_size() {
        for &icon in ALL {
            assert_eq!(render(icon, 4).ink(), 0, "{icon:?} drew at 4px, below the 5px floor");
        }
    }

    #[test]
    fn icons_are_not_almost_solid() {
        // A shape that fills its box is a bug, not an icon: it means the geometry
        // collapsed. Warning is the densest legitimate one (a filled triangle,
        // ~50%), so 70% is a generous ceiling.
        for &size in &[9, 11, 16] {
            for &icon in ALL {
                let r = render(icon, size);
                let frac = r.ink() * 100 / (r.w * r.h) as usize;
                assert!(frac < 70, "{icon:?} at {size}px is {frac}% solid");
            }
        }
    }

    #[test]
    fn chevrons_are_reflections_of_each_other() {
        // Mirroring left gives right. This catches the arm arithmetic drifting in one
        // direction only, which is invisible until two chevrons sit side by side in a
        // settings row.
        let l = render(Icon::ChevronLeft, 11);
        let r = render(Icon::ChevronRight, 11);
        for y in 0..l.dim.h {
            for x in 0..l.dim.w {
                assert_eq!(l.at(x, y), r.at(l.dim.w - 1 - x, y), "differ at ({x},{y})");
            }
        }
    }

    #[test]
    fn vertical_chevrons_are_reflections_of_each_other() {
        let u = render(Icon::ChevronUp, 11);
        let d = render(Icon::ChevronDown, 11);
        for y in 0..u.dim.h {
            for x in 0..u.dim.w {
                assert_eq!(u.at(x, y), d.at(x, u.dim.h - 1 - y), "differ at ({x},{y})");
            }
        }
    }

    #[test]
    fn double_check_is_visibly_more_than_single() {
        // Two marks must be countable as two. Both the extra width and the extra ink
        // are asserted: width alone would pass if the second tick were a stub, and
        // ink alone would pass if the two were drawn on top of each other.
        for &size in &[9, 11, 16] {
            let one = render(Icon::Check, size);
            let two = render(Icon::CheckDouble, size);
            let span = |r: &Rendered| {
                let c = r.cols();
                c[c.len() - 1] - c[0]
            };
            assert!(span(&two) > span(&one), "at {size}px the double tick is no wider");
            assert!(
                two.ink() > one.ink() * 3 / 2,
                "at {size}px: single {} px of ink, double {} — too close to tell apart",
                one.ink(),
                two.ink()
            );
        }
    }

    #[test]
    fn dot_is_symmetric_about_both_axes() {
        let r = render(Icon::Dot, 11);
        for y in 0..r.dim.h {
            for x in 0..r.dim.w {
                assert_eq!(r.at(x, y), r.at(r.dim.w - 1 - x, y), "asymmetric in x");
                assert_eq!(r.at(x, y), r.at(x, r.dim.h - 1 - y), "asymmetric in y");
            }
        }
    }

    #[test]
    fn menu_draws_exactly_three_rules() {
        let r = render(Icon::Menu, 11);
        let rows = r.rows();
        assert_eq!(rows.len(), 3, "expected 3 rules, got rows {rows:?}");
        // The gaps must be equal, or it reads as a list rather than as a menu.
        assert_eq!(rows[1] - rows[0], rows[2] - rows[1]);
        assert!(rows[0] > r.pad && *rows.last().unwrap() < r.pad + r.h);
    }

    #[test]
    fn every_icon_is_balanced_in_its_box() {
        // An odd-sized shape cannot be exactly centred in an even box — that is
        // arithmetic, not a bug, and forcing it would mean an even shape with no
        // centre column. What must hold is that the leftover is split as evenly as it
        // can be, on both axes, for every icon. Two pixels of slack is the tolerance;
        // beyond that a shape reads as pinned to one edge, which is obvious the
        // moment two different icons sit in the same column of a list.
        for &size in &[11, 12] {
            for &icon in ALL {
                let r = render(icon, size);
                let (cols, rows) = (r.cols(), r.rows());
                let l = cols[0] - r.pad;
                let right = (r.pad + r.w - 1) - cols[cols.len() - 1];
                let t = rows[0] - r.pad;
                let bot = (r.pad + r.h - 1) - rows[rows.len() - 1];
                assert!(
                    (l - right).abs() <= 2,
                    "{icon:?} at {size}px: horizontal margins {l} vs {right}"
                );
                assert!(
                    (t - bot).abs() <= 2,
                    "{icon:?} at {size}px: vertical margins {t} vs {bot}"
                );
            }
        }
    }

    #[test]
    fn only_the_double_tick_is_wider_than_it_is_tall() {
        for &icon in ALL {
            let w = width_for(icon, 11);
            if icon == Icon::CheckDouble {
                assert!(w > 11, "the double tick needs room for two marks, got {w}");
            } else {
                assert_eq!(w, 11, "{icon:?} should be square");
            }
        }
    }

    #[test]
    fn a_box_too_narrow_for_the_natural_width_shrinks_rather_than_overflowing() {
        // The double tick wants 16px of width at height 11. Given only 11, it must
        // come back smaller and still inside — not clipped, and not spilling.
        let pad = 4;
        let dim = Size::new(11 + pad * 2, 11 + pad * 2);
        let mut buf = alloc::vec![0u16; (dim.w * dim.h) as usize];
        {
            let mut c = Canvas::from_slice(&mut buf, dim);
            draw(
                &mut c,
                Rect::from_xywh(pad, pad, 11, 11),
                Icon::CheckDouble,
                Color::hex(0xFFFFFF),
            );
        }
        let has = |x: i32, y: i32| buf[(y * dim.w + x) as usize] != 0;
        assert!((0..dim.h).any(|y| (0..dim.w).any(|x| has(x, y))), "drew nothing");
        for y in 0..dim.h {
            for x in 0..dim.w {
                let inside = (pad..pad + 11).contains(&x) && (pad..pad + 11).contains(&y);
                assert!(inside || !has(x, y), "spilled to ({x},{y})");
            }
        }
    }
}
