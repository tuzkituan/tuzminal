//! The binary space partition tree that arranges panes within one tab.
//!
//! Every interior node splits its rectangle in two along an [`Axis`] at a
//! `ratio`; every leaf is a pane. This is the same model tiling window managers
//! use, and it gives arbitrary nesting for free: splitting a pane replaces its
//! leaf with a split whose children are the original pane and the new one.

use crate::geom::{Axis, Direction, Rect};

/// Opaque handle for a pane. Allocated by [`crate::Layout`] and never reused, so
/// a stale id can never silently refer to a different pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneId(pub u32);

impl std::fmt::Display for PaneId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pane{}", self.0)
    }
}

/// Which child of a split to descend into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Branch {
    First,
    Second,
}

/// Route from the tree root to a particular node.
pub type SplitPath = Vec<Branch>;

/// A split may never be dragged past this fraction, so neither side can be
/// collapsed to nothing and become impossible to grab again.
const MIN_RATIO: f32 = 0.05;
const MAX_RATIO: f32 = 0.95;

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Leaf(PaneId),
    Split {
        axis: Axis,
        /// Fraction of the available space given to `first`, in `0.05..=0.95`.
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

/// A pane's computed position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneRect {
    pub pane: PaneId,
    /// The pane's full extent, including its internal padding.
    pub rect: Rect,
}

/// A divider between two panes, for drawing and for drag hit-testing.
#[derive(Debug, Clone, PartialEq)]
pub struct DividerRect {
    pub rect: Rect,
    /// `Horizontal` here means the split arranges its children side by side, so
    /// the divider itself is a vertical bar dragged left and right.
    pub axis: Axis,
    /// Which split this divider belongs to, so a drag can adjust its ratio.
    pub path: SplitPath,
}

impl Node {
    pub const fn leaf(id: PaneId) -> Self {
        Node::Leaf(id)
    }

    /// True when this node is exactly `Leaf(target)`.
    fn is_leaf_of(&self, target: PaneId) -> bool {
        matches!(self, Node::Leaf(id) if *id == target)
    }

    /// Every pane in the subtree, in left-to-right, top-to-bottom tree order.
    pub fn leaves(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect_leaves(&mut out);
        out
    }

