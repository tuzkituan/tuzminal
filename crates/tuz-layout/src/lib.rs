//! Tab and pane layout for Tuzminal.
//!
//! A [`Layout`] owns a list of [`Tab`]s; each tab owns a BSP [`Node`] tree whose
//! leaves are panes. This crate is deliberately pure — no PTYs, no rendering, no
//! I/O — so the geometry and focus rules that are easy to get subtly wrong can be
//! tested exhaustively.
//!
//! ```
//! use tuz_layout::{Layout, LayoutOptions, CellSize, Direction, geom::Rect};
//!
//! let (mut layout, first) = Layout::new();
//! let second = layout.split(Direction::Right).unwrap();
//!
//! let opts = LayoutOptions {
//!     cell: CellSize { width: 8, height: 16 },
//!     ..LayoutOptions::default()
//! };
//! let frame = layout.compute(Rect::from_size(800, 600), &opts);
//! assert_eq!(frame.panes.len(), 2);
//!
//! // Focus follows the screen, not the tree.
//! assert!(layout.focus_direction(Direction::Left, &frame));
//! assert_eq!(layout.active_pane(), first);
//! # let _ = second;
//! ```

pub mod geom;
pub mod tree;

pub use geom::{Axis, Direction, Rect};
pub use tree::{Branch, DividerRect, Node, PaneId, PaneRect, SplitPath};

/// Size of one character cell in physical pixels, from the font's metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellSize {
    pub width: u32,
    pub height: u32,
}

impl Default for CellSize {
    fn default() -> Self {
        // A placeholder for tests and for the window that exists before fonts
        // are loaded; the real value comes from `tuz-font`.
        Self {
            width: 8,
            height: 16,
        }
    }
}

/// Inputs to a layout pass, derived from config plus font metrics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutOptions {
    /// Per-pane inner padding in pixels.
    pub padding_x: u16,
    pub padding_y: u16,
    /// Distribute the pixels left over after cell division evenly on both sides,
    /// instead of letting them pile up on the right and bottom.
    pub center_grid: bool,
    pub divider_width: u32,
    /// Height reserved at the top of the window for the tab bar. Zero hides it.
    pub tab_bar_height: u32,
    /// Height reserved at the bottom for the status bar. Zero hides it.
    pub status_bar_height: u32,
    /// Width reserved on the left for the file explorer. Zero hides it.
    pub sidebar_width: u32,
    /// Preferred width of one tab. Tabs shrink below this when there are many, and
    /// never grow past it, so two tabs do not each take half the window.
    pub tab_width: u32,
    /// Floor on tab width. Below this a tab cannot show anything useful, so the
    /// strip starts scrolling instead of shrinking further.
    pub min_tab_width: u32,
    /// Action buttons to place in the strip, in right-to-left order.
    pub buttons: Vec<ChromeButton>,
    pub cell: CellSize,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            padding_x: 8,
            padding_y: 8,
            center_grid: true,
            divider_width: 1,
            tab_bar_height: 0,
            status_bar_height: 0,
            sidebar_width: 0,
            tab_width: 180,
            min_tab_width: 60,
            buttons: Vec::new(),
            cell: CellSize::default(),
        }
    }
}

/// A pane's position and the terminal grid that fits inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneGeometry {
    pub pane: PaneId,
    /// Full pane extent, including padding. Use for scissor rects and for
    /// painting the pane background.
    pub rect: Rect,
    /// Cell-aligned content area. Use as the origin for glyph placement.
    pub content: Rect,
    /// Grid size to report to the PTY. Always at least 1x1.
    pub cols: u16,
    pub rows: u16,
}

/// Everything a frame needs to render, plus what a click needs to hit-test.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub panes: Vec<PaneGeometry>,
    pub dividers: Vec<DividerRect>,
    /// Strip reserved for the tab bar. Empty when the bar is hidden.
    pub tab_bar: Rect,
    /// Strip reserved for the status bar. Empty when hidden.
    pub status_bar: Rect,
    /// One rect per tab, in tab order. Empty when the bar is hidden.
    pub tabs: Vec<Rect>,
    /// Close button per tab, inside its right edge. Same length as `tabs`.
    pub tab_close: Vec<Rect>,
    /// Action buttons packed against the right of the strip.
    pub actions: Vec<(ChromeButton, Rect)>,
    /// Buttons the strip was too narrow for, to be offered in the app menu instead.
    ///
    /// In the order they should appear there. Empty at any comfortable width.
    pub collapsed_actions: Vec<ChromeButton>,
    /// The explorer sidebar, or a zero-width rect when it is closed.
    pub sidebar: Rect,
}

/// A clickable button in the tab strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeButton {
    NewTab,
    NewTabMenu,
    AppMenu,
    Plugins,
    Explorer,
    Help,
    Settings,
    SplitRight,
    SplitDown,
    Minimize,
    Maximize,
    Close,
}

impl ChromeButton {
    /// Every button, for exhaustive iteration in tests and in callers that must handle
    /// all of them.
    ///
    /// Hand-written, so adding a variant without adding it here is a bug this array
    /// cannot catch — the length assertion in the test below is what does.
    pub const ALL: [ChromeButton; 12] = [
        ChromeButton::NewTab,
        ChromeButton::NewTabMenu,
        ChromeButton::AppMenu,
        ChromeButton::Plugins,
        ChromeButton::Explorer,
        ChromeButton::Help,
        ChromeButton::Settings,
        ChromeButton::SplitRight,
        ChromeButton::SplitDown,
        ChromeButton::Minimize,
        ChromeButton::Maximize,
        ChromeButton::Close,
    ];

    /// Where this button sits in the queue to be moved into the app menu when the strip
    /// runs out of width, or `None` if it must stay on the strip at any width.
    ///
    /// Lower goes first. The order is by how much a button costs to lose: the split
    /// buttons have keyboard shortcuts and are the least missed from the strip, the
    /// explorer likewise, and the new-tab dropdown last because it is the only way to
    /// open a shell other than the default one.
    ///
    /// New-tab, the app menu and the three window controls return `None`. New-tab is the
    /// most used button on the strip, the app menu is where the collapsed ones go — so
    /// collapsing it would strand them — and a window with no close button is a trap.
    pub fn collapse_order(self) -> Option<u8> {
        Some(match self {
            ChromeButton::SplitDown => 0,
            ChromeButton::SplitRight => 1,
            ChromeButton::Explorer => 2,
            ChromeButton::NewTabMenu => 3,
            ChromeButton::NewTab
            | ChromeButton::AppMenu
            | ChromeButton::Plugins
            | ChromeButton::Help
            | ChromeButton::Settings
            | ChromeButton::Minimize
            | ChromeButton::Maximize
            | ChromeButton::Close => return None,
        })
    }

    /// Whether this button acts on the window rather than on the terminal.
    ///
    /// The window controls are a different kind of thing from the rest of the strip:
    /// pressing one is about the window, and pressing any other is about what is inside
    /// it. They are also the ones with real consequences, which is why the toolbar draws
    /// a rule before them — a close button flush against a settings button is a
    /// misclick waiting to happen.
    pub fn is_window_control(self) -> bool {
        matches!(
            self,
            ChromeButton::Minimize | ChromeButton::Maximize | ChromeButton::Close
        )
    }

    /// Whether this button belongs immediately after the last tab rather than in
    /// the group packed against the right edge.
    ///
    /// Only new-tab does. It acts on the tab strip, so it reads as part of it — the
    /// same place every browser puts it. Settings and the window controls have
    /// nothing to do with any particular tab, so they live at the far edge where
    /// they will not shift around as tabs open and close.
    pub fn leading(self) -> bool {
        matches!(self, ChromeButton::NewTab | ChromeButton::NewTabMenu)
    }

    /// The glyph drawn on the button.
    ///
    /// Single characters from ranges an ordinary font covers, so they render without
    /// depending on a Nerd Font being installed.
    pub fn glyph(self) -> char {
        match self {
            ChromeButton::NewTab => '+',
            ChromeButton::NewTabMenu => '⌄',
            ChromeButton::Settings => '⚙',
            ChromeButton::Explorer => '▤',
            ChromeButton::Help => '?',
            ChromeButton::Plugins => '⧉',
            ChromeButton::AppMenu => '☰',
            ChromeButton::SplitRight => '▥',
            ChromeButton::SplitDown => '▤',
            ChromeButton::Minimize => '—',
            ChromeButton::Maximize => '□',
            ChromeButton::Close => '⨯',
        }
    }

