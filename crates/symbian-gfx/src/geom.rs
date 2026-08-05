//! Integer geometry.
//!
//! Coordinates are `i32` and signed on purpose: scrolled content and widgets
//! parked off-screen are the normal case on a 320x240 display, and clamping
//! them to unsigned at the API boundary just moves the bugs somewhere less
//! visible.

use core::ops::{Add, AddAssign, Sub, SubAssign};

#[derive(Copy, Clone, PartialEq, Eq, Default, Debug, Hash)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const ZERO: Self = Self { x: 0, y: 0 };

    #[inline]
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

impl Add for Point {
    type Output = Self;
    #[inline]
    fn add(self, o: Self) -> Self {
        Self::new(self.x + o.x, self.y + o.y)
    }
}

impl Sub for Point {
    type Output = Self;
    #[inline]
    fn sub(self, o: Self) -> Self {
        Self::new(self.x - o.x, self.y - o.y)
    }
}

impl AddAssign for Point {
    #[inline]
    fn add_assign(&mut self, o: Self) {
        *self = *self + o;
    }
}

impl SubAssign for Point {
    #[inline]
    fn sub_assign(&mut self, o: Self) {
        *self = *self - o;
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Default, Debug, Hash)]
pub struct Size {
    pub w: i32,
    pub h: i32,
}

impl Size {
    pub const ZERO: Self = Self { w: 0, h: 0 };

    #[inline]
    pub const fn new(w: i32, h: i32) -> Self {
        Self { w, h }
    }

    #[inline]
    pub const fn is_empty(self) -> bool {
        self.w <= 0 || self.h <= 0
    }
}

/// A half-open rectangle: `x0..x1` by `y0..y1`. The right and bottom edges are
/// exclusive, so `width == x1 - x0` with no off-by-one corrections anywhere.
#[derive(Copy, Clone, PartialEq, Eq, Default, Debug, Hash)]
pub struct Rect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl Rect {
    pub const EMPTY: Self = Self { x0: 0, y0: 0, x1: 0, y1: 0 };

    #[inline]
    pub const fn new(x0: i32, y0: i32, x1: i32, y1: i32) -> Self {
        Self { x0, y0, x1, y1 }
    }

    #[inline]
    pub const fn from_xywh(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x0: x, y0: y, x1: x + w, y1: y + h }
    }

    #[inline]
    pub const fn from_origin_size(o: Point, s: Size) -> Self {
        Self::from_xywh(o.x, o.y, s.w, s.h)
    }

    /// A rect anchored at the origin. Handy for "the whole surface".
    #[inline]
    pub const fn from_size(s: Size) -> Self {
        Self::from_xywh(0, 0, s.w, s.h)
    }

    #[inline]
    pub const fn width(self) -> i32 {
        self.x1 - self.x0
    }

    #[inline]
    pub const fn height(self) -> i32 {
        self.y1 - self.y0
    }

    #[inline]
    pub const fn size(self) -> Size {
        Size::new(self.width(), self.height())
    }

    #[inline]
    pub const fn origin(self) -> Point {
        Point::new(self.x0, self.y0)
    }

    /// True when the rect covers no pixels. Note that a rect can be "inverted"
    /// (`x1 < x0`) after an empty intersection; every consumer must treat that
    /// as empty rather than assume normalisation.
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    #[inline]
    pub fn intersect(self, o: Self) -> Self {
        Self {
            x0: self.x0.max(o.x0),
            y0: self.y0.max(o.y0),
            x1: self.x1.min(o.x1),
            y1: self.y1.min(o.y1),
        }
    }

    /// Smallest rect containing both. An empty operand is ignored rather than
    /// dragging the result out to include its bogus coordinates.
    #[inline]
    pub fn union(self, o: Self) -> Self {
        if self.is_empty() {
            return o;
        }
        if o.is_empty() {
            return self;
        }
        Self {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }

    #[inline]
    pub fn contains(self, p: Point) -> bool {
        p.x >= self.x0 && p.x < self.x1 && p.y >= self.y0 && p.y < self.y1
    }

    #[inline]
    pub fn intersects(self, o: Self) -> bool {
        !self.intersect(o).is_empty()
    }

    #[inline]
    pub fn translate(self, d: Point) -> Self {
        Self { x0: self.x0 + d.x, y0: self.y0 + d.y, x1: self.x1 + d.x, y1: self.y1 + d.y }
    }

    /// Shrink on every side. Negative values grow it.
    #[inline]
    pub fn inset(self, by: i32) -> Self {
        self.inset_xy(by, by)
    }

    #[inline]
    pub fn inset_xy(self, x: i32, y: i32) -> Self {
        Self { x0: self.x0 + x, y0: self.y0 + y, x1: self.x1 - x, y1: self.y1 - y }
    }

    #[inline]
    pub fn inset_edges(self, e: Edges) -> Self {
        Self {
            x0: self.x0 + e.left,
            y0: self.y0 + e.top,
            x1: self.x1 - e.right,
            y1: self.y1 - e.bottom,
        }
    }

    /// Split `amount` pixels off the top, returning `(strip, remainder)`.
    /// Used pervasively by the layout code to carve out title and softkey bars.
    #[inline]
    pub fn split_top(self, amount: i32) -> (Self, Self) {
        let cut = (self.y0 + amount).min(self.y1);
        (Self { y1: cut, ..self }, Self { y0: cut, ..self })
    }

    #[inline]
    pub fn split_bottom(self, amount: i32) -> (Self, Self) {
        let cut = (self.y1 - amount).max(self.y0);
        (Self { y0: cut, ..self }, Self { y1: cut, ..self })
    }

    #[inline]
    pub fn split_left(self, amount: i32) -> (Self, Self) {
        let cut = (self.x0 + amount).min(self.x1);
        (Self { x1: cut, ..self }, Self { x0: cut, ..self })
    }

    #[inline]
    pub fn split_right(self, amount: i32) -> (Self, Self) {
        let cut = (self.x1 - amount).max(self.x0);
        (Self { x0: cut, ..self }, Self { x1: cut, ..self })
    }
}