    fn collect_leaves(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split { first, second, .. } => {
                first.collect_leaves(out);
                second.collect_leaves(out);
            }
        }
    }

    pub fn pane_count(&self) -> usize {
        match self {
            Node::Leaf(_) => 1,
            Node::Split { first, second, .. } => first.pane_count() + second.pane_count(),
        }
    }

    pub fn contains(&self, target: PaneId) -> bool {
        match self {
            Node::Leaf(id) => *id == target,
            Node::Split { first, second, .. } => first.contains(target) || second.contains(target),
        }
    }

    /// The first pane in tree order — used to pick a new focus after a close.
    pub fn first_leaf(&self) -> PaneId {
        match self {
            Node::Leaf(id) => *id,
            Node::Split { first, .. } => first.first_leaf(),
        }
    }

    /// Replace `target`'s leaf with a split containing `target` and `new_pane`.
    ///
    /// `dir` decides both the axis and which side the new pane lands on, so
    /// `split_right` puts it to the right and `split_left` to the left.
    /// Returns false when `target` is not in this subtree.
    pub fn split_leaf(&mut self, target: PaneId, new_pane: PaneId, dir: Direction) -> bool {
        match self {
            Node::Leaf(id) if *id == target => {
                let (first, second) = if dir.is_forward() {
                    (Node::Leaf(target), Node::Leaf(new_pane))
                } else {
                    (Node::Leaf(new_pane), Node::Leaf(target))
                };
                *self = Node::Split {
                    axis: dir.axis(),
                    ratio: 0.5,
                    first: Box::new(first),
                    second: Box::new(second),
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { first, second, .. } => {
                first.split_leaf(target, new_pane, dir) || second.split_leaf(target, new_pane, dir)
            }
        }
    }

    /// Remove `target`, collapsing its parent split into the surviving sibling.
    ///
    /// Returns false when `target` is absent, or when it is the subtree root —
    /// a lone leaf has no parent to collapse, so the caller decides what that
    /// means (for a tab, closing it).
    pub fn remove_leaf(&mut self, target: PaneId) -> bool {
        match self {
            Node::Leaf(_) => false,
            Node::Split { first, second, .. } => {
                let first_matches = first.is_leaf_of(target);
                let second_matches = second.is_leaf_of(target);

                if first_matches || second_matches {
                    // Take ownership of the whole split so the surviving child
                    // can be moved out of its Box without cloning.
                    let placeholder = Node::Leaf(target);
                    let old = std::mem::replace(self, placeholder);
                    let Node::Split { first, second, .. } = old else {
                        unreachable!("matched Split immediately above")
                    };
                    *self = if first_matches { *second } else { *first };
                    true
                } else {
                    first.remove_leaf(target) || second.remove_leaf(target)
                }
            }
        }
    }

    /// Route from this node down to `target`.
    pub fn path_to(&self, target: PaneId) -> Option<SplitPath> {
        let mut path = Vec::new();
        if self.build_path(target, &mut path) {
            Some(path)
        } else {
            None
        }
    }

    fn build_path(&self, target: PaneId, path: &mut SplitPath) -> bool {
        match self {
            Node::Leaf(id) => *id == target,
            Node::Split { first, second, .. } => {
                path.push(Branch::First);
                if first.build_path(target, path) {
                    return true;
                }
                path.pop();

                path.push(Branch::Second);
                if second.build_path(target, path) {
                    return true;
                }
                path.pop();
                false
            }
        }
    }

    fn at_path_mut(&mut self, path: &[Branch]) -> Option<&mut Node> {
        let mut node = self;
        for step in path {
            match node {
                Node::Split { first, second, .. } => {
                    node = match step {
                        Branch::First => first,
                        Branch::Second => second,
                    };
                }
                Node::Leaf(_) => return None,
            }
        }
        Some(node)
    }

    /// Nudge the ratio of the split at `path` by `delta`, clamped so neither
    /// side collapses. Returns the applied ratio.
    pub fn adjust_ratio_at(&mut self, path: &[Branch], delta: f32) -> Option<f32> {
        match self.at_path_mut(path)? {
            Node::Split { ratio, .. } => {
                *ratio = (*ratio + delta).clamp(MIN_RATIO, MAX_RATIO);
                Some(*ratio)
            }
            Node::Leaf(_) => None,
        }
    }

    /// Set the ratio of the split at `path` directly, clamped.
    pub fn set_ratio_at(&mut self, path: &[Branch], ratio: f32) -> Option<f32> {
        match self.at_path_mut(path)? {
            Node::Split { ratio: r, .. } => {
                *r = ratio.clamp(MIN_RATIO, MAX_RATIO);
                Some(*r)
            }
            Node::Leaf(_) => None,
        }
    }

    /// Move the divider bounding `pane` along `dir` by `delta`.
    ///
    /// The semantic is *the boundary moves the way you pressed*: `Right` always
    /// shifts the relevant divider rightward, whichever side of it the focused
    /// pane is on. So the same keystroke grows the left pane of a split and
    /// shrinks the right one. The alternative — "always grow the focused pane" —
    /// reads better in a sentence but means the key does nothing whenever the
    /// pane has no divider on that side, which feels broken in use.
    ///
    /// Increasing a ratio always moves that split's divider forward (right or
    /// down), so the sign depends only on `dir`, never on which branch the pane
    /// occupies.
    ///
    /// Walks up from the pane to the nearest ancestor split on the matching axis.
    /// Without that search, resizing a deeply nested pane would either do nothing
    /// or move an unrelated divider.
    pub fn resize_pane(&mut self, pane: PaneId, dir: Direction, delta: f32) -> Option<f32> {
        let path = self.path_to(pane)?;
        let target_axis = dir.axis();
        let signed = if dir.is_forward() { delta } else { -delta };

        // Deepest ancestor first: that is the divider closest to the pane, and
        // the one the user means.
        for depth in (0..path.len()).rev() {
            let ancestor = &path[..depth];
            let is_split_on_axis = matches!(
                self.at_path_mut(ancestor),
                Some(Node::Split { axis, .. }) if *axis == target_axis
            );
            if is_split_on_axis {
                return self.adjust_ratio_at(ancestor, signed);
            }
        }
        None
    }

    /// Compute pane and divider rectangles for `area`.
    pub fn layout(&self, area: Rect, divider_width: u32) -> (Vec<PaneRect>, Vec<DividerRect>) {
        let mut panes = Vec::new();
        let mut dividers = Vec::new();
        let mut path = Vec::new();
        self.layout_into(area, divider_width, &mut path, &mut panes, &mut dividers);
        (panes, dividers)
    }

    fn layout_into(
        &self,
        area: Rect,
        divider_width: u32,
        path: &mut SplitPath,
        panes: &mut Vec<PaneRect>,
        dividers: &mut Vec<DividerRect>,
    ) {
        match self {
            Node::Leaf(id) => panes.push(PaneRect {
                pane: *id,
                rect: area,
            }),
            Node::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let ratio = ratio.clamp(MIN_RATIO, MAX_RATIO);

                match axis {
                    Axis::Horizontal => {
                        // The divider consumes space before the ratio is
                        // applied, so the two panes plus the divider always sum
                        // to exactly the parent width with no rounding drift.
                        let avail = area.width.saturating_sub(divider_width);
                        let first_w = ((avail as f32) * ratio).round() as u32;
                        let first_w = first_w.min(avail);
                        let second_w = avail - first_w;
                        let divider_x = area.x + first_w as i32;

                        first.layout_child(
                            Rect::new(area.x, area.y, first_w, area.height),
                            divider_width,
                            Branch::First,
                            path,
                            panes,
                            dividers,
                        );
                        if divider_width > 0 {
                            dividers.push(DividerRect {
                                rect: Rect::new(divider_x, area.y, divider_width, area.height),
                                axis: *axis,
                                path: path.clone(),
                            });
                        }
                        second.layout_child(
                            Rect::new(
                                divider_x + divider_width as i32,
                                area.y,
                                second_w,
                                area.height,
                            ),
                            divider_width,
                            Branch::Second,
                            path,
                            panes,
                            dividers,
                        );
                    }
                    Axis::Vertical => {
                        let avail = area.height.saturating_sub(divider_width);
                        let first_h = ((avail as f32) * ratio).round() as u32;
                        let first_h = first_h.min(avail);
                        let second_h = avail - first_h;
                        let divider_y = area.y + first_h as i32;

                        first.layout_child(
                            Rect::new(area.x, area.y, area.width, first_h),
                            divider_width,
                            Branch::First,
                            path,
                            panes,
                            dividers,
                        );
                        if divider_width > 0 {
                            dividers.push(DividerRect {
                                rect: Rect::new(area.x, divider_y, area.width, divider_width),
                                axis: *axis,
                                path: path.clone(),
                            });
                        }
                        second.layout_child(
                            Rect::new(
                                area.x,
                                divider_y + divider_width as i32,
                                area.width,
                                second_h,
                            ),
                            divider_width,
                            Branch::Second,
                            path,
                            panes,
                            dividers,
                        );
                    }
                }
            }
        }
    }

    fn layout_child(
        &self,
        area: Rect,
        divider_width: u32,
        branch: Branch,
        path: &mut SplitPath,
        panes: &mut Vec<PaneRect>,
        dividers: &mut Vec<DividerRect>,
    ) {
        path.push(branch);
        self.layout_into(area, divider_width, path, panes, dividers);
        path.pop();
    }
}