    /// Tooltip-style description, used in logs and available for a future tooltip.
    pub fn describe(self) -> &'static str {
        match self {
            ChromeButton::NewTab => "New tab",
            ChromeButton::NewTabMenu => "New tab with…",
            ChromeButton::Settings => "Settings",
            ChromeButton::Explorer => "File explorer",
            ChromeButton::Help => "Keyboard shortcuts",
            ChromeButton::Plugins => "Plugins",
            ChromeButton::AppMenu => "Menu",
            ChromeButton::SplitRight => "Split right",
            ChromeButton::SplitDown => "Split down",
            ChromeButton::Minimize => "Minimize",
            ChromeButton::Maximize => "Maximize",
            ChromeButton::Close => "Close window",
        }
    }
}

impl Frame {
    pub fn pane(&self, id: PaneId) -> Option<&PaneGeometry> {
        self.panes.iter().find(|p| p.pane == id)
    }

    /// Pane rects only, for the geometric focus helpers.
    fn rects(&self) -> Vec<PaneRect> {
        self.panes
            .iter()
            .map(|p| PaneRect {
                pane: p.pane,
                rect: p.rect,
            })
            .collect()
    }

    /// The pane under a window-relative point.
    pub fn pane_at(&self, x: i32, y: i32) -> Option<PaneId> {
        self.panes
            .iter()
            .find(|p| p.rect.contains(x, y))
            .map(|p| p.pane)
    }

    /// Convert a window-relative point to a cell within a pane, clamped to the
    /// pane's grid so a drag that leaves the pane still selects its edge.
    pub fn cell_at(&self, pane: PaneId, x: i32, y: i32, cell: CellSize) -> Option<(u16, u16)> {
        let g = self.pane(pane)?;
        if cell.width == 0 || cell.height == 0 {
            return None;
        }
        let col =
            ((x - g.content.x).max(0) as u32 / cell.width).min(g.cols.saturating_sub(1) as u32);
        let row =
            ((y - g.content.y).max(0) as u32 / cell.height).min(g.rows.saturating_sub(1) as u32);
        Some((col as u16, row as u16))
    }

    /// The divider near a point, within a generous grab tolerance.
    pub fn divider_at(&self, x: i32, y: i32, tolerance: u32) -> Option<&DividerRect> {
        tree::divider_at(&self.dividers, x, y, tolerance)
    }

    /// The index of the tab under a point, if the click landed on the tab strip.
    pub fn tab_at(&self, x: i32, y: i32) -> Option<usize> {
        if !self.tab_bar.contains(x, y) {
            return None;
        }
        self.tabs.iter().position(|r| r.contains(x, y))
    }

    /// The tab whose close button is under a point.
    ///
    /// Checked before [`tab_at`](Self::tab_at) by callers, since a close button sits
    /// inside its tab and both would otherwise match.
    pub fn tab_close_at(&self, x: i32, y: i32) -> Option<usize> {
        if !self.tab_bar.contains(x, y) {
            return None;
        }
        self.tab_close.iter().position(|r| r.contains(x, y))
    }

    /// The action button under a point.
    pub fn action_at(&self, x: i32, y: i32) -> Option<ChromeButton> {
        if !self.tab_bar.contains(x, y) {
            return None;
        }
        self.actions
            .iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(button, _)| *button)
    }

    /// True when the point is on chrome rather than on a pane.
    ///
    /// Used to stop a click on the tab bar from also starting a text selection in
    /// whichever pane happens to be underneath.
    pub fn is_chrome(&self, x: i32, y: i32) -> bool {
        // Deliberately NOT the sidebar. The caller treats chrome as the draggable
        // title bar, so counting the sidebar here would make every click on a file
        // start moving the window. It gets its own branch ahead of this one.
        self.tab_bar.contains(x, y) || self.status_bar.contains(x, y)
    }
}

/// What a tab holds.
///
/// Settings is a tab rather than an overlay so it behaves like everything else: it
/// can be switched away from and back to without losing its scroll position, it does
/// not black out the terminal behind it, and closing it is the same gesture as
/// closing anything else.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TabKind {
    #[default]
    Terminal,
    Settings,
    Help,
    Plugins,
}

/// One tab: a pane tree plus which of its panes has focus.
#[derive(Debug, Clone, PartialEq)]
pub struct Tab {
    root: Node,
    focus: PaneId,
    /// What this tab shows. Terminal tabs run shells; a settings tab has a pane rect
    /// for layout purposes but never gets a PTY.
    kind: TabKind,
    /// Explicit title set by a plugin or the user. When absent the UI falls back
    /// to the focused pane's process title.
    pub title: Option<String>,
}

impl Tab {
    fn new(pane: PaneId) -> Self {
        Self::of_kind(pane, TabKind::Terminal)
    }

    fn of_kind(pane: PaneId, kind: TabKind) -> Self {
        Self {
            root: Node::leaf(pane),
            focus: pane,
            kind,
            title: None,
        }
    }

    pub fn kind(&self) -> TabKind {
        self.kind
    }

    pub fn root(&self) -> &Node {
        &self.root
    }
    pub fn focus(&self) -> PaneId {
        self.focus
    }
    pub fn panes(&self) -> Vec<PaneId> {
        self.root.leaves()
    }
    pub fn pane_count(&self) -> usize {
        self.root.pane_count()
    }
}

/// What happened when a pane was closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseOutcome {
    /// The pane went away; focus moved to `new_focus`.
    PaneClosed { new_focus: PaneId },
    /// It was the tab's last pane, so the tab closed too. Another tab is active.
    TabClosed,
    /// It was the last pane of the last tab — the application should exit.
    Emptied,
    /// No such pane.
    NotFound,
}

/// Tabs, panes, focus and id allocation for one window.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    tabs: Vec<Tab>,
    active: usize,
    next_id: u32,
}

impl Layout {
    /// Create a layout with one tab holding one pane, and return that pane's id.
    pub fn new() -> (Self, PaneId) {
        let first = PaneId(1);
        (
            Self {
                tabs: vec![Tab::new(first)],
                active: 0,
                next_id: 2,
            },
            first,
        )
    }

    fn alloc_pane(&mut self) -> PaneId {
        let id = PaneId(self.next_id);
        // Ids are never reused, so a message referring to a closed pane is
        // detectably stale rather than silently aimed at a new one.
        self.next_id += 1;
        id
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }
    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active_tab(&self) -> &Tab {
        // `active` is kept in range by every mutating method.
        &self.tabs[self.active]
    }

    fn active_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    pub fn active_pane(&self) -> PaneId {
        self.active_tab().focus
    }

    /// Every pane across every tab. Panes in inactive tabs still hold live PTYs.
    pub fn all_panes(&self) -> Vec<PaneId> {
        self.tabs.iter().flat_map(|t| t.panes()).collect()
    }

    /// Panes in the active tab, which are the only ones that need rendering.
    pub fn visible_panes(&self) -> Vec<PaneId> {
        self.active_tab().panes()
    }

    pub fn tab_of(&self, pane: PaneId) -> Option<usize> {
        self.tabs.iter().position(|t| t.root.contains(pane))
    }

    // --- panes ------------------------------------------------------------

    /// Split the focused pane, moving focus to the new pane.
    pub fn split(&mut self, dir: Direction) -> Option<PaneId> {
        self.split_pane(self.active_pane(), dir)
    }

    /// Split a specific pane in whichever tab holds it.
    pub fn split_pane(&mut self, pane: PaneId, dir: Direction) -> Option<PaneId> {
        let tab_idx = self.tab_of(pane)?;
        let new_pane = self.alloc_pane();
        let tab = &mut self.tabs[tab_idx];
        if tab.root.split_leaf(pane, new_pane, dir) {
            tab.focus = new_pane;
            Some(new_pane)
        } else {
            // Roll the id back: nothing was created, so consuming one would
            // leave a confusing gap in the sequence.
            self.next_id -= 1;
            None
        }
    }

    /// Close a pane, collapsing its split or its tab as needed.
    pub fn close_pane(&mut self, pane: PaneId) -> CloseOutcome {
        let Some(tab_idx) = self.tab_of(pane) else {
            return CloseOutcome::NotFound;
        };
        let tab = &mut self.tabs[tab_idx];

        if tab.root.remove_leaf(pane) {
            // Focus the surviving neighbour. Tree order is a reasonable choice
            // here: the sibling that absorbed the space is its first leaf.
            if tab.focus == pane {
                tab.focus = tab.root.first_leaf();
            }
            return CloseOutcome::PaneClosed {
                new_focus: tab.focus,
            };
        }

        // `remove_leaf` refuses the root leaf, so this was the tab's last pane.
        self.tabs.remove(tab_idx);
        if self.tabs.is_empty() {
            return CloseOutcome::Emptied;
        }
        // Keep the neighbouring tab active rather than jumping to the start.
        self.active = self.active.min(self.tabs.len() - 1);
        CloseOutcome::TabClosed
    }

    /// Close the focused pane.
    pub fn close_active_pane(&mut self) -> CloseOutcome {
        self.close_pane(self.active_pane())
    }

