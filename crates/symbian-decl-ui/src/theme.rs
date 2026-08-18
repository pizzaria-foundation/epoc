//! Naming a font instead of holding one.
//!
//! A widget is built long before it is drawn, and the [`Theme`] it will be drawn against does not
//! exist yet — its fonts borrow atlases owned by the host's frame loop. A `Text` that stored a
//! `&dyn Font` would therefore carry a lifetime, and that lifetime would spread: into
//! `Box<dyn Widget>`, into the container holding it, into `DeclarativeApp::view`'s return type, and
//! finally into the model, which is the one place in this design that is supposed to be plain data.
//!
//! So a widget stores a *role* — "this is body text", "this is a title" — and resolves it against
//! whatever theme is current at draw time. Retheming then costs nothing: the same tree drawn
//! against a different [`Theme`] picks up different fonts, because it never captured the old ones.

use symbian_gfx::Font;
use symbian_ui::Theme;

/// Which of the theme's four fonts a widget means.
///
/// Four, and no way to ask for a fifth: the device has a fixed set of atlases compiled into the
/// image, and a role that resolved to "whatever the caller passed" would be a font reference again
/// with extra steps. See [`symbian_ui::Fonts`] for what each one is for.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default, Hash)]
#[repr(u8)]
pub enum FontRole {
    /// Body text. The default, because most text is.
    #[default]
    Body,
    /// Emphasis: contact names, the author of a bubble.
    Strong,
    /// Timestamps, previews, hints, softkey labels.
    Small,
    /// The title bar.
    Title,
}

impl FontRole {
    /// The font this role resolves to.
    ///
    /// The plan put this on `Fonts` as `Fonts::resolve(role)`. It cannot live there: `Fonts` belongs
    /// to `symbian-ui`, so an inherent `impl` in this crate is not allowed, and an extension trait
    /// would have to be imported by every file that draws a word. A method on the role reads the
    /// same way at the call site and needs no import beyond the enum itself.
    ///
    /// The returned reference borrows the *atlas*, not the theme, which is what lets a widget
    /// resolve a font and hand it straight to a `Canvas` call without threading a second lifetime
    /// through the draw signature.
    pub fn font<'a>(self, theme: &Theme<'a>) -> &'a dyn Font {
        match self {
            FontRole::Body => theme.fonts.body,
            FontRole::Strong => theme.fonts.strong,
            FontRole::Small => theme.fonts.small,
            FontRole::Title => theme.fonts.title,
        }
    }

    /// Baseline-to-baseline distance for this role — one line's worth of height.
    pub fn line_height(self, theme: &Theme<'_>) -> i32 {
        self.font(theme).line_height()
    }

    /// Width of `s` in this role, in pixels.
    pub fn measure(self, theme: &Theme<'_>, s: &str) -> i32 {
        self.font(theme).measure(s)
    }

    /// A distinct byte per role, for [`content_hash`](crate::Widget::content_hash).
    ///
    /// `as u8` on the enum would do the same thing and would silently change meaning if a variant
    /// were ever inserted in the middle. Nothing breaks when that happens — a hash only has to
    /// distinguish, not to be stable across builds — but the cast being deliberate is the note that
    /// says so.
    pub const fn tag(self) -> u8 {
        self as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::{testing, Palette};

    #[test]
    fn every_role_resolves_to_a_usable_font() {
        testing::with_theme(Palette::DARK, |t| {
            for role in [FontRole::Body, FontRole::Strong, FontRole::Small, FontRole::Title] {
                assert!(role.line_height(t) > 0, "{role:?} resolved to a font with no height");
                // The test atlas is one glyph with a fixed advance, so width is proportional to
                // length. Anything else means the role resolved to nothing at all.
                assert_eq!(role.measure(t, "aa"), role.measure(t, "a") * 2);
            }
        });
    }

    #[test]
    fn the_roles_are_told_apart_by_their_tags() {
        // A hash that gave two roles the same byte would let a heading keep a caption's measured
        // height when only the role changed — the one property change a text hash exists to catch.
        let tags = [
            FontRole::Body.tag(),
            FontRole::Strong.tag(),
            FontRole::Small.tag(),
            FontRole::Title.tag(),
        ];
        for (i, a) in tags.iter().enumerate() {
            for b in &tags[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn the_default_is_body() {
        assert_eq!(FontRole::default(), FontRole::Body);
    }
}