/// Per-side insets: padding, margins, border widths.
#[derive(Copy, Clone, PartialEq, Eq, Default, Debug)]
pub struct Edges {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Edges {
    pub const ZERO: Self = Self::all(0);

    #[inline]
    pub const fn all(v: i32) -> Self {
        Self { left: v, top: v, right: v, bottom: v }
    }

    #[inline]
    pub const fn xy(x: i32, y: i32) -> Self {
        Self { left: x, top: y, right: x, bottom: y }
    }

    #[inline]
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self { left, top, right, bottom }
    }

    #[inline]
    pub const fn horizontal(self) -> i32 {
        self.left + self.right
    }

    #[inline]
    pub const fn vertical(self) -> i32 {
        self.top + self.bottom
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_intersection_reads_as_empty_even_when_inverted() {
        let a = Rect::from_xywh(0, 0, 10, 10);
        let b = Rect::from_xywh(50, 50, 10, 10);
        let i = a.intersect(b);
        assert!(i.is_empty());
        // Deliberately inverted rather than normalised; is_empty() is the guard.
        assert!(i.x1 < i.x0);
    }

    #[test]
    fn union_ignores_empty_operands() {
        let a = Rect::from_xywh(4, 4, 2, 2);
        assert_eq!(a.union(Rect::EMPTY), a);
        assert_eq!(Rect::EMPTY.union(a), a);
    }

    #[test]
    fn splits_clamp_instead_of_inverting() {
        let r = Rect::from_xywh(0, 0, 100, 20);
        let (strip, rest) = r.split_top(50);
        assert_eq!(strip, Rect::new(0, 0, 100, 20));
        assert!(rest.is_empty());

        let (bottom, rest) = r.split_bottom(50);
        assert_eq!(bottom, Rect::new(0, 0, 100, 20));
        assert!(rest.is_empty());
    }

    #[test]
    fn split_top_partitions_exactly() {
        let r = Rect::from_xywh(3, 7, 100, 40);
        let (a, b) = r.split_top(12);
        assert_eq!(a.height(), 12);
        assert_eq!(b.height(), 28);
        assert_eq!(a.y1, b.y0);
        assert_eq!(a.union(b), r);
    }

    #[test]
    fn contains_excludes_far_edges() {
        let r = Rect::from_xywh(0, 0, 4, 4);
        assert!(r.contains(Point::new(0, 0)));
        assert!(r.contains(Point::new(3, 3)));
        assert!(!r.contains(Point::new(4, 3)));
        assert!(!r.contains(Point::new(3, 4)));
    }
}