    /// Give a specific pane focus, switching tabs if it lives elsewhere.
    pub fn focus_pane(&mut self, pane: PaneId) -> bool {
        match self.tab_of(pane) {
            Some(idx) => {
                self.active = idx;
                self.tabs[idx].focus = pane;
                true
            }
            None => false,
        }
    }

    /// Move focus geometrically. Returns false at the edge of the layout.
    ///
    /// Takes the frame from the last layout pass because the decision is made on
    /// what is on screen, not on tree structure.
    pub fn focus_direction(&mut self, dir: Direction, frame: &Frame) -> bool {
        let from = self.active_pane();
        match tree::focus_neighbor(&frame.rects(), from, dir) {
            Some(next) => {
                self.active_tab_mut().focus = next;
                true
            }
            None => false,
        }
    }

    /// Resize the focused pane along `dir` by a fraction of its parent split.
    pub fn resize_active(&mut self, dir: Direction, delta: f32) -> Option<f32> {
        let pane = self.active_pane();
        self.active_tab_mut().root.resize_pane(pane, dir, delta)
    }

    /// Set a split's ratio directly, for mouse drags on a divider.
    pub fn set_split_ratio(&mut self, path: &[Branch], ratio: f32) -> Option<f32> {
        self.active_tab_mut().root.set_ratio_at(path, ratio)
    }

    // --- tabs -------------------------------------------------------------

    /// Append a tab with a single pane and make it active.
    pub fn new_tab(&mut self) -> PaneId {
        self.new_tab_of(TabKind::Terminal)
    }

    /// Open a tab of a given kind and make it active.
    pub fn new_tab_of(&mut self, kind: TabKind) -> PaneId {
        let pane = self.alloc_pane();
        self.tabs.push(Tab::of_kind(pane, kind));
        self.active = self.tabs.len() - 1;
        pane
    }

    /// Index of the first tab of `kind`, if any.
    ///
    /// Used to keep settings to a single tab: opening it twice should return you to
    /// the one you already have, with its scroll position and pending edits intact.
    pub fn tab_of_kind(&self, kind: TabKind) -> Option<usize> {
        self.tabs.iter().position(|t| t.kind == kind)
    }

    /// The kind of the tab currently shown.
    pub fn active_kind(&self) -> TabKind {
        self.tabs
            .get(self.active)
            .map(|t| t.kind)
            .unwrap_or_default()
    }

    /// Close a whole tab. Returns the panes it held so their PTYs can be closed.
    pub fn close_tab(&mut self, index: usize) -> Option<Vec<PaneId>> {
        if index >= self.tabs.len() {
            return None;
        }
        let panes = self.tabs.remove(index).panes();
        if !self.tabs.is_empty() {
            // Closing a tab before the active one would otherwise shift the
            // active index and silently switch tabs.
            if self.active > index {
                self.active -= 1;
            }
            self.active = self.active.min(self.tabs.len() - 1);
        }
        Some(panes)
    }

    /// True once the last tab is gone and the window should close.
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Move a tab to a new position, keeping the same tab active.
    ///
    /// Returns whether anything moved. The active *tab* is preserved rather than the
    /// active *index*: dragging a tab must not silently switch which one you are
    /// looking at, and the index of the tab you are on changes as things shuffle
    /// past it.
    pub fn move_tab(&mut self, from: usize, to: usize) -> bool {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return false;
        }

        let active_pane = self.tabs[self.active].focus();
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);

        // Find where the previously active tab ended up.
        if let Some(index) = self
            .tabs
            .iter()
            .position(|t| t.root().contains(active_pane))
        {
            self.active = index;
        }
        true
    }

    pub fn select_tab(&mut self, index: usize) -> bool {
        if index < self.tabs.len() {
            self.active = index;
            true
        } else {
            false
        }
    }

    /// Advance to the next tab, wrapping. Tab cycling that stops at the end is
    /// consistently more annoying than one that wraps.
    pub fn next_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
    }

    pub fn prev_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
        }
    }

    // --- geometry ---------------------------------------------------------

    /// Compute the frame for the active tab within `window`.
    pub fn compute(&self, window: Rect, opts: &LayoutOptions) -> Frame {
        let show_bar = opts.tab_bar_height > 0;
        let tab_bar = if show_bar {
            Rect::new(
                window.x,
                window.y,
                window.width,
                opts.tab_bar_height.min(window.height),
            )
        } else {
            Rect::new(window.x, window.y, window.width, 0)
        };

        // The status bar takes from the bottom, so it is subtracted from the body
        // height but does not move the body's origin.
        let status_height = opts
            .status_bar_height
            .min(window.height.saturating_sub(tab_bar.height));
        let status_bar = if status_height > 0 {
            Rect::new(
                window.x,
                window.bottom() - status_height as i32,
                window.width,
                status_height,
            )
        } else {
            Rect::new(window.x, window.bottom(), window.width, 0)
        };

        let body = Rect::new(
            window.x,
            window.y + tab_bar.height as i32,
            window.width,
            window
                .height
                .saturating_sub(tab_bar.height)
                .saturating_sub(status_bar.height),
        );

        // The sidebar takes from the left of the body, the way the status bar takes
        // from the bottom, so the panes lay out into what is left and every grid
        // shrinks by exactly the width reserved here.
        let sidebar_width = opts.sidebar_width.min(body.width);
        let sidebar = if sidebar_width > 0 {
            Rect::new(body.x, body.y, sidebar_width, body.height)
        } else {
            Rect::new(body.x, body.y, 0, body.height)
        };
        let body = Rect::new(
            body.x + sidebar.width as i32,
            body.y,
            body.width.saturating_sub(sidebar.width),
            body.height,
        );

        // Action buttons are square, sized from the strip height, and packed from the
        // right. Tabs then divide whatever is left, so a tab can never sit underneath
        // a button.
        let leading: Vec<ChromeButton> = opts
            .buttons
            .iter()
            .copied()
            .filter(|b| b.leading())
            .collect();
        let trailing: Vec<ChromeButton> = opts
            .buttons
            .iter()
            .copied()
            .filter(|b| !b.leading())
            .collect();

        // Keep room for one tab at its minimum, plus the slot each leading button will
        // take after the tabs. Reserving only one button's worth — what this did before —
        // meant the trailing buttons could eat the space new-tab was about to need.
        let reserve = opts.min_tab_width + leading.len() as u32 * tab_bar.height;
        let (mut actions, collapsed_actions, free) = if tab_bar.height > 0 {
            action_rects_collapsing(tab_bar, &trailing, reserve)
        } else {
            (Vec::new(), Vec::new(), tab_bar)
        };

        // Reserve a slot per leading button up front, so the tabs stop short of where
        // new-tab will land instead of being covered by it.
        let reserved = leading.len() as u32 * tab_bar.height;
        let tab_area = Rect::new(
            free.x,
            free.y,
            free.width.saturating_sub(reserved),
            free.height,
        );

        let tabs = if tab_bar.height > 0 {
            tab_rects(
                tab_area,
                self.tabs.len(),
                opts.tab_width,
                opts.min_tab_width,
            )
        } else {
            Vec::new()
        };

        // Leading buttons follow the last tab, so new-tab sits against the strip it
        // adds to and slides right as tabs are opened.
        if tab_bar.height > 0 {
            let size = tab_bar.height;
            let mut x = tabs.last().map(|t| t.right()).unwrap_or(tab_area.x);
            for button in leading {
                let rect = Rect::new(x, tab_bar.y, size, size);
                if rect.right() > free.right() {
                    break;
                }
                actions.push((button, rect));
                x += size as i32;
            }
        }

        let tab_close = tabs.iter().map(|tab| close_rect(*tab)).collect();

        let (pane_rects, dividers) = self.active_tab().root.layout(body, opts.divider_width);
        let panes = pane_rects
            .into_iter()
            .map(|pr| grid_for(pr, opts))
            .collect();

        Frame {
            panes,
            dividers,
            tab_bar,
            status_bar,
            tabs,
            tab_close,
            actions,
            collapsed_actions,
            sidebar,
        }
    }
}

/// Lay out action buttons from the right, returning them and the space left for tabs.
///
/// Buttons are square and sized from the strip height so they scale with the font
/// rather than being fixed pixels. If the strip is too narrow to hold them all, the
/// ones that do not fit are dropped rather than overlapping the tabs — a button drawn
/// on top of a tab would steal its clicks.
pub fn action_rects(bar: Rect, buttons: &[ChromeButton]) -> (Vec<(ChromeButton, Rect)>, Rect) {
    let (placed, _, tab_area) = action_rects_collapsing(bar, buttons, bar.height);
    (placed, tab_area)
}

