//! Where a run of items breaks when it is wider than the room it has.
//!
//! [`crate::list`] stacks rows down a screen and [`crate::grid`] divides a band into fixed columns.
//! Neither describes a row of chips, a set of tags, or a keypad legend: items of unlike widths, laid
//! along a line, wrapping onto another line when the first runs out. That is a third shape, and it
//! is the one this module answers.
//!
//! # Why it is a state machine and not a function returning lines
//!
//! Wrapping has to be computed **twice** — once to find out how tall the block is, and again to
//! place each item — and the two must agree exactly. A function that returned a list of lines would
//! be called twice and allocate twice; worse, a caller that walked its children in a slightly
//! different order the second time would get a different answer and nothing would say so.
//!
//! [`Packer`] is fed the items in order and answers, for each one, which line it landed on and where
//! along that line. Same feed, same answers, no allocation, and the agreement is structural: there
//! is one rule and both passes walk it the same way. It is the shape
//! [`Font::wrap`](symbian_gfx::Font::wrap) already uses for the same problem one level down.
//!
//! # The first item on a line never breaks
//!
//! An item wider than the whole line has to go *somewhere*. Breaking before it would open a fresh
//! line that it also does not fit on, and the loop would either never terminate or emit an empty
//! line for every over-wide item. So the first item on a line is placed regardless and left to be
//! clipped by whoever is drawing — which is the same choice `Font::wrap` makes for a word longer
//! than its column.

/// Where one item landed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    /// Which line, counting from zero.
    pub line: usize,
    /// How far along that line the item starts, in pixels from the line's own origin.
    pub offset: i32,
}

/// Breaks a run of items into lines, one item at a time.
///
/// Holds no items and no sizes: it is told a width and answers a position, so the caller keeps
/// owning whatever it is laying out. That is what lets the measure pass feed it freshly measured
/// sizes and the placement pass feed it cached ones, and get the same lines from both.
#[derive(Copy, Clone, Debug)]
pub struct Packer {
    /// How much room a line has. A non-positive limit puts every item on its own line, which is the
    /// honest answer for a group squeezed to nothing rather than a division by zero.
    limit: i32,
    /// Space between two items on the same line. Never applied before the first.
    gap: i32,
    /// How far along the current line the next item would start, gap included.
    cursor: i32,
    line: usize,
    /// Whether the next item is the first on its line — the one that may not break.
    at_line_start: bool,
}

impl Packer {
    pub fn new(limit: i32, gap: i32) -> Self {
        Self { limit: limit.max(0), gap: gap.max(0), cursor: 0, line: 0, at_line_start: true }
    }

    /// Place an item `main` pixels wide, breaking onto a new line first if it will not fit.
    pub fn place(&mut self, main: i32) -> Placed {
        let main = main.max(0);
        if !self.at_line_start && self.cursor + self.gap + main > self.limit {
            self.line += 1;
            self.cursor = 0;
            self.at_line_start = true;
        }
        let offset = if self.at_line_start { 0 } else { self.cursor + self.gap };
        self.cursor = offset + main;
        self.at_line_start = false;
        Placed { line: self.line, offset }
    }

    /// How far the current line reaches — the width of the widest item run so far *on this line*.
    ///
    /// Read after a [`place`](Self::place) to grow a line's extent, and read again after the line
    /// number advances to close the previous one off.
    pub fn line_extent(&self) -> i32 {
        self.cursor
    }

    /// Which line the last placed item went on.
    pub fn line(&self) -> usize {
        self.line
    }

    /// How many lines have been opened. One before anything is placed, because an empty block is one
    /// empty line rather than none — a group with no children still has a cross extent of zero, and
    /// counting zero lines would make the gap arithmetic below subtract one from it.
    pub fn lines(&self) -> usize {
        self.line + 1
    }
}

