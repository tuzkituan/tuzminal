//! Pixel geometry primitives.

/// An axis-aligned rectangle in physical pixels.
///
/// Position is signed because intermediate layout arithmetic (subtracting
/// padding from a narrow pane) can go negative; sizes are unsigned to match what
/// `wgpu` scissor rects and surface configuration expect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn from_size(width: u32, height: u32) -> Self {
        Self::new(0, 0, width, height)
    }

    pub const fn right(&self) -> i32 {
        self.x + self.width as i32
    }
    pub const fn bottom(&self) -> i32 {
        self.y + self.height as i32
    }
    pub const fn center_x(&self) -> i32 {
        self.x + (self.width / 2) as i32
    }
    pub const fn center_y(&self) -> i32 {
        self.y + (self.height / 2) as i32
    }
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub const fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// Shrink by `dx` on each horizontal edge and `dy` on each vertical edge,
    /// saturating at zero size rather than wrapping.
    pub fn inset(&self, dx: u32, dy: u32) -> Rect {
        let shrink_x = dx.saturating_mul(2);
        let shrink_y = dy.saturating_mul(2);
        Rect {
            x: self.x + dx as i32,
            y: self.y + dy as i32,
            width: self.width.saturating_sub(shrink_x),
            height: self.height.saturating_sub(shrink_y),
        }
    }

    /// Length of the overlap between the two rects' vertical extents.
    pub fn vertical_overlap(&self, other: &Rect) -> i32 {
        (self.bottom().min(other.bottom()) - self.y.max(other.y)).max(0)
    }

    /// Length of the overlap between the two rects' horizontal extents.
    pub fn horizontal_overlap(&self, other: &Rect) -> i32 {
        (self.right().min(other.right()) - self.x.max(other.x)).max(0)
    }
}

/// Which way a split divides space, and which way focus moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    /// The axis this direction travels along.
    pub const fn axis(self) -> Axis {
        match self {
            Direction::Left | Direction::Right => Axis::Horizontal,
            Direction::Up | Direction::Down => Axis::Vertical,
        }
    }

    /// True when moving this way increases the coordinate, i.e. the new pane
    /// belongs after the existing one.
    pub const fn is_forward(self) -> bool {
        matches!(self, Direction::Right | Direction::Down)
    }

    pub const fn opposite(self) -> Direction {
        match self {
            Direction::Left => Direction::Right,
            Direction::Right => Direction::Left,
            Direction::Up => Direction::Down,
            Direction::Down => Direction::Up,
        }
    }
}

/// The axis along which a split's children are arranged.
///
/// `Horizontal` places children side by side (a vertical divider between them);
/// `Vertical` stacks them (a horizontal divider). The name refers to the
/// direction children advance along, which is the convention that keeps the
/// layout math readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_and_centers() {
        let r = Rect::new(10, 20, 100, 50);
        assert_eq!(r.right(), 110);
        assert_eq!(r.bottom(), 70);
        assert_eq!(r.center_x(), 60);
        assert_eq!(r.center_y(), 45);
    }

    #[test]
    fn contains_is_half_open_on_the_far_edges() {
        let r = Rect::new(0, 0, 10, 10);
        assert!(r.contains(0, 0));
        assert!(r.contains(9, 9));
        // Excluding the far edge is what stops adjacent panes from both
        // claiming a click on their shared boundary.
        assert!(!r.contains(10, 5));
        assert!(!r.contains(5, 10));
    }

    #[test]
    fn inset_saturates_instead_of_underflowing() {
        let r = Rect::new(0, 0, 10, 4);
        let i = r.inset(8, 8);
        assert_eq!(i.width, 0);
        assert_eq!(i.height, 0);
    }

    #[test]
    fn overlap_measures_shared_extent() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(50, 50, 100, 100);
        assert_eq!(a.vertical_overlap(&b), 50);
        assert_eq!(a.horizontal_overlap(&b), 50);

        let far = Rect::new(500, 500, 10, 10);
        assert_eq!(a.vertical_overlap(&far), 0);
    }

    #[test]
    fn direction_axis_and_orientation() {
        assert_eq!(Direction::Right.axis(), Axis::Horizontal);
        assert_eq!(Direction::Down.axis(), Axis::Vertical);
        assert!(Direction::Right.is_forward());
        assert!(!Direction::Left.is_forward());
        assert_eq!(Direction::Up.opposite(), Direction::Down);
    }
}