/// Lay out the trailing buttons, moving what does not fit into the app menu.
///
/// Returns the buttons that were placed, the ones that must be offered in the menu
/// instead, and what is left of the strip for tabs.
///
/// The narrow case used to be handled by stopping the loop when it ran out of room,
/// which dropped whichever buttons happened to be last in the list — new-tab and the
/// new-tab dropdown, since the strip is packed from the right. So the two most useful
/// buttons were the first to vanish, and nothing said where they had gone. Buttons now
/// leave in a deliberate order and land somewhere the user can still reach them.
///
/// `reserve` is how much of the strip to keep for tabs. A strip that is all buttons and
/// no tabs is not a tab strip.
pub fn action_rects_collapsing(
    bar: Rect,
    buttons: &[ChromeButton],
    reserve: u32,
) -> (Vec<(ChromeButton, Rect)>, Vec<ChromeButton>, Rect) {
    let size = bar.height;
    if size == 0 || buttons.is_empty() {
        return (Vec::new(), Vec::new(), bar);
    }

    // Space between the window controls and the app buttons, so close does not sit flush
    // against the button next to it. Derived from the button size rather than fixed, so
    // it stays proportionate when the font size changes the whole strip's height.
    let group_gap = (size / 2).max(4);

    // How many whole buttons fit beside the space kept for tabs. The gap is subtracted
    // here as well as spent below: counting the room without it would fit one more button
    // than there is space for, and the loop would then drop it off the left edge.
    let room = bar.width.saturating_sub(reserve + group_gap) / size;

    // Who leaves, in the order they volunteered. Sorted by `collapse_order` rather than
    // by position, so it is the least-missed button that goes and not merely the last
    // one in the list.
    let mut collapsed: Vec<ChromeButton> = Vec::new();
    if buttons.len() as u32 > room {
        let mut candidates: Vec<ChromeButton> = buttons
            .iter()
            .copied()
            .filter(|b| b.collapse_order().is_some())
            .collect();
        candidates.sort_by_key(|b| b.collapse_order());

        let excess = buttons.len() as u32 - room;
        collapsed.extend(candidates.into_iter().take(excess as usize));
    }

    let mut placed = Vec::with_capacity(buttons.len());
    let mut right = bar.right();
    // Tracks the group boundary as the loop walks right to left. The window controls are
    // packed first, so the boundary is where a control is followed by something that is
    // not one.
    let mut previous_was_control = false;

    for button in buttons {
        if collapsed.contains(button) {
            continue;
        }
        let is_control = button.is_window_control();
        if previous_was_control && !is_control {
            right -= group_gap as i32;
        }
        previous_was_control = is_control;

        let left = right - size as i32;
        // Nothing collapsible left to give up, and still no room. Better a button off the
        // edge than a close button that cannot be clicked, so the loop stops here.
        if left < bar.x {
            break;
        }
        placed.push((*button, Rect::new(left, bar.y, size, size.min(bar.height))));
        right = left;
    }

    let consumed = (bar.right() - right).max(0) as u32;
    let tab_area = Rect::new(bar.x, bar.y, bar.width.saturating_sub(consumed), bar.height);
    (placed, collapsed, tab_area)
}

/// The close button inside a tab, against its right edge.
fn close_rect(tab: Rect) -> Rect {
    // A square inset from the edge, and never more than a third of the tab: on a
    // narrow tab a full-height button would leave no room for the title.
    //
    // Half the strip height, not two thirds. The renderer rounds its hover fill, and at
    // two thirds that fill was a disc nearly as tall as the tab's own pill — it read as
    // the largest thing on the tab rather than as an affordance on it.
    let size = (tab.height / 2).max(1).min(tab.width / 3);
    let inset = ((tab.height.saturating_sub(size)) / 2) as i32;
    Rect::new(tab.right() - size as i32 - inset, tab.y + inset, size, size)
}

/// Divide the tab strip into per-tab rects.
///
/// Tabs take `preferred` width until they would overflow, then share the strip
/// equally down to `minimum`. Below that they stop shrinking and the surplus simply
/// overflows the right edge — a tab narrower than a few characters conveys nothing,
/// so shrinking further trades one unusable strip for another.
pub fn tab_rects(bar: Rect, count: usize, preferred: u32, minimum: u32) -> Vec<Rect> {
    if count == 0 || bar.height == 0 {
        return Vec::new();
    }
    let preferred = preferred.max(1);
    let minimum = minimum.min(preferred).max(1);

    let equal = bar.width / count as u32;
    let width = equal.min(preferred).max(minimum);

    (0..count)
        .map(|i| {
            let x = bar.x + (i as u32 * width) as i32;
            Rect::new(x, bar.y, width, bar.height)
        })
        .collect()
}

