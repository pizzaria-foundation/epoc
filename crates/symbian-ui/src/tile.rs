//! The "letter tile": a coloured rounded square with the caption's first letter — the placeholder
//! icon a launcher draws when an app has no real icon of its own.
//!
//! It lives in the SDK (not in the launcher) so every list that shows apps draws the *same* fake
//! icon from one place: the home grid, the application menu, and the [`crate::app_picker`] drawer.
//! Pure geometry + one glyph, so it is crisp at any size and owes nothing to a bitmap.

use alloc::string::String;

use symbian_gfx::{Align, Canvas, Color, Rect};

use crate::theme::Theme;

/// The tile background palette, indexed by a per-item seed (usually the app's UID). Eight saturated
/// hues, each chosen to read white text on top.
pub const TILE_COLOURS: [u32; 8] = [
    0xFF_3B82F6, // blue
    0xFF_10B981, // green
    0xFF_F59E0B, // amber
    0xFF_8B5CF6, // violet
    0xFF_14B8A6, // teal
    0xFF_EF4444, // red
    0xFF_EC4899, // pink
    0xFF_6366F1, // indigo
];

/// Draw the seeded letter tile filling `rect`: a rounded square in a seed-picked colour with the
/// first non-space character of `caption`, upper-cased and centred in white. `seed` picks the hue —
/// pass the app UID so an app keeps its colour across screens.
pub fn letter_tile(c: &mut Canvas<'_>, rect: Rect, caption: &str, seed: u32, theme: &Theme<'_>) {
    if rect.is_empty() {
        return;
    }
    let bg = Color(TILE_COLOURS[(seed as usize) % TILE_COLOURS.len()]);
    let radius = (rect.width().min(rect.height()) / 5).clamp(2, 8);
    c.fill_round_rect(rect, radius, bg);

    let ch = caption.chars().find(|c| !c.is_whitespace()).unwrap_or('?').to_ascii_uppercase();
    let mut s = String::new();
    s.push(ch);
    // White reads on every tile colour above; the tile is the contrast, not the theme.
    c.draw_text_in(rect, &s, theme.fonts.strong, Color::WHITE, Align::Center);
}
