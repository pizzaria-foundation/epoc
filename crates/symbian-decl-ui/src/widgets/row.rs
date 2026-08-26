//! Children side by side.

use crate::layout::Axis;
use crate::widgets::Group;

/// A horizontal line of children.
///
/// `Row::new()` builds a [`Group`] rather than a type of its own, because a row and a column differ
/// by one field and nothing else. See [`Group`] for the builder — `.gap()`, `.pad()`, `.child()`,
/// `.group()`, `.fill()` — and [`crate::layout`] for how the space is divided.
///
/// ```ignore
/// Row::new().gap(6).pad(4)
///     .child(Avatar::new("JP"))
///     .child(Text::new(&chat.name).fill(1))   // takes what the avatar and the badge leave
///     .child(Badge::new(chat.unread))
/// ```
pub struct Row;

impl Row {
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> Group {
        Group::new(Axis::Horizontal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::UiCache;
    use crate::constraints::Constraints;
    use crate::layout::{layout_tree, measure_tree};
    use crate::widgets::{Node, Spacer};
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
    fn a_row_runs_left_to_right() {
        let root = Node::Group(
            Row::new()
                .child(Spacer::new().width(10).height(4))
                .child(Spacer::new().width(20).height(6)),
        );
        let cache = laid_out(&root, Rect::from_xywh(0, 0, 100, 20));
        assert_eq!(cache.rect(1), Some(Rect::from_xywh(0, 0, 10, 4)));
        assert_eq!(cache.rect(2), Some(Rect::from_xywh(10, 0, 20, 6)));
    }

    #[test]
    fn a_row_divides_width_and_leaves_height_alone() {
        // The axis is the whole difference between this file and `column.rs`: a row shares out `w`
        // and lets each child keep the `h` it asked for.
        let root = Node::Group(
            Row::new()
                .child(Spacer::new().fill(1).height(4))
                .child(Spacer::new().fill(1).height(9)),
        );
        let cache = laid_out(&root, Rect::from_xywh(0, 0, 100, 20));
        assert_eq!(cache.rect(1), Some(Rect::from_xywh(0, 0, 50, 4)));
        assert_eq!(cache.rect(2), Some(Rect::from_xywh(50, 0, 50, 9)));
    }

    #[test]
    fn a_wrapping_row_is_as_wide_as_its_children_and_as_tall_as_the_tallest() {
        testing::with_theme(Palette::DARK, |t| {
            let mut cache = UiCache::new();
            let root = Node::Group(
                Row::new()
                    .gap(2)
                    .child(Spacer::new().width(10).height(4))
                    .child(Spacer::new().width(20).height(9)),
            );
            assert_eq!(
                measure_tree(&root, Constraints::loose(100, 50), t, &mut cache),
                Size::new(32, 9)
            );
        });
    }

    #[test]
    fn a_filling_spacer_pushes_the_rest_to_the_far_edge() {
        // The idiom this exists for: label on the left, timestamp on the right, no arithmetic.
        let root = Node::Group(
            Row::new()
                .child(Spacer::new().width(10).height(4))
                .child(Spacer::new().fill(1))
                .child(Spacer::new().width(15).height(4)),
        );
        let cache = laid_out(&root, Rect::from_xywh(0, 0, 100, 20));
        assert_eq!(cache.rect(3).unwrap().x1, 100);
        assert_eq!(cache.rect(3).unwrap().x0, 85);
    }

    #[test]
    fn a_row_starts_where_it_was_put_not_at_the_origin() {
        // Rects are absolute: a row nested three deep still reports screen coordinates, because
        // that is what the canvas and the key dispatch both take.
        let root = Node::Group(Row::new().child(Spacer::new().width(10).height(4)));
        let cache = laid_out(&root, Rect::from_xywh(17, 23, 100, 20));
        assert_eq!(cache.rect(1), Some(Rect::from_xywh(17, 23, 10, 4)));
    }
}