/// Fit a cell grid inside a pane rect.
///
/// Cell counts are floored so no partial row or column is ever reported to the
/// PTY, then clamped to a minimum of 1x1: a pane too small for one cell still
/// needs a valid grid, because `TIOCSWINSZ` with a zero dimension makes programs
/// behave erratically.
fn grid_for(pr: PaneRect, opts: &LayoutOptions) -> PaneGeometry {
    let inner = pr.rect.inset(opts.padding_x as u32, opts.padding_y as u32);

    let cell_w = opts.cell.width.max(1);
    let cell_h = opts.cell.height.max(1);

    let cols = (inner.width / cell_w).max(1);
    let rows = (inner.height / cell_h).max(1);

    let used_w = cols * cell_w;
    let used_h = rows * cell_h;

    let (x, y) = if opts.center_grid {
        // Split the remainder between both edges so a window that is not an
        // exact multiple of the cell size looks balanced instead of lopsided.
        let slack_x = pr.rect.width.saturating_sub(used_w) / 2;
        let slack_y = pr.rect.height.saturating_sub(used_h) / 2;
        (pr.rect.x + slack_x as i32, pr.rect.y + slack_y as i32)
    } else {
        (inner.x, inner.y)
    };

    PaneGeometry {
        pane: pr.pane,
        rect: pr.rect,
        content: Rect::new(x, y, used_w, used_h),
        cols: cols.min(u16::MAX as u32) as u16,
        rows: rows.min(u16::MAX as u32) as u16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 8x16 cells, no padding or divider: pixel maths stay checkable by hand.
    fn bare_opts() -> LayoutOptions {
        LayoutOptions {
            padding_x: 0,
            padding_y: 0,
            center_grid: false,
            divider_width: 0,
            tab_bar_height: 0,
            status_bar_height: 0,
            sidebar_width: 0,
            tab_width: 180,
            min_tab_width: 60,
            buttons: Vec::new(),
            cell: CellSize {
                width: 8,
                height: 16,
            },
        }
    }

    #[test]
    fn a_new_layout_has_one_tab_with_one_focused_pane() {
        let (l, first) = Layout::new();
        assert_eq!(l.tab_count(), 1);
        assert_eq!(l.active_pane(), first);
        assert_eq!(l.all_panes(), [first]);
    }

    #[test]
    fn splitting_moves_focus_to_the_new_pane() {
        let (mut l, first) = Layout::new();
        let second = l.split(Direction::Right).unwrap();
        assert_eq!(l.active_pane(), second);
        assert_eq!(l.active_tab().panes(), [first, second]);
    }

    #[test]
    fn pane_ids_are_never_reused() {
        // A recycled id would let a queued message for a dead pane land in a new
        // one — a bug that only shows up under load.
        let (mut l, first) = Layout::new();
        let second = l.split(Direction::Right).unwrap();
        l.close_pane(second);
        let third = l.split(Direction::Right).unwrap();
        assert_ne!(third, second);
        assert!(third.0 > second.0);
        let _ = first;
    }

    #[test]
    fn a_failed_split_does_not_consume_a_pane_id() {
        let (mut l, _) = Layout::new();
        assert_eq!(l.split_pane(PaneId(999), Direction::Right), None);
        let next = l.split(Direction::Right).unwrap();
        assert_eq!(next, PaneId(2), "id sequence should have no gap");
    }

    #[test]
    fn closing_a_pane_refocuses_a_survivor() {
        let (mut l, first) = Layout::new();
        let second = l.split(Direction::Right).unwrap();
        match l.close_pane(second) {
            CloseOutcome::PaneClosed { new_focus } => assert_eq!(new_focus, first),
            other => panic!("expected PaneClosed, got {other:?}"),
        }
        assert_eq!(l.active_pane(), first);
        assert_eq!(l.active_tab().pane_count(), 1);
    }

    #[test]
    fn closing_a_pane_that_is_not_focused_leaves_focus_alone() {
        let (mut l, first) = Layout::new();
        let second = l.split(Direction::Right).unwrap();
        assert_eq!(l.active_pane(), second);
        l.close_pane(first);
        assert_eq!(
            l.active_pane(),
            second,
            "focus should not move unnecessarily"
        );
    }

    #[test]
    fn closing_the_last_pane_of_a_tab_closes_the_tab() {
        let (mut l, first) = Layout::new();
        l.new_tab();
        assert_eq!(l.tab_count(), 2);

        l.focus_pane(first);
        assert!(matches!(l.close_pane(first), CloseOutcome::TabClosed));
        assert_eq!(l.tab_count(), 1);
    }

    #[test]
    fn closing_the_final_pane_reports_that_the_app_should_exit() {
        let (mut l, first) = Layout::new();
        assert!(matches!(l.close_pane(first), CloseOutcome::Emptied));
        assert!(l.is_empty());
    }

    #[test]
    fn closing_an_unknown_pane_is_reported_not_ignored() {
        let (mut l, _) = Layout::new();
        assert_eq!(l.close_pane(PaneId(404)), CloseOutcome::NotFound);
    }

    #[test]
    fn tab_cycling_wraps_in_both_directions() {
        let (mut l, _) = Layout::new();
        l.new_tab();
        l.new_tab();
        assert_eq!(l.active_index(), 2);

        l.next_tab();
        assert_eq!(l.active_index(), 0, "next from the last wraps to the first");
        l.prev_tab();
        assert_eq!(l.active_index(), 2, "prev from the first wraps to the last");
    }

    #[test]
    fn closing_an_earlier_tab_keeps_the_same_tab_active() {
        // Removing an element before the active index shifts everything down;
        // without compensating, the user silently lands on a different tab.
        let (mut l, _) = Layout::new();
        l.new_tab();
        let third_pane = l.new_tab();
        assert_eq!(l.active_index(), 2);

        l.close_tab(0);
        assert_eq!(l.active_index(), 1);
        assert_eq!(l.active_pane(), third_pane, "still the same tab");
    }

    #[test]
    fn closing_a_tab_returns_its_panes_so_their_ptys_can_be_reaped() {
        let (mut l, _) = Layout::new();
        let pane_a = l.new_tab();
        let pane_b = l.split(Direction::Down).unwrap();

        let closed = l.close_tab(1).unwrap();
        assert_eq!(closed.len(), 2);
        assert!(closed.contains(&pane_a) && closed.contains(&pane_b));
    }

    #[test]
    fn focus_pane_switches_tabs_when_the_pane_lives_elsewhere() {
        let (mut l, first) = Layout::new();
        l.new_tab();
        assert_eq!(l.active_index(), 1);

        assert!(l.focus_pane(first));
        assert_eq!(l.active_index(), 0);
        assert!(!l.focus_pane(PaneId(999)));
    }

    #[test]
    fn splitting_targets_only_the_tab_that_owns_the_pane() {
        let (mut l, first) = Layout::new();
        let other_tab_pane = l.new_tab();

        // Split a pane in the inactive tab.
        let added = l.split_pane(first, Direction::Right).unwrap();
        assert_eq!(l.tab_of(added), Some(0));
        assert_eq!(l.tabs()[1].pane_count(), 1, "the other tab is untouched");
        let _ = other_tab_pane;
    }

    // --- geometry ---------------------------------------------------------

    #[test]
    fn a_single_pane_grid_divides_the_window_by_cell_size() {
        let (l, first) = Layout::new();
        let f = l.compute(Rect::from_size(800, 600), &bare_opts());

        let g = f.pane(first).unwrap();
        assert_eq!((g.cols, g.rows), (100, 37)); // 800/8, floor(600/16)
        assert_eq!(g.content, Rect::new(0, 0, 800, 592));
    }

    #[test]
    fn padding_shrinks_the_grid() {
        let (l, first) = Layout::new();
        let opts = LayoutOptions {
            padding_x: 10,
            padding_y: 8,
            ..bare_opts()
        };
        let f = l.compute(Rect::from_size(800, 600), &opts);

        let g = f.pane(first).unwrap();
        // 800 - 20 = 780 -> 97 cols; 600 - 16 = 584 -> 36 rows.
        assert_eq!((g.cols, g.rows), (97, 36));
        assert_eq!(g.content.x, 10);
        assert_eq!(g.content.y, 8);
    }

    #[test]
    fn centering_leftovers_are_a_sawtooth_across_a_resize() {
        // Not a bug in centering — this is what centering *is*, and it is why the caller
        // suspends it during a window drag. The remainder after the last whole cell grows
        // as the window widens and drops back the moment another column fits, so
        // recomputing it every frame walks the text block out and snaps it back.
        //
        // Pinned here so the reason the caller turns it off does not become mysterious.
        let cell = CellSize {
            width: 8,
            height: 16,
        };
        let opts = LayoutOptions {
            padding_x: 0,
            padding_y: 0,
            center_grid: true,
            cell,
            ..LayoutOptions::default()
        };

        let offset_at = |width: u32| {
            let pr = PaneRect {
                pane: PaneId(1),
                rect: Rect::new(0, 0, width, 100),
            };
            grid_for(pr, &opts).content.x
        };

        // Across one cell's worth of width the offset climbs and then resets, rather than
        // moving in one direction.
        let offsets: Vec<i32> = (800..800 + cell.width).map(offset_at).collect();
        let climbed = offsets.iter().max().copied().unwrap();
        assert!(
            climbed > offsets[0],
            "the offset never moved, so this pane is not being centered at all"
        );
        assert_eq!(
            offset_at(800 + cell.width),
            offsets[0],
            "a whole extra column should return the offset to where it started"
        );
    }

    #[test]
    fn centering_off_anchors_the_content_and_holds_it_there() {
        // What a drag uses instead. The origin must not move with the window width, or
        // suspending centering would not have bought anything.
        let cell = CellSize {
            width: 8,
            height: 16,
        };
        let opts = LayoutOptions {
            padding_x: 4,
            padding_y: 4,
            center_grid: false,
            cell,
            ..LayoutOptions::default()
        };

        let origin_at = |width: u32| {
            let pr = PaneRect {
                pane: PaneId(1),
                rect: Rect::new(0, 0, width, 100),
            };
            let g = grid_for(pr, &opts);
            (g.content.x, g.content.y)
        };

        let first = origin_at(800);
        for width in 800..900 {
            assert_eq!(
                origin_at(width),
                first,
                "the content origin moved at width {width}, so it would still shimmer"
            );
        }
    }

    #[test]
    fn center_grid_balances_the_leftover_pixels() {
        let (l, first) = Layout::new();
        let opts = LayoutOptions {
            center_grid: true,
            ..bare_opts()
        };
        // 803 px / 8 = 100 cols using 800, leaving 3 px of slack.
        let f = l.compute(Rect::from_size(803, 600), &opts);
        let g = f.pane(first).unwrap();

        assert_eq!(g.cols, 100);
        assert_eq!(
            g.content.x, 1,
            "slack should be split, not dumped on one side"
        );
        assert_eq!(g.content.width, 800);
    }

    #[test]
    fn a_pane_too_small_for_one_cell_still_reports_a_valid_grid() {
        // TIOCSWINSZ with a zero dimension makes programs misbehave, so the
        // floor is 1x1 even when the arithmetic says zero.
        let (l, first) = Layout::new();
        let f = l.compute(Rect::from_size(4, 4), &bare_opts());
        let g = f.pane(first).unwrap();
        assert_eq!((g.cols, g.rows), (1, 1));
    }

    #[test]
    fn the_tab_bar_takes_height_off_the_top_of_the_panes() {
        let (l, first) = Layout::new();
        let opts = LayoutOptions {
            tab_bar_height: 24,
            ..bare_opts()
        };
        let f = l.compute(Rect::from_size(800, 600), &opts);

        assert_eq!(f.tab_bar, Rect::new(0, 0, 800, 24));
        let g = f.pane(first).unwrap();
        assert_eq!(g.rect, Rect::new(0, 24, 800, 576));
        assert_eq!(g.rows, 36); // 576/16
    }

    #[test]
    fn hiding_the_tab_bar_gives_its_space_back() {
        let (l, first) = Layout::new();
        let f = l.compute(Rect::from_size(800, 600), &bare_opts());
        assert_eq!(f.tab_bar.height, 0);
        assert_eq!(f.pane(first).unwrap().rect.y, 0);
    }

    #[test]
    fn split_panes_partition_the_window_without_overlap() {
        let (mut l, first) = Layout::new();
        let second = l.split(Direction::Right).unwrap();
        let f = l.compute(Rect::from_size(800, 600), &bare_opts());

        let a = f.pane(first).unwrap();
        let b = f.pane(second).unwrap();
        assert_eq!(a.rect.right(), b.rect.x, "no gap or overlap between panes");
        assert_eq!(a.cols + b.cols, 100);
    }

    #[test]
    fn focus_direction_follows_the_rendered_frame() {
        let (mut l, first) = Layout::new();
        let second = l.split(Direction::Right).unwrap();
        let f = l.compute(Rect::from_size(800, 600), &bare_opts());

        assert_eq!(l.active_pane(), second);
        assert!(l.focus_direction(Direction::Left, &f));
        assert_eq!(l.active_pane(), first);

        // At the edge, focus stays put and the caller learns nothing happened.
        assert!(!l.focus_direction(Direction::Left, &f));
        assert_eq!(l.active_pane(), first);
    }

    #[test]
    fn pane_at_maps_a_click_to_a_pane() {
        let (mut l, first) = Layout::new();
        let second = l.split(Direction::Right).unwrap();
        let f = l.compute(Rect::from_size(800, 600), &bare_opts());

        assert_eq!(f.pane_at(10, 10), Some(first));
        assert_eq!(f.pane_at(500, 10), Some(second));
        assert_eq!(f.pane_at(10_000, 10), None);
    }

    #[test]
    fn cell_at_converts_pixels_to_grid_coordinates() {
        let (l, first) = Layout::new();
        let opts = bare_opts();
        let f = l.compute(Rect::from_size(800, 600), &opts);

        assert_eq!(f.cell_at(first, 0, 0, opts.cell), Some((0, 0)));
        assert_eq!(f.cell_at(first, 8, 16, opts.cell), Some((1, 1)));
        assert_eq!(f.cell_at(first, 23, 47, opts.cell), Some((2, 2)));
    }

    #[test]
    fn cell_at_clamps_a_drag_that_leaves_the_pane() {
        // Selection dragging past the edge should extend to the last cell, not
        // return None and abandon the drag.
        let (l, first) = Layout::new();
        let opts = bare_opts();
        let f = l.compute(Rect::from_size(800, 600), &opts);

        assert_eq!(f.cell_at(first, -50, -50, opts.cell), Some((0, 0)));
        assert_eq!(f.cell_at(first, 99_999, 99_999, opts.cell), Some((99, 36)));
    }

    #[test]
    fn resize_active_adjusts_the_relevant_divider() {
        let (mut l, _) = Layout::new();
        l.split(Direction::Right);
        // Focus is on the right pane; growing it leftward shrinks the ratio.
        let r = l.resize_active(Direction::Left, 0.1).unwrap();
        assert!((r - 0.4).abs() < 1e-6, "got {r}");
    }

    #[test]
    fn divider_hit_testing_finds_the_split_to_drag() {
        let (mut l, _) = Layout::new();
        l.split(Direction::Right);
        let opts = LayoutOptions {
            divider_width: 2,
            ..bare_opts()
        };
        let f = l.compute(Rect::from_size(802, 600), &opts);

        let d = f
            .divider_at(401, 300, 4)
            .expect("divider should be grabbable");
        assert_eq!(d.axis, Axis::Horizontal);

        // Dragging it to 25% must actually move the panes.
        l.set_split_ratio(&d.path.clone(), 0.25);
        let f2 = l.compute(Rect::from_size(802, 600), &opts);
        assert_eq!(f2.panes[0].rect.width, 200);
    }

    #[test]
    fn a_deeply_nested_layout_stays_consistent() {
        // Build a 4-pane grid and assert the panes exactly tile the window.
        let (mut l, first) = Layout::new();
        let right = l.split(Direction::Right).unwrap();
        l.focus_pane(first);
        l.split(Direction::Down);
        l.focus_pane(right);
        l.split(Direction::Down);

        let f = l.compute(Rect::from_size(800, 600), &bare_opts());
        assert_eq!(f.panes.len(), 4);

        let area: u32 = f.panes.iter().map(|p| p.rect.width * p.rect.height).sum();
        assert_eq!(area, 800 * 600, "panes must tile the window exactly");
    }
}

#[cfg(test)]
mod chrome_tests {
    use super::*;

    fn bar(width: u32) -> Rect {
        Rect::new(0, 0, width, 24)
    }

    #[test]
    fn no_tabs_means_no_rects() {
        assert!(tab_rects(bar(800), 0, 180, 60).is_empty());
    }

    #[test]
    fn a_hidden_bar_produces_no_rects() {
        // Height zero is how the bar is hidden, so it must short-circuit.
        assert!(tab_rects(Rect::new(0, 0, 800, 0), 3, 180, 60).is_empty());
    }

    #[test]
    fn few_tabs_take_the_preferred_width_not_the_whole_strip() {
        // Two tabs each taking half an 800px window would look absurd.
        let rects = tab_rects(bar(800), 2, 180, 60);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].width, 180);
        assert_eq!(rects[1].x, 180);
    }

    #[test]
    fn many_tabs_share_the_strip_equally() {
        let rects = tab_rects(bar(800), 8, 180, 60);
        assert_eq!(rects.len(), 8);
        assert_eq!(rects[0].width, 100, "800/8");
        // And they tile without gaps.
        for pair in rects.windows(2) {
            assert_eq!(pair[0].right(), pair[1].x);
        }
    }

    #[test]
    fn tabs_stop_shrinking_at_the_minimum() {
        // Twenty tabs in 800px would be 40px each, which shows nothing useful.
        let rects = tab_rects(bar(800), 20, 180, 60);
        assert!(
            rects.iter().all(|r| r.width == 60),
            "expected the floor to apply, got {:?}",
            rects.first()
        );
    }

    #[test]
    fn a_minimum_larger_than_the_preferred_width_is_clamped() {
        // Misconfiguration must not produce tabs wider than requested.
        let rects = tab_rects(bar(800), 2, 100, 500);
        assert_eq!(rects[0].width, 100);
    }

    #[test]
    fn the_sidebar_comes_out_of_the_pane_body() {
        let (mut layout, _) = Layout::new();
        layout.split(Direction::Right).unwrap();

        let window = Rect::new(0, 0, 800, 600);
        let mut opts = opts_with_chrome();
        let without: Vec<Rect> = layout
            .compute(window, &opts)
            .panes
            .iter()
            .map(|p| p.rect)
            .collect();

        opts.sidebar_width = 200;
        let frame = layout.compute(window, &opts);

        assert_eq!(frame.sidebar.width, 200);
        assert_eq!(frame.sidebar.x, window.x);
        // No pane may sit under it, or the sidebar would be drawn over live terminal
        // content and clicks would land on whichever won.
        for pane in &frame.panes {
            assert!(
                pane.rect.x >= frame.sidebar.right(),
                "pane {:?} overlaps the sidebar {:?}",
                pane.rect,
                frame.sidebar
            );
        }
        // And every pane really did shrink, which is what forces the PTY resize.
        for (before, after) in without.iter().zip(&frame.panes) {
            assert!(after.rect.width < before.width);
        }
    }

    #[test]
    fn a_closed_sidebar_takes_no_room_and_is_not_chrome() {
        let (layout, _) = Layout::new();
        let frame = layout.compute(Rect::new(0, 0, 800, 600), &opts_with_chrome());

        assert_eq!(frame.sidebar.width, 0);
        assert!(!frame.sidebar.contains(0, 300));
    }

    #[test]
    fn the_sidebar_is_not_chrome() {
        let (layout, _) = Layout::new();
        let mut opts = opts_with_chrome();
        opts.sidebar_width = 200;
        let frame = layout.compute(Rect::new(0, 0, 800, 600), &opts);

        // `is_chrome` means "the draggable title bar" to the caller, which drags the
        // window on a press. Counting the sidebar would make clicking a file move the
        // window instead of selecting it.
        assert!(!frame.is_chrome(10, 300));
        assert!(
            frame.sidebar.contains(10, 300),
            "but it is still the sidebar"
        );
    }

    #[test]
    fn a_sidebar_wider_than_the_window_is_clamped_rather_than_underflowing() {
        let (layout, _) = Layout::new();
        let mut opts = opts_with_chrome();
        opts.sidebar_width = 10_000;
        let frame = layout.compute(Rect::new(0, 0, 800, 600), &opts);

        assert_eq!(frame.sidebar.width, 800);
        // The pane still needs a valid grid even with nothing left for it.
        assert!(frame.panes[0].cols >= 1 && frame.panes[0].rows >= 1);
    }

    #[test]
    fn settings_is_found_by_kind_so_it_is_never_opened_twice() {
        let (mut layout, _) = Layout::new();
        assert_eq!(layout.tab_of_kind(TabKind::Settings), None);

        layout.new_tab_of(TabKind::Settings);
        assert_eq!(layout.tab_of_kind(TabKind::Settings), Some(1));
        assert_eq!(layout.active_kind(), TabKind::Settings);

        // Switching away must not change what kind the settings tab is, or the page
        // would keep the keyboard from behind another tab.
        layout.select_tab(0);
        assert_eq!(layout.active_kind(), TabKind::Terminal);
        assert_eq!(layout.tab_of_kind(TabKind::Settings), Some(1));

        layout.close_tab(1);
        assert_eq!(layout.tab_of_kind(TabKind::Settings), None);
    }

    #[test]
    fn a_settings_tab_still_gets_a_pane_rect_to_draw_into() {
        let (mut layout, _) = Layout::new();
        layout.new_tab_of(TabKind::Settings);

        // The tab carries a pane purely so the existing layout code needs no special
        // case; nothing ever starts a shell for it.
        let frame = layout.compute(Rect::new(0, 0, 800, 600), &opts_with_chrome());
        assert_eq!(frame.panes.len(), 1, "the page needs somewhere to draw");
        assert!(frame.panes[0].rect.height > 0);
    }

    #[test]
    fn new_tab_follows_the_last_tab_and_the_rest_pack_right() {
        let (mut layout, _) = Layout::new();
        layout.new_tab();

        let mut opts = opts_with_chrome();
        opts.buttons = vec![
            ChromeButton::Close,
            ChromeButton::Settings,
            ChromeButton::NewTab,
        ];
        let frame = layout.compute(Rect::new(0, 0, 800, 600), &opts);

        let at = |want: ChromeButton| {
            frame
                .actions
                .iter()
                .find(|(b, _)| *b == want)
                .map(|(_, r)| *r)
                .unwrap_or_else(|| panic!("{want:?} should be placed"))
        };

        let last_tab = *frame.tabs.last().expect("two tabs");
        assert_eq!(
            at(ChromeButton::NewTab).x,
            last_tab.right(),
            "new-tab belongs against the tabs it adds to"
        );
        assert!(
            at(ChromeButton::Settings).x > at(ChromeButton::NewTab).right(),
            "the right-hand group stays past new-tab"
        );
        assert_eq!(
            at(ChromeButton::Close).right(),
            frame.tab_bar.right(),
            "close sits in the far corner"
        );
    }

    #[test]
    fn tabs_stop_short_of_the_new_tab_button() {
        let (mut layout, _) = Layout::new();
        layout.new_tab();

        let mut opts = opts_with_chrome();
        opts.buttons = vec![ChromeButton::NewTab];
        let frame = layout.compute(Rect::new(0, 0, 800, 600), &opts);

        // Without the reservation the tabs would fill the strip and new-tab would be
        // drawn on top of one, stealing its clicks.
        let new_tab = frame.actions[0].1;
        for tab in &frame.tabs {
            assert!(
                tab.right() <= new_tab.x,
                "tab {tab:?} runs under the new-tab button at {new_tab:?}"
            );
        }
        assert!(new_tab.right() <= frame.tab_bar.right());
    }

    #[test]
    fn tab_rects_inherit_the_strip_position_and_height() {
        let rects = tab_rects(Rect::new(10, 5, 400, 30), 2, 180, 60);
        assert_eq!(rects[0].x, 10);
        assert_eq!(rects[0].y, 5);
        assert_eq!(rects[0].height, 30);
    }

    /// Options with both strips visible, for the reservation tests.
    fn opts_with_chrome() -> LayoutOptions {
        LayoutOptions {
            padding_x: 0,
            padding_y: 0,
            center_grid: false,
            divider_width: 0,
            tab_bar_height: 24,
            status_bar_height: 20,
            sidebar_width: 0,
            tab_width: 180,
            min_tab_width: 60,
            buttons: Vec::new(),
            cell: CellSize {
                width: 8,
                height: 16,
            },
        }
    }

    #[test]
    fn both_strips_are_reserved_out_of_the_pane_area() {
        let (l, first) = Layout::new();
        let f = l.compute(Rect::from_size(800, 600), &opts_with_chrome());

        assert_eq!(f.tab_bar, Rect::new(0, 0, 800, 24));
        assert_eq!(f.status_bar, Rect::new(0, 580, 800, 20));

        let pane = f.pane(first).unwrap();
        assert_eq!(pane.rect.y, 24, "panes start below the tab bar");
        assert_eq!(pane.rect.bottom(), 580, "panes stop above the status bar");
        assert_eq!(pane.rect.height, 556, "600 - 24 - 20");
    }

    #[test]
    fn hiding_both_strips_gives_the_whole_window_to_panes() {
        let (l, first) = Layout::new();
        let opts = LayoutOptions {
            tab_bar_height: 0,
            status_bar_height: 0,
            ..opts_with_chrome()
        };
        let f = l.compute(Rect::from_size(800, 600), &opts);

        assert_eq!(f.tab_bar.height, 0);
        assert_eq!(f.status_bar.height, 0);
        assert!(f.tabs.is_empty());
        assert_eq!(f.pane(first).unwrap().rect, Rect::from_size(800, 600));
    }

    #[test]
    fn a_tab_rect_exists_for_every_tab() {
        let (mut l, _) = Layout::new();
        l.new_tab();
        l.new_tab();
        let f = l.compute(Rect::from_size(800, 600), &opts_with_chrome());
        assert_eq!(f.tabs.len(), 3);
    }

    #[test]
    fn clicking_the_strip_maps_to_a_tab_index() {
        let (mut l, _) = Layout::new();
        l.new_tab();
        let f = l.compute(Rect::from_size(800, 600), &opts_with_chrome());

        assert_eq!(f.tab_at(10, 12), Some(0));
        assert_eq!(f.tab_at(190, 12), Some(1));
        // Past the last tab is still the strip, but not a tab.
        assert_eq!(f.tab_at(700, 12), None);
        // Below the strip is a pane, not a tab.
        assert_eq!(f.tab_at(10, 200), None);
    }

    #[test]
    fn chrome_hit_testing_covers_both_strips_but_not_panes() {
        // Without this a click on the tab bar would also start a text selection in
        // the pane underneath.
        let (l, _) = Layout::new();
        let f = l.compute(Rect::from_size(800, 600), &opts_with_chrome());

        assert!(f.is_chrome(400, 5), "tab bar");
        assert!(f.is_chrome(400, 590), "status bar");
        assert!(!f.is_chrome(400, 300), "pane area");
    }

    #[test]
    fn a_window_too_short_for_its_chrome_does_not_underflow() {
        // Resizing a window very small must not produce a negative pane height.
        let (l, first) = Layout::new();
        let f = l.compute(Rect::from_size(200, 10), &opts_with_chrome());

        assert!(f.tab_bar.height <= 10);
        let pane = f.pane(first).unwrap();
        // The grid floor still applies, so the pane reports a usable grid.
        assert!(pane.cols >= 1 && pane.rows >= 1);
    }

    #[test]
    fn splits_divide_only_the_body_not_the_chrome() {
        let (mut l, first) = Layout::new();
        let second = l.split(Direction::Down).unwrap();
        let f = l.compute(Rect::from_size(800, 600), &opts_with_chrome());

        let top = f.pane(first).unwrap().rect;
        let bottom = f.pane(second).unwrap().rect;
        assert_eq!(top.y, 24, "the upper pane starts below the tab bar");
        assert_eq!(
            bottom.bottom(),
            580,
            "the lower pane stops above the status bar"
        );
        assert_eq!(top.height + bottom.height, 556);
    }
}

