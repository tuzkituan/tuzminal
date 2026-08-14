//! A dropdown anchored to a toolbar button.
//!
//! Deliberately small: one column of rows, one selection, no submenus. The settings
//! page and the explorer already carry the weight of general widget layout; a menu
//! that opens under a button and closes the moment you pick something needs almost
//! none of it, and building it on the full [`tuz_ui::Ui`] would mean scrolling,
//! footers and focus rings for a list of four shells.
//!
//! What it does need, and what the tests cover, is the part that is easy to get
//! wrong: staying on screen when the button it hangs from is near an edge.

use tuz_layout::Rect;

/// What an open menu is for, so the app knows what picking a row means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKind {
    /// Values are shell paths.
    NewTabShell,
    /// Values name a page to open.
    AppMenu,
}

/// One row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    pub label: String,
    /// What picking it means. The caller decides; the menu only carries it.
    pub value: String,
}

pub struct Menu {
    pub kind: MenuKind,
    pub items: Vec<MenuItem>,
    /// The row the keyboard is on, and the one a click lands on when it opens.
    pub selected: usize,
    /// The button this hangs from, so it can be re-anchored on a resize.
    anchor: Rect,
}

/// Rows are one cell tall plus this much breathing room, matching a settings row.
const ROW_PADDING: u32 = 6;
/// Inset from the menu edge to its text.
pub const PADDING: f32 = 8.0;
/// Widest a menu gets, in characters, before labels are left to truncate.
const MAX_COLUMNS: u32 = 40;

impl Menu {
    pub fn new(kind: MenuKind, anchor: Rect, items: Vec<MenuItem>) -> Self {
        Self {
            kind,
            items,
            selected: 0,
            anchor,
        }
    }

    pub fn selected(&self) -> Option<&MenuItem> {
        self.items.get(self.selected)
    }

    /// Move the selection, wrapping at both ends.
    ///
    /// Wrapping because a menu is short and circular movement is what every menu
    /// does; clamping would make the last row feel stuck.
    pub fn move_selection(&mut self, delta: i32) {
        if self.items.is_empty() {
            return;
        }
        let len = self.items.len() as i32;
        self.selected = (((self.selected as i32 + delta) % len + len) % len) as usize;
    }

    /// The row height for a given cell height.
    pub fn row_height(cell_height: u32) -> u32 {
        cell_height + ROW_PADDING
    }

    /// Where the menu is drawn, given the window it has to stay inside.
    ///
    /// Hangs below its button and left-aligned with it by default, then is pushed
    /// back on screen if that would overflow. A menu half off the right edge is the
    /// normal outcome of anchoring to a button in a toolbar that packs from the
    /// right, so this is the common case rather than an edge case.
    pub fn rect(&self, window: Rect, cell: (u32, u32)) -> Rect {
        let (cell_width, cell_height) = cell;
        let row = Self::row_height(cell_height);

        let widest = self
            .items
            .iter()
            .map(|i| i.label.chars().count() as u32)
            .max()
            .unwrap_or(0)
            .min(MAX_COLUMNS);
        let width = (widest * cell_width + PADDING as u32 * 4).min(window.width);
        let height = (row * self.items.len() as u32 + PADDING as u32 * 2).min(window.height);

        // Below the button, unless there is no room below — then above it, which is
        // what a menu near the bottom of the screen has to do.
        let below = self.anchor.bottom();
        let y = if below + height as i32 <= window.bottom() {
            below
        } else {
            (self.anchor.y - height as i32).max(window.y)
        };

        let x = self
            .anchor
            .x
            .min(window.right() - width as i32)
            .max(window.x);

        Rect::new(x, y, width, height)
    }

    /// The rect of row `index` within `rect`.
    pub fn row_rect(&self, rect: Rect, index: usize, cell_height: u32) -> Rect {
        let row = Self::row_height(cell_height);
        Rect::new(
            rect.x + PADDING as i32,
            rect.y + PADDING as i32 + (index as u32 * row) as i32,
            rect.width.saturating_sub(PADDING as u32 * 2),
            row,
        )
    }