/// Total cross extent of `lines` lines, given each line's own extent and the space between them.
///
/// Trivial, and here rather than at each call site because the off-by-one is not: the gap goes
/// *between* lines, so `n` lines have `n - 1` gaps, and a block of one line has none. Written out
/// twice — in a measure pass and a placement pass — it is written differently once.
pub fn stack_extent(line_extents: &[i32], gap: i32) -> i32 {
    let sum: i32 = line_extents.iter().map(|e| e.max(&0)).sum();
    sum + gap.max(0) * (line_extents.len() as i32 - 1).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `widths` to a fresh packer and collect where each one landed.
    fn pack(widths: &[i32], limit: i32, gap: i32) -> Vec<Placed> {
        let mut p = Packer::new(limit, gap);
        widths.iter().map(|w| p.place(*w)).collect()
    }

    #[test]
    fn items_that_fit_stay_on_one_line_with_the_gap_between_them() {
        let out = pack(&[20, 30, 10], 100, 4);
        assert_eq!(out[0], Placed { line: 0, offset: 0 });
        assert_eq!(out[1], Placed { line: 0, offset: 24 });
        assert_eq!(out[2], Placed { line: 0, offset: 58 });
    }

    #[test]
    fn the_gap_is_never_applied_before_the_first_item_on_a_line() {
        // On either line. A leading gap on the second line would indent every wrapped row by the
        // gap, which reads as a deliberate hanging indent and is not one.
        let out = pack(&[60, 60], 100, 8);
        assert_eq!(out[0], Placed { line: 0, offset: 0 });
        assert_eq!(out[1], Placed { line: 1, offset: 0 });
    }

    #[test]
    fn the_gap_counts_against_the_limit_and_not_only_the_items() {
        // 48 + 48 is 96 and fits in 100; with the 8-pixel join it is 104 and does not. A packer that
        // measured only the items would overflow the line by exactly one gap, every time.
        assert_eq!(pack(&[48, 48], 100, 8)[1].line, 1);
        assert_eq!(pack(&[48, 48], 100, 0)[1].line, 0);
    }

    #[test]
    fn an_item_wider_than_the_line_is_placed_anyway_rather_than_looping() {
        // Breaking before it would open a line it also does not fit on. It goes down and is left to
        // be clipped, which is what `Font::wrap` does with a word longer than its column.
        let out = pack(&[500], 100, 4);
        assert_eq!(out[0], Placed { line: 0, offset: 0 });

        // And it does not drag its neighbour onto its own overflowing line.
        let out = pack(&[10, 500, 10], 100, 4);
        assert_eq!(out[0].line, 0);
        assert_eq!(out[1].line, 1, "the wide item breaks first");
        assert_eq!(out[1].offset, 0);
        assert_eq!(out[2].line, 2, "and nothing joins it");
    }

    #[test]
    fn a_line_with_no_room_at_all_gives_every_item_its_own() {
        // A group squeezed to nothing. One item per line is wrong-looking and finite; a division by
        // the limit would not be.
        let out = pack(&[10, 10, 10], 0, 0);
        assert_eq!(out.iter().map(|p| p.line).collect::<Vec<_>>(), vec![0, 1, 2]);
    }

    #[test]
    fn a_negative_limit_is_no_room_rather_than_a_rect_turned_inside_out() {
        let out = pack(&[10, 10], -50, 4);
        assert_eq!(out[1].line, 1);
    }

    #[test]
    fn a_zero_width_item_takes_a_place_without_taking_room() {
        // A chip whose label came back empty still exists in the run, and must not be silently
        // dropped or the items after it shift by a gap.
        let out = pack(&[0, 10], 100, 4);
        assert_eq!(out[0], Placed { line: 0, offset: 0 });
        assert_eq!(out[1], Placed { line: 0, offset: 4 });
    }

    #[test]
    fn the_same_feed_gives_the_same_lines_twice() {
        // The property the whole design rests on: a measure pass and a placement pass agree because
        // there is one rule and both walk it in the same order.
        let widths = [30, 40, 25, 60, 15, 90];
        assert_eq!(pack(&widths, 100, 5), pack(&widths, 100, 5));
    }

    #[test]
    fn a_packer_reports_one_line_before_anything_is_placed() {
        // Not zero: an empty block is one empty line, and `stack_extent` on zero lines would
        // subtract a gap that is not there.
        let p = Packer::new(100, 4);
        assert_eq!(p.lines(), 1);
        assert_eq!(p.line_extent(), 0);
    }

    #[test]
    fn the_line_extent_grows_with_the_line_and_resets_when_it_breaks() {
        let mut p = Packer::new(100, 4);
        p.place(20);
        assert_eq!(p.line_extent(), 20);
        p.place(30);
        assert_eq!(p.line_extent(), 54, "the gap is part of what the line occupies");
        p.place(60);
        assert_eq!(p.line(), 1);
        assert_eq!(p.line_extent(), 60, "a fresh line starts from its own origin");
    }

    #[test]
    fn stacking_puts_the_gap_between_lines_and_not_after_the_last() {
        assert_eq!(stack_extent(&[10, 10, 10], 4), 38);
        assert_eq!(stack_extent(&[10], 4), 10, "one line has no join");
        assert_eq!(stack_extent(&[], 4), 0);
    }

    #[test]
    fn stacking_ignores_a_negative_line_and_a_negative_gap() {
        assert_eq!(stack_extent(&[10, -5, 10], 0), 20);
        assert_eq!(stack_extent(&[10, 10], -4), 20);
    }
}