#[cfg(test)]
mod reorder_tests {
    use super::*;

    /// Three tabs, each identified by the pane it holds.
    fn three() -> (Layout, Vec<PaneId>) {
        let (mut layout, first) = Layout::new();
        let second = layout.new_tab();
        let third = layout.new_tab();
        (layout, vec![first, second, third])
    }

    fn order(layout: &Layout) -> Vec<PaneId> {
        layout.tabs().iter().map(|t| t.focus()).collect()
    }

    #[test]
    fn moving_a_tab_later_shifts_the_others_back() {
        let (mut layout, panes) = three();
        assert!(layout.move_tab(0, 2));
        assert_eq!(order(&layout), vec![panes[1], panes[2], panes[0]]);
    }

    #[test]
    fn moving_a_tab_earlier_shifts_the_others_forward() {
        let (mut layout, panes) = three();
        assert!(layout.move_tab(2, 0));
        assert_eq!(order(&layout), vec![panes[2], panes[0], panes[1]]);
    }

    #[test]
    fn the_same_tab_stays_active_after_a_move() {
        // Preserving the active *index* would silently switch which tab you are
        // looking at as others shuffle past it.
        let (mut layout, panes) = three();
        layout.select_tab(0);
        assert_eq!(layout.active_pane(), panes[0]);

        layout.move_tab(0, 2);
        assert_eq!(
            layout.active_pane(),
            panes[0],
            "still looking at the tab that was dragged"
        );
        assert_eq!(layout.active_index(), 2, "which is now last");
    }