/// Pick the pane to focus when moving `dir` from `from`.
///
/// Chosen geometrically rather than by tree structure: users expect focus to
/// follow what they see, and tree order diverges from screen order as soon as
/// splits nest. Candidates must lie beyond the source pane's leading edge; among
/// them the one sharing the most perpendicular overlap wins, with distance as
/// the tie-break.
pub fn focus_neighbor(panes: &[PaneRect], from: PaneId, dir: Direction) -> Option<PaneId> {
    let src = panes.iter().find(|p| p.pane == from)?.rect;

    panes
        .iter()
        .filter(|p| p.pane != from)
        .filter_map(|p| {
            let r = p.rect;
            // `>=` rather than `>` so a zero-width divider still separates.
            let (beyond, distance, overlap) = match dir {
                Direction::Right => (
                    r.x >= src.right(),
                    r.x - src.right(),
                    src.vertical_overlap(&r),
                ),
                Direction::Left => (
                    r.right() <= src.x,
                    src.x - r.right(),
                    src.vertical_overlap(&r),
                ),
                Direction::Down => (
                    r.y >= src.bottom(),
                    r.y - src.bottom(),
                    src.horizontal_overlap(&r),
                ),
                Direction::Up => (
                    r.bottom() <= src.y,
                    src.y - r.bottom(),
                    src.horizontal_overlap(&r),
                ),
            };
            // Requiring real overlap keeps focus from jumping diagonally to a
            // pane that merely happens to be further along the axis.
            (beyond && overlap > 0).then_some((p.pane, distance, overlap))
        })
        // Nearest first, then greatest overlap, then lowest id so the result is
        // deterministic for symmetric layouts.
        .min_by(|a, b| {
            a.1.cmp(&b.1)
                .then_with(|| b.2.cmp(&a.2))
                .then_with(|| a.0.cmp(&b.0))
        })
        .map(|(id, _, _)| id)
}