    /// The row under a point, if any.
    pub fn row_at(&self, rect: Rect, cell_height: u32, x: i32, y: i32) -> Option<usize> {
        (0..self.items.len()).find(|i| self.row_rect(rect, *i, cell_height).contains(x, y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu(anchor: Rect, count: usize) -> Menu {
        Menu::new(
            MenuKind::AppMenu,
            anchor,
            (0..count)
                .map(|i| MenuItem {
                    label: format!("item {i}"),
                    value: format!("{i}"),
                })
                .collect(),
        )
    }

    const CELL: (u32, u32) = (8, 20);
    const WINDOW: Rect = Rect {
        x: 0,
        y: 0,
        width: 800,
        height: 600,
    };

    #[test]
    fn it_hangs_below_its_button() {
        let m = menu(Rect::new(100, 0, 40, 40), 3);
        let rect = m.rect(WINDOW, CELL);
        assert_eq!(rect.y, 40, "directly under the button");
        assert_eq!(rect.x, 100, "and aligned with its left edge");
    }

    #[test]
    fn a_menu_near_the_right_edge_is_pushed_back_on_screen() {
        // The normal case, not an edge case: the toolbar packs from the right, so a
        // button anchored there is exactly where these buttons live.
        let m = menu(Rect::new(790, 0, 40, 40), 3);
        let rect = m.rect(WINDOW, CELL);
        assert!(rect.right() <= WINDOW.right(), "{rect:?} runs off the right");
        assert!(rect.x >= WINDOW.x);
    }

    #[test]
    fn a_menu_with_no_room_below_opens_upwards() {
        let m = menu(Rect::new(10, 580, 40, 20), 5);
        let rect = m.rect(WINDOW, CELL);
        assert!(rect.bottom() <= WINDOW.bottom(), "{rect:?} runs off the bottom");
        assert!(rect.y < 580, "it should open above the button");
    }

    #[test]
    fn a_menu_taller_than_the_window_is_clamped_rather_than_overflowing() {
        let m = menu(Rect::new(0, 0, 40, 40), 200);
        let rect = m.rect(WINDOW, CELL);
        assert!(rect.height <= WINDOW.height);
        assert!(rect.width <= WINDOW.width);
    }

    #[test]
    fn rows_stack_without_gaps_or_overlap() {
        let m = menu(Rect::new(0, 0, 40, 40), 4);
        let rect = m.rect(WINDOW, CELL);
        for i in 1..4 {
            let above = m.row_rect(rect, i - 1, CELL.1);
            let here = m.row_rect(rect, i, CELL.1);
            assert_eq!(above.bottom(), here.y, "row {i} does not meet the one above");
        }
    }

    #[test]
    fn every_row_is_hit_testable_at_its_own_position() {
        let m = menu(Rect::new(0, 0, 40, 40), 4);
        let rect = m.rect(WINDOW, CELL);
        for i in 0..4 {
            let row = m.row_rect(rect, i, CELL.1);
            assert_eq!(
                m.row_at(rect, CELL.1, row.x + 1, row.y + 1),
                Some(i),
                "row {i} is not clickable where it is drawn"
            );
        }
        // Outside every row.
        assert_eq!(m.row_at(rect, CELL.1, rect.x - 5, rect.y), None);
    }

    #[test]
    fn a_menu_remembers_what_it_is_for() {
        // One menu type serves the shell picker and the app menu; picking a row means
        // different things in each, so the kind travels with it rather than being
        // inferred from the values.
        let shells = Menu::new(
            MenuKind::NewTabShell,
            Rect::new(0, 0, 40, 40),
            vec![MenuItem {
                label: "bash".to_owned(),
                value: "/bin/bash".to_owned(),
            }],
        );
        assert_eq!(shells.kind, MenuKind::NewTabShell);
        assert_eq!(menu(Rect::new(0, 0, 40, 40), 1).kind, MenuKind::AppMenu);
    }

    #[test]
    fn the_selection_wraps_at_both_ends() {
        let mut m = menu(Rect::new(0, 0, 40, 40), 3);
        m.move_selection(-1);
        assert_eq!(m.selected, 2, "up from the first row reaches the last");
        m.move_selection(1);
        assert_eq!(m.selected, 0, "and down from the last comes back round");
    }

    #[test]
    fn an_empty_menu_does_not_panic_or_select_anything() {
        let mut m = menu(Rect::new(0, 0, 40, 40), 0);
        m.move_selection(1);
        assert_eq!(m.selected(), None);
        // Still has to produce a usable rect rather than dividing by zero.
        let _ = m.rect(WINDOW, CELL);
    }
}