    #[test]
    fn moving_a_tab_past_the_active_one_updates_its_index() {
        let (mut layout, panes) = three();
        layout.select_tab(2);
        assert_eq!(layout.active_index(), 2);

        // Drag the first tab to the end; the active one shifts down by one.
        layout.move_tab(0, 2);
        assert_eq!(layout.active_pane(), panes[2]);
        assert_eq!(layout.active_index(), 1);
    }

    #[test]
    fn a_no_op_move_is_reported_as_such() {
        let (mut layout, _) = three();
        assert!(!layout.move_tab(1, 1));
    }

    #[test]
    fn out_of_range_indices_are_refused_rather_than_panicking() {
        // Reachable from a drag that ends outside the strip.
        let (mut layout, panes) = three();
        assert!(!layout.move_tab(0, 99));
        assert!(!layout.move_tab(99, 0));
        assert_eq!(order(&layout), panes);
    }

    #[test]
    fn moving_with_a_single_tab_does_nothing() {
        let (mut layout, _) = Layout::new();
        assert!(!layout.move_tab(0, 0));
        assert_eq!(layout.tab_count(), 1);
    }

    #[test]
    fn a_move_preserves_every_tab() {
        // The reorder must not lose or duplicate a tab, which would strand a PTY.
        let (mut layout, panes) = three();
        layout.move_tab(1, 0);

        let mut after = order(&layout);
        after.sort();
        let mut before = panes.clone();
        before.sort();
        assert_eq!(after, before);
        assert_eq!(layout.tab_count(), 3);
    }
}