/// The pane whose rect contains the point, if any.
pub fn pane_at(panes: &[PaneRect], x: i32, y: i32) -> Option<PaneId> {
    panes.iter().find(|p| p.rect.contains(x, y)).map(|p| p.pane)
}

/// The divider within `tolerance` pixels of the point.
///
/// A one-pixel divider is impossible to hit with a mouse, so hit-testing is
/// deliberately more generous than the drawn width.
pub fn divider_at(
    dividers: &[DividerRect],
    x: i32,
    y: i32,
    tolerance: u32,
) -> Option<&DividerRect> {
    dividers.iter().find(|d| {
        let grab = match d.axis {
            Axis::Horizontal => d.rect.inset(0, 0).grow_x(tolerance),
            Axis::Vertical => d.rect.inset(0, 0).grow_y(tolerance),
        };
        grab.contains(x, y)
    })
}

impl Rect {
    /// Expand horizontally by `n` pixels on both sides.
    fn grow_x(self, n: u32) -> Rect {
        Rect {
            x: self.x - n as i32,
            y: self.y,
            width: self.width + n * 2,
            height: self.height,
        }
    }
    /// Expand vertically by `n` pixels on both sides.
    fn grow_y(self, n: u32) -> Rect {
        Rect {
            x: self.x,
            y: self.y - n as i32,
            width: self.width,
            height: self.height + n * 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(n: u32) -> PaneId {
        PaneId(n)
    }

    /// 200x100 area, no divider, so arithmetic in assertions stays obvious.
    const AREA: Rect = Rect::new(0, 0, 200, 100);

    #[test]
    fn a_lone_leaf_fills_the_area() {
        let t = Node::leaf(p(1));
        let (panes, dividers) = t.layout(AREA, 0);
        assert_eq!(
            panes,
            [PaneRect {
                pane: p(1),
                rect: AREA
            }]
        );
        assert!(dividers.is_empty());
    }

    #[test]
    fn splitting_right_puts_the_new_pane_on_the_right() {
        let mut t = Node::leaf(p(1));
        assert!(t.split_leaf(p(1), p(2), Direction::Right));

        let (panes, _) = t.layout(AREA, 0);
        assert_eq!(panes[0].pane, p(1));
        assert_eq!(panes[0].rect, Rect::new(0, 0, 100, 100));
        assert_eq!(panes[1].pane, p(2));
        assert_eq!(panes[1].rect, Rect::new(100, 0, 100, 100));
    }

    #[test]
    fn splitting_left_puts_the_new_pane_on_the_left() {
        let mut t = Node::leaf(p(1));
        t.split_leaf(p(1), p(2), Direction::Left);

        let (panes, _) = t.layout(AREA, 0);
        // Tree order now starts with the new pane.
        assert_eq!(panes[0].pane, p(2));
        assert_eq!(panes[0].rect.x, 0);
        assert_eq!(panes[1].pane, p(1));
        assert_eq!(panes[1].rect.x, 100);
    }

    #[test]
    fn splitting_down_stacks_panes() {
        let mut t = Node::leaf(p(1));
        t.split_leaf(p(1), p(2), Direction::Down);

        let (panes, _) = t.layout(AREA, 0);
        assert_eq!(panes[0].rect, Rect::new(0, 0, 200, 50));
        assert_eq!(panes[1].rect, Rect::new(0, 50, 200, 50));
    }

    #[test]
    fn splitting_an_unknown_pane_changes_nothing() {
        let mut t = Node::leaf(p(1));
        let before = t.clone();
        assert!(!t.split_leaf(p(99), p(2), Direction::Right));
        assert_eq!(t, before);
    }

    #[test]
    fn nested_splits_subdivide_only_their_own_pane() {
        let mut t = Node::leaf(p(1));
        t.split_leaf(p(1), p(2), Direction::Right); // 1 | 2
        t.split_leaf(p(2), p(3), Direction::Down); // 1 | (2 / 3)

        let (panes, _) = t.layout(AREA, 0);
        let get = |id: u32| panes.iter().find(|x| x.pane == p(id)).unwrap().rect;

        assert_eq!(get(1), Rect::new(0, 0, 100, 100), "pane 1 is untouched");
        assert_eq!(get(2), Rect::new(100, 0, 100, 50));
        assert_eq!(get(3), Rect::new(100, 50, 100, 50));
    }

    #[test]
    fn divider_width_is_taken_out_before_the_ratio_is_applied() {
        // The invariant that matters: children plus divider exactly fill the
        // parent, with no off-by-one seam or overlap.
        let mut t = Node::leaf(p(1));
        t.split_leaf(p(1), p(2), Direction::Right);

        let (panes, dividers) = t.layout(Rect::new(0, 0, 201, 100), 1);
        assert_eq!(panes[0].rect.width, 100);
        assert_eq!(dividers.len(), 1);
        assert_eq!(dividers[0].rect, Rect::new(100, 0, 1, 100));
        assert_eq!(panes[1].rect, Rect::new(101, 0, 100, 100));

        let total = panes[0].rect.width + dividers[0].rect.width + panes[1].rect.width;
        assert_eq!(total, 201);
    }

    #[test]
    fn odd_sizes_never_leave_a_gap_between_panes() {
        let mut t = Node::leaf(p(1));
        t.split_leaf(p(1), p(2), Direction::Right);
        for w in 3..64u32 {
            let (panes, dividers) = t.layout(Rect::new(0, 0, w, 10), 1);
            assert_eq!(
                panes[0].rect.right(),
                dividers[0].rect.x,
                "gap before divider at width {w}"
            );
            assert_eq!(
                dividers[0].rect.right(),
                panes[1].rect.x,
                "gap after divider at width {w}"
            );
            assert_eq!(panes[1].rect.right(), w as i32, "short of the edge at {w}");
        }
    }

    #[test]
    fn closing_a_pane_collapses_the_split_and_its_sibling_takes_the_space() {
        let mut t = Node::leaf(p(1));
        t.split_leaf(p(1), p(2), Direction::Right);
        assert!(t.remove_leaf(p(2)));

        assert_eq!(t, Node::leaf(p(1)));
        let (panes, _) = t.layout(AREA, 0);
        assert_eq!(panes[0].rect, AREA, "survivor must reclaim the full area");
    }

    #[test]
    fn closing_a_nested_pane_promotes_its_sibling_subtree() {
        let mut t = Node::leaf(p(1));
        t.split_leaf(p(1), p(2), Direction::Right); // 1 | 2
        t.split_leaf(p(2), p(3), Direction::Down); // 1 | (2 / 3)
        assert!(t.remove_leaf(p(2)));

        assert_eq!(t.leaves(), [p(1), p(3)]);
        let (panes, _) = t.layout(AREA, 0);
        let get = |id: u32| panes.iter().find(|x| x.pane == p(id)).unwrap().rect;
        // 3 inherits the whole right half, not just its old bottom quarter.
        assert_eq!(get(3), Rect::new(100, 0, 100, 100));
    }

    #[test]
    fn removing_the_root_leaf_is_refused_for_the_caller_to_handle() {
        let mut t = Node::leaf(p(1));
        assert!(
            !t.remove_leaf(p(1)),
            "a lone leaf has no parent to collapse"
        );
        assert_eq!(t, Node::leaf(p(1)));
    }

    #[test]
    fn removing_an_absent_pane_is_a_no_op() {
        let mut t = Node::leaf(p(1));
        t.split_leaf(p(1), p(2), Direction::Right);
        let before = t.clone();
        assert!(!t.remove_leaf(p(42)));
        assert_eq!(t, before);
    }

    #[test]
    fn path_locates_leaves_and_misses_absent_ones() {
        let mut t = Node::leaf(p(1));
        t.split_leaf(p(1), p(2), Direction::Right);
        t.split_leaf(p(2), p(3), Direction::Down);

        assert_eq!(t.path_to(p(1)), Some(vec![Branch::First]));
        assert_eq!(t.path_to(p(3)), Some(vec![Branch::Second, Branch::Second]));
        assert_eq!(t.path_to(p(9)), None);
    }

    #[test]
    fn ratios_clamp_so_a_pane_cannot_be_squeezed_away() {
        let mut t = Node::leaf(p(1));
        t.split_leaf(p(1), p(2), Direction::Right);

        assert_eq!(t.set_ratio_at(&[], 0.0), Some(MIN_RATIO));
        assert_eq!(t.set_ratio_at(&[], 1.0), Some(MAX_RATIO));
        assert_eq!(t.adjust_ratio_at(&[], -10.0), Some(MIN_RATIO));
    }

    #[test]
    fn resize_moves_the_divider_the_pane_actually_touches() {
        let mut t = Node::leaf(p(1));
        t.split_leaf(p(1), p(2), Direction::Right);

        // Growing the left pane rightward increases the split ratio.
        let r = t.resize_pane(p(1), Direction::Right, 0.1).unwrap();
        assert!((r - 0.6).abs() < 1e-6, "got {r}");

        // Growing the right pane leftward decreases it by the same amount.
        let r = t.resize_pane(p(2), Direction::Left, 0.1).unwrap();
        assert!((r - 0.5).abs() < 1e-6, "got {r}");
    }

    #[test]
    fn resize_skips_ancestors_on_the_wrong_axis() {
        // 1 | (2 / 3): pane 2's parent is a vertical split, but a horizontal
        // resize must reach past it to the outer split rather than doing nothing.
        let mut t = Node::leaf(p(1));
        t.split_leaf(p(1), p(2), Direction::Right);
        t.split_leaf(p(2), p(3), Direction::Down);

        let r = t.resize_pane(p(2), Direction::Left, 0.1).unwrap();
        assert!((r - 0.4).abs() < 1e-6, "outer ratio should shrink, got {r}");

        // And a vertical resize hits the inner split instead.
        let r = t.resize_pane(p(2), Direction::Down, 0.1).unwrap();
        assert!((r - 0.6).abs() < 1e-6, "inner ratio should grow, got {r}");
    }

    #[test]
    fn resize_returns_none_when_no_matching_divider_exists() {
        let mut t = Node::leaf(p(1));
        t.split_leaf(p(1), p(2), Direction::Right);
        // Only a horizontal split exists, so there is nothing to move vertically.
        assert_eq!(t.resize_pane(p(1), Direction::Down, 0.1), None);
        assert_eq!(t.resize_pane(p(99), Direction::Right, 0.1), None);
    }

    // --- focus navigation -------------------------------------------------

    /// Build `1 | (2 / 3)` and lay it out in a 200x100 area:
    /// pane 1 is the left half, 2 the top-right, 3 the bottom-right.
    fn three_pane_layout() -> Vec<PaneRect> {
        let mut t = Node::leaf(p(1));
        t.split_leaf(p(1), p(2), Direction::Right);
        t.split_leaf(p(2), p(3), Direction::Down);
        t.layout(AREA, 0).0
    }

    #[test]
    fn focus_moves_to_the_adjacent_pane() {
        let panes = three_pane_layout();
        // From the tall left pane, moving right must land on one of the stacked
        // panes; both overlap it, and the nearest-then-most-overlap rule picks
        // the top one deterministically.
        assert_eq!(focus_neighbor(&panes, p(1), Direction::Right), Some(p(2)));
        assert_eq!(focus_neighbor(&panes, p(2), Direction::Left), Some(p(1)));
        assert_eq!(focus_neighbor(&panes, p(3), Direction::Left), Some(p(1)));
        assert_eq!(focus_neighbor(&panes, p(2), Direction::Down), Some(p(3)));
        assert_eq!(focus_neighbor(&panes, p(3), Direction::Up), Some(p(2)));
    }

    #[test]
    fn focus_stops_at_the_edge_instead_of_wrapping() {
        let panes = three_pane_layout();
        assert_eq!(focus_neighbor(&panes, p(1), Direction::Left), None);
        assert_eq!(focus_neighbor(&panes, p(1), Direction::Up), None);
        assert_eq!(focus_neighbor(&panes, p(2), Direction::Right), None);
        assert_eq!(focus_neighbor(&panes, p(3), Direction::Down), None);
    }

    #[test]
    fn focus_never_jumps_diagonally() {
        // Two panes that share no edge: 1 top-left, 2 bottom-right. Moving right
        // from 1 must find nothing, because 2 has zero vertical overlap with it.
        let panes = vec![
            PaneRect {
                pane: p(1),
                rect: Rect::new(0, 0, 100, 50),
            },
            PaneRect {
                pane: p(2),
                rect: Rect::new(100, 50, 100, 50),
            },
        ];
        assert_eq!(focus_neighbor(&panes, p(1), Direction::Right), None);
        assert_eq!(focus_neighbor(&panes, p(1), Direction::Down), None);
    }

    #[test]
    fn focus_prefers_the_nearest_pane_over_a_more_overlapping_distant_one() {
        // 1 is at the left. 2 is immediately right but short; 3 is far right and
        // spans the full height. Nearest must win, or focus would skip a column.
        let panes = vec![
            PaneRect {
                pane: p(1),
                rect: Rect::new(0, 0, 50, 100),
            },
            PaneRect {
                pane: p(2),
                rect: Rect::new(50, 0, 50, 20),
            },
            PaneRect {
                pane: p(3),
                rect: Rect::new(100, 0, 50, 100),
            },
        ];
        assert_eq!(focus_neighbor(&panes, p(1), Direction::Right), Some(p(2)));
    }

    #[test]
    fn focus_on_an_unknown_pane_is_none_not_a_panic() {
        assert_eq!(
            focus_neighbor(&three_pane_layout(), p(77), Direction::Right),
            None
        );
    }

    // --- hit testing ------------------------------------------------------

    #[test]
    fn pane_at_finds_the_pane_under_a_point() {
        let panes = three_pane_layout();
        assert_eq!(pane_at(&panes, 10, 10), Some(p(1)));
        assert_eq!(pane_at(&panes, 150, 10), Some(p(2)));
        assert_eq!(pane_at(&panes, 150, 90), Some(p(3)));
        assert_eq!(pane_at(&panes, 999, 999), None);
    }

    #[test]
    fn divider_hit_testing_is_more_forgiving_than_the_drawn_width() {
        let mut t = Node::leaf(p(1));
        t.split_leaf(p(1), p(2), Direction::Right);
        let (_, dividers) = t.layout(Rect::new(0, 0, 201, 100), 1);

        // Exactly on the 1px divider.
        assert!(divider_at(&dividers, 100, 50, 0).is_some());
        // Three pixels off would miss the drawn bar, but a 4px grab zone catches
        // it — otherwise dragging a thin divider is nearly impossible.
        assert!(divider_at(&dividers, 103, 50, 4).is_some());
        assert!(divider_at(&dividers, 130, 50, 4).is_none());
    }

    #[test]
    fn leaves_and_counts_track_the_tree() {
        let mut t = Node::leaf(p(1));
        assert_eq!(t.pane_count(), 1);
        t.split_leaf(p(1), p(2), Direction::Right);
        t.split_leaf(p(2), p(3), Direction::Down);
        assert_eq!(t.pane_count(), 3);
        assert_eq!(t.leaves(), [p(1), p(2), p(3)]);
        assert!(t.contains(p(3)));
        assert!(!t.contains(p(4)));
        assert_eq!(t.first_leaf(), p(1));
    }
}
