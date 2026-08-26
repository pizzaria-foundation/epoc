//! Children stacked downwards.

use crate::layout::Axis;
use crate::widgets::Group;

/// A vertical line of children.
///
/// The same [`Group`] as [`Row`](crate::widgets::Row) with the axis flipped — see [`crate::layout`]
/// for why that is one type and not two. This is the outermost shape of nearly every screen: a
/// header that wraps, a body that fills, a bar that wraps.
///
/// ```ignore
/// Column::new()
///     .child(TitleBar::new("Recent"))
///     .group(Row::new().fill(1).child(ScrollList::new(&rows)))
///     .child(SoftkeyBar::new(&keys))
/// ```
pub struct Column;

impl Column {
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> Group {
        Group::new(Axis::Vertical)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::UiCache;
    use crate::constraints::Constraints;
    use crate::layout::{layout_tree, measure_tree};
    use crate::widgets::{Node, Row, Spacer};
    use symbian_gfx::{Rect, Size};
    use symbian_ui::{testing, Palette};

    fn laid_out(root: &Node, area: Rect) -> UiCache {
        testing::with_theme(Palette::DARK, |t| {
            let mut cache = UiCache::new();
            cache.begin_frame();
            measure_tree(root, Constraints::tight(area.width(), area.height()), t, &mut cache);
            layout_tree(root, area, &mut cache, t);
            cache
        })
    }

    #[test]
    fn a_column_runs_top_to_bottom() {
        let root = Node::Group(
            Column::new()
                .child(Spacer::new().width(10).height(4))
                .child(Spacer::new().width(20).height(6)),
        );
        let cache = laid_out(&root, Rect::from_xywh(0, 0, 100, 40));
        assert_eq!(cache.rect(1), Some(Rect::from_xywh(0, 0, 10, 4)));
        assert_eq!(cache.rect(2), Some(Rect::from_xywh(0, 4, 20, 6)));
    }

    #[test]
    fn a_column_divides_height_and_leaves_width_alone() {
        let root = Node::Group(
            Column::new()
                .child(Spacer::new().fill(1).width(4))
                .child(Spacer::new().fill(1).width(9)),
        );
        let cache = laid_out(&root, Rect::from_xywh(0, 0, 100, 40));
        assert_eq!(cache.rect(1), Some(Rect::from_xywh(0, 0, 4, 20)));
        assert_eq!(cache.rect(2), Some(Rect::from_xywh(0, 20, 9, 20)));
    }

    #[test]
    fn gaps_run_down_the_column_and_come_out_before_the_division() {
        // The transposed twin of the row test. If the engine ever grew a second copy of the
        // arithmetic, this is the assertion that would catch it: the gap must leave the *height*
        // alone to divide, not the width.
        let root = Node::Group(
            Column::new().gap(10).child(Spacer::new().fill(1)).child(Spacer::new().fill(1)),
        );
        let cache = laid_out(&root, Rect::from_xywh(0, 0, 100, 100));
        assert_eq!(cache.rect(1).unwrap().height(), 45);
        assert_eq!(cache.rect(2).unwrap().height(), 45);
        assert_eq!(cache.rect(2).unwrap().y0, 55);
        assert_eq!(cache.rect(2).unwrap().y1, 100);
    }

    #[test]
    fn the_shape_of_a_screen() {
        // Header wraps, body fills, bar wraps — three bands, and the body gets exactly what the
        // other two do not want. Getting this wrong by a pixel is what makes a softkey bar sit one
        // row too low for a whole release.
        let root = Node::Group(
            Column::new()
                .child(Spacer::new().height(18).fill(0))
                .group(Row::new().fill(1).stretch_width())
                .child(Spacer::new().height(17)),
        );
        let cache = laid_out(&root, Rect::from_xywh(0, 0, 320, 240));
        assert_eq!(cache.rect(1).unwrap().height(), 18);
        assert_eq!(cache.rect(2).unwrap(), Rect::from_xywh(0, 18, 320, 205));
        assert_eq!(cache.rect(3).unwrap().y0, 223);
        assert_eq!(cache.rect(3).unwrap().y1, 240);
    }

    #[test]
    fn a_wrapping_column_is_as_tall_as_its_children_and_as_wide_as_the_widest() {
        testing::with_theme(Palette::DARK, |t| {
            let mut cache = UiCache::new();
            let root = Node::Group(
                Column::new()
                    .gap(2)
                    .child(Spacer::new().width(10).height(4))
                    .child(Spacer::new().width(20).height(9)),
            );
            assert_eq!(
                measure_tree(&root, Constraints::loose(100, 50), t, &mut cache),
                Size::new(20, 15)
            );
        });
    }
}