#[cfg(test)]
mod chrome_button_tests {
    use super::*;

    #[test]
    fn all_lists_every_variant_exactly_once() {
        // `ALL` is hand-written, and the compiler will not notice a variant left out of
        // it. This is what notices: match on each entry so a new variant makes the match
        // non-exhaustive, and check for duplicates so a copy-paste cannot pad the count.
        for button in ChromeButton::ALL {
            match button {
                ChromeButton::NewTab
                | ChromeButton::NewTabMenu
                | ChromeButton::AppMenu
                | ChromeButton::Plugins
                | ChromeButton::Explorer
                | ChromeButton::Help
                | ChromeButton::Settings
                | ChromeButton::SplitRight
                | ChromeButton::SplitDown
                | ChromeButton::Minimize
                | ChromeButton::Maximize
                | ChromeButton::Close => {}
            }
        }
        for (i, a) in ChromeButton::ALL.iter().enumerate() {
            for b in &ChromeButton::ALL[i + 1..] {
                assert_ne!(a, b, "{a:?} appears twice in ALL");
            }
        }
    }

    #[test]
    fn every_button_has_a_glyph_and_a_description() {
        for button in ChromeButton::ALL {
            assert!(
                !button.describe().is_empty(),
                "{button:?} has no description"
            );
            assert_ne!(button.glyph(), '\0', "{button:?} has no glyph");
        }
    }
}

#[cfg(test)]
mod collapse_tests {
    use super::*;

    const H: u32 = 30;

    /// The strip as the app builds it: window controls, menu, explorer, splits, new tab.
    fn trailing() -> Vec<ChromeButton> {
        vec![
            ChromeButton::Close,
            ChromeButton::Maximize,
            ChromeButton::Minimize,
            ChromeButton::AppMenu,
            ChromeButton::Explorer,
            ChromeButton::SplitDown,
            ChromeButton::SplitRight,
            ChromeButton::NewTabMenu,
        ]
    }

    fn at(width: u32) -> (Vec<ChromeButton>, Vec<ChromeButton>) {
        let (placed, collapsed, _) =
            action_rects_collapsing(Rect::new(0, 0, width, H), &trailing(), 120);
        (placed.into_iter().map(|(b, _)| b).collect(), collapsed)
    }

    #[test]
    fn a_wide_strip_shows_everything_and_collapses_nothing() {
        let (placed, collapsed) = at(1600);
        assert_eq!(placed.len(), trailing().len());
        assert!(collapsed.is_empty(), "{collapsed:?} collapsed at 1600px");
    }

    #[test]
    fn the_window_controls_are_separated_from_the_app_buttons_by_a_gap() {
        // Close sits next to a button that opens a panel. Without a gap between the two
        // groups they read as one strip of equivalent things, and a misclick closes the
        // window. The renderer draws its rule inside this gap.
        let (placed, _, _) = action_rects_collapsing(Rect::new(0, 0, 1600, H), &trailing(), 120);

        let leftmost_control = placed
            .iter()
            .filter(|(b, _)| b.is_window_control())
            .map(|(_, r)| r.x)
            .min()
            .expect("the window controls should be placed");
        let rightmost_app = placed
            .iter()
            .filter(|(b, r)| !b.is_window_control() && r.right() <= leftmost_control)
            .map(|(_, r)| r.right())
            .max()
            .expect("the app buttons should be placed");

        // Absolute bounds, not "bigger than zero": the gap is half a button wide, so it
        // has to be at least several pixels and cannot have swallowed a whole slot.
        let gap = leftmost_control - rightmost_app;
        assert!(gap >= 4, "the groups are only {gap}px apart");
        assert!(gap < H as i32, "the gap grew wider than a button: {gap}px");
    }

    #[test]
    fn the_gap_does_not_push_a_button_off_the_left_edge() {
        // The gap is spent from the same width the buttons are, so the room calculation
        // has to account for it. If it does not, the last button is placed at a negative
        // x and simply cannot be clicked.
        for width in 120..1200 {
            let (placed, _, _) =
                action_rects_collapsing(Rect::new(0, 0, width, H), &trailing(), 120);
            for (button, rect) in &placed {
                assert!(
                    rect.x >= 0,
                    "{button:?} at x={} on a {width}px strip",
                    rect.x
                );
            }
        }
    }

    #[test]
    fn the_split_buttons_go_first_and_the_window_controls_never_go() {
        // The order matters: splits have keyboard shortcuts, a close button does not have
        // an alternative that anyone will find.
        let mut seen: Vec<ChromeButton> = Vec::new();
        for width in (200..1200).rev().step_by(10) {
            let (placed, collapsed) = at(width);
            for button in collapsed {
                if !seen.contains(&button) {
                    seen.push(button);
                }
            }
            for essential in [
                ChromeButton::Close,
                ChromeButton::Maximize,
                ChromeButton::Minimize,
                ChromeButton::AppMenu,
            ] {
                assert!(
                    placed.contains(&essential),
                    "{essential:?} left the strip at {width}px"
                );
            }
        }
        assert_eq!(
            seen,
            vec![
                ChromeButton::SplitDown,
                ChromeButton::SplitRight,
                ChromeButton::Explorer,
                ChromeButton::NewTabMenu,
            ],
            "buttons collapsed in the wrong order"
        );
    }

    #[test]
    fn nothing_is_both_placed_and_collapsed() {
        // A button in both lists would be drawn on the strip *and* offered in the menu.
        for width in (150..1400).step_by(7) {
            let (placed, collapsed) = at(width);
            for button in &collapsed {
                assert!(
                    !placed.contains(button),
                    "{button:?} is on the strip and in the menu at {width}px"
                );
            }
        }
    }

    #[test]
    fn every_button_is_accounted_for_at_every_width() {
        // The bug this replaces: the loop stopped when it ran out of room and the
        // remaining buttons simply ceased to exist, with nothing offering them anywhere.
        // Below the point where even the non-collapsible ones stop fitting, buttons can
        // still be dropped — but that is a strip too narrow for a close button, not a
        // width anyone resizes to on purpose.
        for width in (400..1400).step_by(11) {
            let (placed, collapsed) = at(width);
            assert_eq!(
                placed.len() + collapsed.len(),
                trailing().len(),
                "buttons went missing at {width}px: placed {placed:?}, collapsed {collapsed:?}"
            );
        }
    }

    #[test]
    fn collapsed_buttons_are_all_ones_that_volunteered() {
        for width in (150..1400).step_by(13) {
            let (_, collapsed) = at(width);
            for button in collapsed {
                assert!(
                    button.collapse_order().is_some(),
                    "{button:?} was collapsed but is marked as staying"
                );
            }
        }
    }

    #[test]
    fn the_placed_buttons_never_overlap_or_leave_the_strip() {
        for width in (200..1400).step_by(9) {
            let bar = Rect::new(0, 0, width, H);
            let (placed, _, tabs) = action_rects_collapsing(bar, &trailing(), 120);
            for (i, (_, a)) in placed.iter().enumerate() {
                assert!(
                    a.x >= bar.x && a.right() <= bar.right(),
                    "{a:?} at {width}px"
                );
                for (_, b) in &placed[i + 1..] {
                    assert!(
                        a.right() <= b.x || b.right() <= a.x,
                        "{a:?} overlaps {b:?} at {width}px"
                    );
                }
                assert!(
                    a.x >= tabs.right(),
                    "{a:?} overlaps the tab area {tabs:?} at {width}px"
                );
            }
        }
    }

    #[test]
    fn the_app_menu_is_never_collapsed_because_it_is_where_the_others_go() {
        // Collapsing it would strand everything that collapsed into it.
        assert_eq!(ChromeButton::AppMenu.collapse_order(), None);
        for width in (100..1400).step_by(5) {
            let (_, collapsed) = at(width);
            assert!(!collapsed.contains(&ChromeButton::AppMenu));
        }
    }
}
