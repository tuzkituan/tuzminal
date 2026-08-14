//! Widget model, layout, focus and hit-testing for Tuzminal's settings UI.
//!
//! # Immediate mode
//!
//! The widget list is rebuilt every frame from whatever it represents — for the
//! settings panel, the live [`tuz_config::Config`]. Nothing is retained between
//! frames except *interaction* state: which widget has focus and which is hovered.
//!
//! That is the point. A retained widget tree has to be kept in sync with the values
//! it displays, and the classic failure is a panel showing a stale number after the
//! underlying setting changed some other way (a config reload, a keybinding, a
//! plugin). Rebuilding from the source of truth makes that impossible.
//!
//! # What lives here and what does not
//!
//! This crate is pure: it computes rectangles, decides focus order, and turns clicks
//! and key presses into [`UiAction`]s. It draws nothing and knows nothing about
//! `wgpu`. Drawing lives in `tuz-render`; applying the actions lives in the
//! application. Splitting it this way is what makes focus order and value clamping —
//! fiddly logic where bugs are quiet — cheap to test.
//!
//! ```
//! use tuz_ui::{Ui, Widget, WidgetId, UiAction};
//! use tuz_layout::Rect;
//!
//! let widgets = vec![
//!     Widget::heading("Appearance"),
//!     Widget::toggle(WidgetId(1), "Ligatures", false),
//!     Widget::button(WidgetId(2), "Apply"),
//! ];
//!
//! let mut ui = Ui::new();
//! ui.layout(&widgets, Rect::new(0, 0, 400, 300), 20);
//!
//! // Clicking the toggle reports the value it should become.
//! let toggle = ui.rect_of(WidgetId(1)).unwrap();
//! assert_eq!(
//!     ui.click(toggle.center_x(), toggle.center_y()),
//!     Some(UiAction::Toggled(WidgetId(1), true)),
//! );
//! ```

use tuz_layout::Rect;

/// Identifies a widget across frames.
///
/// Derived from *what the widget controls*, not from its position, so a widget keeps
/// its focus and hover state when the list is rebuilt — even if rows move because a
/// section grew.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WidgetId(pub u32);

/// A control, before layout.
/// What a file-list row represents, which decides its icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// The `..` row.
    Parent,
    Directory,
    File,
    Symlink,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Widget {
    /// Static text. Never focusable and never hit-testable.
    Label { text: String, heading: bool },
    Button {
        id: WidgetId,
        label: String,
        enabled: bool,
    },
    Toggle {
        id: WidgetId,
        label: String,
        on: bool,
    },
    /// One of a fixed set of options, cycled with `<` and `>`.
    Select {
        id: WidgetId,
        label: String,
        options: Vec<String>,
        index: usize,
    },
    /// An editable line of text.
    ///
    /// Deliberately minimal: a caret, insertion, deletion and horizontal movement.
    /// No selection, no clipboard, no IME — those belong to the terminal grid, and a
    /// settings field that grows them becomes a second text editor to maintain.
    Text {
        id: WidgetId,
        label: String,
        value: String,
        /// Caret position, in **characters** not bytes, so multi-byte input cannot
        /// split a character and panic on the next slice.
        caret: usize,
        /// Shown greyed when the value is empty.
        placeholder: String,
    },
    /// A number adjusted by a fixed step.
    /// A reference row: what something does, and the keys that do it.
    ///
    /// Not interactive — a help page you can Tab through is a help page where Tab
    /// moves a focus ring over thirty rows that do nothing.
    Shortcut { label: String, keys: String },
    /// A row in a list: icon, name, and a dimmed detail column.
    ///
    /// `Button` was the only row-shaped widget before this, and it centres its label
    /// in a box — fine for "Save", wrong for a file name, and with nowhere to put a
    /// size or an icon.
    Entry {
        id: WidgetId,
        kind: EntryKind,
        label: String,
        detail: String,
        /// The current row, as distinct from the focused one. Both can be true: focus
        /// is where the keyboard is, selection is what an action would act on.
        selected: bool,
    },
    Stepper {
        id: WidgetId,
        label: String,
        value: f32,
        min: f32,
        max: f32,
        step: f32,
        /// Decimal places to display. Zero renders as an integer.
        decimals: u8,
    },
}

impl Widget {
    pub fn label(text: impl Into<String>) -> Self {
        Widget::Label {
            text: text.into(),
            heading: false,
        }
    }

    pub fn heading(text: impl Into<String>) -> Self {
        Widget::Label {
            text: text.into(),
            heading: true,
        }
    }

    pub fn button(id: WidgetId, label: impl Into<String>) -> Self {
        Widget::Button {
            id,
            label: label.into(),
            enabled: true,
        }
    }

    pub fn disabled_button(id: WidgetId, label: impl Into<String>) -> Self {
        Widget::Button {
            id,
            label: label.into(),
            enabled: false,
        }
    }

    pub fn toggle(id: WidgetId, label: impl Into<String>, on: bool) -> Self {
        Widget::Toggle {
            id,
            label: label.into(),
            on,
        }
    }

    pub fn select(
        id: WidgetId,
        label: impl Into<String>,
        options: Vec<String>,
        index: usize,
    ) -> Self {
        Widget::Select {
            id,
            label: label.into(),
            // Clamped so a caller passing a stale index cannot make later code
            // index out of bounds.
            index: index.min(options.len().saturating_sub(1)),
            options,
        }
    }

    pub fn text(
        id: WidgetId,
        label: impl Into<String>,
        value: impl Into<String>,
        placeholder: impl Into<String>,
    ) -> Self {
        let value: String = value.into();
        // Caret starts at the end, which is where someone editing an existing value
        // wants it.
        let caret = value.chars().count();
        Widget::Text {
            id,
            label: label.into(),
            value,
            caret,
            placeholder: placeholder.into(),
        }
    }

    pub fn shortcut(label: impl Into<String>, keys: impl Into<String>) -> Self {
        Widget::Shortcut {
            label: label.into(),
            keys: keys.into(),
        }
    }

    pub fn entry(
        id: WidgetId,
        kind: EntryKind,
        label: impl Into<String>,
        detail: impl Into<String>,
        selected: bool,
    ) -> Self {
        Widget::Entry {
            id,
            kind,
            label: label.into(),
            detail: detail.into(),
            selected,
        }
    }

    pub fn stepper(
        id: WidgetId,
        label: impl Into<String>,
        value: f32,
        range: std::ops::RangeInclusive<f32>,
        step: f32,
        decimals: u8,
    ) -> Self {
        Widget::Stepper {
            id,
            label: label.into(),
            value,
            min: *range.start(),
            max: *range.end(),
            step,
            decimals,
        }
    }

    /// The id, for everything except labels.
    pub fn id(&self) -> Option<WidgetId> {
        match self {
            Widget::Label { .. } | Widget::Shortcut { .. } => None,
            Widget::Button { id, .. }
            | Widget::Toggle { id, .. }
            | Widget::Select { id, .. }
            | Widget::Text { id, .. }
            | Widget::Entry { id, .. }
            | Widget::Stepper { id, .. } => Some(*id),
        }
    }

    /// Whether this widget can take keyboard focus or respond to a click.
    ///
    /// Labels never can; a disabled button never can. Everything else always can.
    pub fn is_interactive(&self) -> bool {
        match self {
            Widget::Label { .. } | Widget::Shortcut { .. } => false,
            Widget::Button { enabled, .. } => *enabled,
            _ => true,
        }
    }

    /// Text shown on the right of the row, if the widget has a value.
    pub fn value_text(&self) -> Option<String> {
        match self {
            Widget::Label { .. } | Widget::Button { .. } | Widget::Shortcut { .. } => None,
            // The detail column is drawn by the row itself, not as a value: it is
            // dimmed supporting text, not something the row's controls change.
            Widget::Entry { .. } => None,
            Widget::Toggle { on, .. } => Some(if *on {
                "[x]".to_owned()
            } else {
                "[ ]".to_owned()
            }),
            Widget::Select { options, index, .. } => Some(
                options
                    .get(*index)
                    .map(|s| format!("‹ {s} ›"))
                    // An empty option list is inert rather than a panic.
                    .unwrap_or_else(|| "‹ — ›".to_owned()),
            ),
            Widget::Stepper {
                value, decimals, ..
            } => Some(format!("‹ {value:.*} ›", *decimals as usize)),
            Widget::Text {
                value, placeholder, ..
            } => Some(if value.is_empty() {
                placeholder.clone()
            } else {
                value.clone()
            }),
        }
    }
}

/// Byte offset of the `n`th character.
///
/// Every edit goes through this rather than indexing bytes directly: `String::insert`
/// and `remove` take byte offsets and panic on a boundary inside a multi-byte
/// character, so a caret counted in characters must be converted before use.
fn char_to_byte(text: &str, n: usize) -> usize {
    text.char_indices()
        .nth(n)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

/// A widget with its computed position.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub widget: Widget,
    /// The whole row.
    pub rect: Rect,
    /// The right-hand portion holding the value, for `Toggle`/`Select`/`Stepper`.
    /// Clicking the left half of this decrements, the right half increments.
    pub value_rect: Rect,
}

impl Placed {
    fn is_interactive(&self) -> bool {
        self.widget.is_interactive()
    }
}

/// What an interaction produced.
///
/// Each variant carries the *resulting* value rather than a delta, so the caller
/// applies it without re-deriving anything and cannot disagree with the UI about
/// what the new value is.
#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    Pressed(WidgetId),
    Toggled(WidgetId, bool),
    Selected(WidgetId, usize),
    Changed(WidgetId, f32),
    /// A text field's new contents. Carries the whole value rather than a delta, so
    /// the caller never has to replay edits to work out what it now says.
    Edited(WidgetId, String),
}

impl UiAction {
    pub fn id(&self) -> WidgetId {
        match self {
            UiAction::Pressed(id)
            | UiAction::Toggled(id, _)
            | UiAction::Selected(id, _)
            | UiAction::Changed(id, _) => *id,
            UiAction::Edited(id, _) => *id,
        }
    }
}

/// A key press the UI understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiKey {
    Tab,
    ShiftTab,
    Up,
    Down,
    Left,
    Right,
    Activate,
    Escape,
    /// Editing keys, meaningful only when a text field has focus.
    Backspace,
    Delete,
    Home,
    End,
}

/// How the UI responded to a key.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyResponse {
    /// Focus moved, or nothing happened, but the key was consumed.
    Consumed,
    /// The key produced a value change.
    Action(UiAction),
    /// Escape: the caller should close the panel.
    Close,
}

/// Layout metrics, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    /// Height of one row.
    pub row_height: u32,
    /// Vertical gap between rows.
    pub row_gap: u32,
    /// Extra space above a heading, to separate groups.
    pub heading_space: u32,
    /// Inset from the panel edge.
    pub padding: u32,
    /// Width of the value column on the right.
    pub value_width: u32,
    /// Width of one character, for sizing a button to its label.
    pub char_width: u32,
}

impl Metrics {
    /// Metrics derived from the cell height, so the panel scales with the font
    /// rather than being fixed pixel sizes that look wrong at other sizes.
    pub fn from_cell_height(cell_height: u32) -> Self {
        Self {
            row_height: cell_height + 6,
            row_gap: 2,
            heading_space: cell_height,
            padding: cell_height,
            value_width: cell_height * 12,
            // An estimate. Monospace cells are roughly half as wide as they are tall,
            // and this constructor is only given the height. Prefer `from_cell` where
            // the real width is known.
            char_width: (cell_height / 2).max(1),
        }
    }

    /// Metrics from the real cell size, so a button sized to its label is exact.
    pub fn from_cell(cell_width: u32, cell_height: u32) -> Self {
        Self {
            char_width: cell_width.max(1),
            ..Self::from_cell_height(cell_height)
        }
    }
}

/// Whether a footer stacks its label above its value rather than beside it.
///
/// Only a lone editable prompt does. A row of buttons has no label to stack, and a
/// settings row is wide enough to keep the two side by side.
fn stacks_its_label(footer: &[Widget]) -> bool {
    footer.len() == 1
        && matches!(
            footer[0],
            Widget::Text { .. } | Widget::Select { .. } | Widget::Stepper { .. }
        )
}

/// Height of a footer button. Half again a body row, so the page's actions carry
/// more weight than the settings above them.
fn footer_row_height(metrics: Metrics) -> u32 {
    metrics.row_height + metrics.row_height / 2
}

/// The text a footer widget shows, for sizing it.
fn footer_label(widget: &Widget) -> &str {
    match widget {
        Widget::Label { text, .. } => text,
        Widget::Shortcut { label, .. } => label,
        Widget::Button { label, .. }
        | Widget::Toggle { label, .. }
        | Widget::Select { label, .. }
        | Widget::Text { label, .. }
        | Widget::Entry { label, .. }
        | Widget::Stepper { label, .. } => label,
    }
}

/// Laid-out widgets plus interaction state.
#[derive(Debug, Default)]
pub struct Ui {
    placed: Vec<Placed>,
    /// Index into `placed`. Stored as an id as well so focus survives a rebuild.
    focus: Option<WidgetId>,
    hover: Option<WidgetId>,
    /// Total height the last layout needed.
    content_height: u32,
    /// Height of the area the content was laid out into.
    viewport_height: u32,
    /// How far the content is scrolled, in pixels. Always clamped so the last row
    /// can reach the bottom edge and no further.
    scroll: u32,
    /// How many entries in `placed` belong to the scrolling body. The rest are the
    /// pinned footer, which never scrolls and is excluded from the scroll maths.
    body_count: usize,
    /// The band the footer occupies, or a zero-height rect when there is no footer.
    footer_area: Rect,
    /// The scrolling viewport. Body rows outside it are laid out but not hit-testable,
    /// because they are clipped away when drawn.
    body_area: Rect,
}

impl Ui {
    pub fn new() -> Self {
        Self::default()
    }

    /// Position `widgets` inside `area`, preserving focus and hover by id.
    ///
    /// Called every frame. Focus is kept by id rather than index, so a widget that
    /// moves because a section above it grew does not lose focus.
    pub fn layout(&mut self, widgets: &[Widget], area: Rect, cell_height: u32) {
        self.layout_with(widgets, area, Metrics::from_cell_height(cell_height));
    }

    pub fn layout_with(&mut self, widgets: &[Widget], area: Rect, metrics: Metrics) {
        self.layout_split_with(widgets, &[], area, metrics)
    }

    /// Lay out a scrolling body with a row of `footer` widgets pinned to the bottom.
    ///
    /// The footer is where actions live — save, revert, close. Leaving them at the end
    /// of the scrolling list meant they were off screen exactly when you wanted them:
    /// you would change a setting near the top and then have to scroll to the bottom
    /// to save it. Pinned, they are always reachable, and the body scrolls behind.
    ///
    /// Footer widgets are laid out side by side across the width rather than stacked,
    /// because a handful of buttons in a row is what a footer is.
    pub fn layout_split(
        &mut self,
        body: &[Widget],
        footer: &[Widget],
        area: Rect,
        cell_height: u32,
    ) {
        self.layout_split_with(body, footer, area, Metrics::from_cell_height(cell_height));
    }

    pub fn layout_split_with(
        &mut self,
        widgets: &[Widget],
        footer: &[Widget],
        area: Rect,
        metrics: Metrics,
    ) {
        self.placed.clear();

        // The footer claims its band first, so the body never lays out underneath it.
        let footer_height = if footer.is_empty() {
            0
        } else {
            // The footer row is taller than a body row: these are the page's primary
            // actions and a button the same height as a checkbox does not read as one.
            // A lone prompt is taller still, because its label sits above the field
            // rather than beside it — see `place_footer`.
            let rows = if stacks_its_label(footer) { 2 } else { 1 };
            footer_row_height(metrics) * rows + metrics.padding * 2
        };
        let body_height = area.height.saturating_sub(footer_height);
        let area = Rect::new(area.x, area.y, area.width, body_height);
        self.footer_area = Rect::new(area.x, area.bottom(), area.width, footer_height);
        self.body_area = area;

        let left = area.x + metrics.padding as i32;
        let width = area.width.saturating_sub(metrics.padding * 2);
        // Scroll shifts every row up. Applied here rather than at draw time so
        // hit-testing and focus operate on what is actually on screen.
        let mut y = area.y + metrics.padding as i32 - self.scroll as i32;

        for widget in widgets {
            if matches!(widget, Widget::Label { heading: true, .. }) && !self.placed.is_empty() {
                // Headings get breathing room above, except the first.
                y += metrics.heading_space as i32;
            }

            let rect = Rect::new(left, y, width, metrics.row_height);
            // The value column is right-aligned within the row, and never wide enough
            // to reach the label. Capping it at the row width was not enough: on a
            // narrow panel `value_width` exceeds the row, the cap hands it everything,
            // and the label and the value are drawn one on top of the other.
            let value_width = metrics.value_width.min(width * 2 / 3);
            let value_rect = Rect::new(
                rect.right() - value_width as i32,
                y,
                value_width,
                metrics.row_height,
            );

            self.placed.push(Placed {
                widget: widget.clone(),
                rect,
                value_rect,
            });
            y += (metrics.row_height + metrics.row_gap) as i32;
        }

        // Measured without the scroll offset, or the content would appear to shrink
        // as it scrolls and the clamp below would drift.
        self.content_height = (y - area.y + self.scroll as i32).max(0) as u32 + metrics.padding;
        self.viewport_height = area.height;

        // Re-clamp after a resize or a change in content: a panel that grew, or a
        // window that got taller, must not leave the view scrolled past the end.
        let max = self.max_scroll();
        if self.scroll > max {
            self.scroll = max;
            // Redo the placement with the corrected offset rather than showing one
            // stale frame at the old position.
            self.relayout_rows(area, metrics);
        }

        // Everything placed so far scrolls; everything after this does not.
        self.body_count = self.placed.len();
        self.place_footer(footer, metrics);

        // Drop focus and hover if the widget they referred to is gone.
        if let Some(id) = self.focus {
            if self.index_of(id).is_none() {
                self.focus = None;
            }
        }
        if let Some(id) = self.hover {
            if self.index_of(id).is_none() {
                self.hover = None;
            }
        }
    }

    /// Reposition already-placed rows for the current scroll offset.
    ///
    /// Cheaper than rebuilding the widget list, and used only to correct a clamp.
    /// Lay the footer widgets out in a row across the pinned band.
    ///
    /// Each button is sized to its own label rather than stretched to an equal share
    /// of the width. A button spanning a third of the window is a big target that
    /// still reads as a mistake — the label floats in the middle of an expanse of
    /// box, and nothing about the shape says where the edges are. Sized to their
    /// text and pushed to the right, they look like the buttons they are.
    fn place_footer(&mut self, footer: &[Widget], metrics: Metrics) {
        if footer.is_empty() {
            return;
        }
        let band = self.footer_area;
        let gap = metrics.row_gap * 4;

        let width_of = |widget: &Widget| {
            let chars = footer_label(widget).chars().count() as u32;
            // Padding either side of the label, so the text is not against the border.
            chars * metrics.char_width + metrics.char_width * 6
        };

        let total: u32 = footer.iter().map(width_of).sum::<u32>()
            + gap * footer.len().saturating_sub(1) as u32;

        // Right-aligned, and clamped to the band so a narrow window pushes the row to
        // the left edge rather than off it.
        let available = band.width.saturating_sub(metrics.padding * 2);
        let start = if total >= available {
            band.x + metrics.padding as i32
        } else {
            band.right() - metrics.padding as i32 - total as i32
        };

        // A lone footer widget is a prompt, not a button row: a rename field sized to
        // the words "Rename to" would leave a handful of characters for the name.
        // Give it the whole band.
        let sole = footer.len() == 1;
        let stacked = stacks_its_label(footer);

        let mut x = if sole { band.x + metrics.padding as i32 } else { start };
        let y = band.y + metrics.padding as i32;
        for widget in footer {
            let width = if sole { available } else { width_of(widget) };
            let row = footer_row_height(metrics);
            let rect = Rect::new(x, y, width, if stacked { row * 2 } else { row });

            // Buttons draw their box at `rect`, so for them the two agree. Anything
            // with a value — a text field especially — needs a rect of its own, or the
            // label and the value are drawn one on top of the other.
            let value_rect = if matches!(widget, Widget::Button { .. } | Widget::Label { .. }) {
                rect
            } else if stacked {
                // The lower row, full width. A prompt in a narrow column has no room
                // to put a label beside a filename, so it goes above it instead.
                Rect::new(rect.x, rect.y + row as i32, width, row)
            } else {
                // Measured off the label that is actually there rather than a fixed
                // column width, because the label is the only thing competing with it.
                let label = footer_label(widget).chars().count() as u32;
                let reserved = label * metrics.char_width + metrics.char_width * 2;
                let value_width = width
                    .saturating_sub(reserved)
                    .max(metrics.char_width * 4)
                    .min(width);
                Rect::new(
                    rect.right() - value_width as i32,
                    y,
                    value_width,
                    rect.height,
                )
            };

            self.placed.push(Placed {
                widget: widget.clone(),
                rect,
                value_rect,
            });
            x += (width + gap) as i32;
        }
    }

    /// The scrolling viewport rows are clipped to, plus the pinned footer.
    ///
    /// Drawing uses this to skip rows that are laid out but off screen. It spans both
    /// bands because the footer sits below the body and its rows must never be culled.
    pub fn viewport(&self) -> Rect {
        let body = self.body_area;
        let footer = self.footer_area;
        if footer.height == 0 {
            return body;
        }
        Rect::new(
            body.x,
            body.y,
            body.width.max(footer.width),
            body.height + footer.height,
        )
    }

    /// The pinned footer band, for drawing a divider above it.
    pub fn footer_area(&self) -> Rect {
        self.footer_area
    }

    /// Rows that scroll. The footer is excluded.
    pub fn body(&self) -> &[Placed] {
        &self.placed[..self.body_count.min(self.placed.len())]
    }

    fn relayout_rows(&mut self, area: Rect, metrics: Metrics) {
        let mut y = area.y + metrics.padding as i32 - self.scroll as i32;
        let mut first = true;
        for placed in &mut self.placed {
            if matches!(placed.widget, Widget::Label { heading: true, .. }) && !first {
                y += metrics.heading_space as i32;
            }
            first = false;
            let height = placed.rect.height;
            placed.rect.y = y;
            placed.value_rect.y = y;
            y += (height + metrics.row_gap) as i32;
        }
    }

    pub fn placed(&self) -> &[Placed] {
        &self.placed
    }
    pub fn content_height(&self) -> u32 {
        self.content_height
    }
    pub fn scroll(&self) -> u32 {
        self.scroll
    }

    /// The furthest the content can scroll: zero when everything already fits.
    pub fn max_scroll(&self) -> u32 {
        self.content_height.saturating_sub(self.viewport_height)
    }

    /// True when the content is taller than the area it was laid out into.
    pub fn is_scrollable(&self) -> bool {
        self.max_scroll() > 0
    }

    /// Scroll by `delta` pixels, positive being downward. Returns whether it moved.
    pub fn scroll_by(&mut self, delta: i32) -> bool {
        let max = self.max_scroll() as i32;
        let next = (self.scroll as i32 + delta).clamp(0, max) as u32;
        let moved = next != self.scroll;
        self.scroll = next;
        moved
    }

    /// Scroll the focused widget into view, using the area of the last layout.
    ///
    /// The rect-taking form exists for callers that clip to something other than the
    /// body; a list just wants its cursor on screen.
    pub fn scroll_to_focus_in_body(&mut self) -> bool {
        let area = self.body_area;
        self.scroll_to_focus(area)
    }

    /// Scroll so the focused widget is fully visible.
    ///
    /// Called after focus moves: tabbing to a control below the fold would otherwise
    /// move an invisible focus ring, which looks like the key did nothing.
    pub fn scroll_to_focus(&mut self, area: Rect) -> bool {
        let Some(id) = self.focus else {
            return false;
        };
        // A focused footer button is pinned below the body and so always visible;
        // scrolling toward it would chase a target that never enters the viewport.
        if self
            .placed
            .iter()
            .position(|p| p.widget.id() == Some(id))
            .is_some_and(|i| i >= self.body_count)
        {
            return false;
        }
        let Some(rect) = self.rect_of(id) else {
            return false;
        };

        if rect.y < area.y {
            let delta = rect.y - area.y;
            return self.scroll_by(delta);
        }
        if rect.bottom() > area.bottom() {
            let delta = rect.bottom() - area.bottom();
            return self.scroll_by(delta);
        }
        false
    }
    pub fn focused(&self) -> Option<WidgetId> {
        self.focus
    }
    pub fn hovered(&self) -> Option<WidgetId> {
        self.hover
    }

    pub fn rect_of(&self, id: WidgetId) -> Option<Rect> {
        self.index_of(id).map(|i| self.placed[i].rect)
    }

    fn index_of(&self, id: WidgetId) -> Option<usize> {
        self.placed.iter().position(|p| p.widget.id() == Some(id))
    }

    /// The interactive widget at a point, if any.
    pub fn hit(&self, x: i32, y: i32) -> Option<WidgetId> {
        self.hit_index(x, y).and_then(|i| self.placed[i].widget.id())
    }

    /// Index of the interactive row under a point.
    ///
    /// Body rows are only hit inside the scrolling viewport. Layout places every row
    /// at its natural offset without clamping, so rows past the fold have rects that
    /// extend down into the pinned footer — and since they come first in `placed`,
    /// one of them would answer for a click meant for a footer button. They are
    /// clipped away when drawn, so treating them as absent here is what makes
    /// hit-testing agree with what is on screen.
    fn hit_index(&self, x: i32, y: i32) -> Option<usize> {
        self.placed
            .iter()
            .enumerate()
            .find(|(i, p)| {
                p.is_interactive()
                    && p.rect.contains(x, y)
                    // A clipped-away body row is skipped rather than ending the
                    // search: the footer button underneath it is the real target.
                    && (*i >= self.body_count || self.body_area.contains(x, y))
            })
            .map(|(i, _)| i)
    }

    /// Update the hovered widget, reporting whether it changed.
    ///
    /// The return value exists so the caller only requests a redraw when the answer
    /// differs — repainting on every pointer move would defeat the event-driven
    /// redraw policy the whole application is built around.
    pub fn set_pointer(&mut self, x: i32, y: i32) -> bool {
        let next = self.hit(x, y);
        let changed = next != self.hover;
        self.hover = next;
        changed
    }

    /// Clear hover, e.g. when the pointer leaves the panel.
    pub fn clear_pointer(&mut self) -> bool {
        let changed = self.hover.is_some();
        self.hover = None;
        changed
    }

    /// Handle a click, returning the action it produced.
    ///
    /// Clicking also moves focus there, so the keyboard and mouse never disagree
    /// about which widget is current.
    pub fn click(&mut self, x: i32, y: i32) -> Option<UiAction> {
        let index = self.hit_index(x, y)?;

        let placed = &self.placed[index];
        let id = placed.widget.id()?;
        self.focus = Some(id);

        match &placed.widget {
            Widget::Button { .. } => Some(UiAction::Pressed(id)),
            // A list row behaves like a button: clicking it is activating it. What
            // that means — descend, or select — is the caller's business.
            Widget::Entry { .. } => Some(UiAction::Pressed(id)),
            Widget::Toggle { on, .. } => Some(UiAction::Toggled(id, !on)),

            // Never reached: `hit_index` filters on `is_interactive`, which a
            // shortcut row is not. Matched explicitly so a future variant cannot slip
            // through as a silent no-op.
            Widget::Shortcut { .. } => None,

            // For a value widget, which half of the value column was clicked
            // decides the direction. Clicking the label side does nothing but focus,
            // so a click meant to select a row cannot accidentally change it.
            Widget::Select { .. } | Widget::Stepper { .. } => {
                if !placed.value_rect.contains(x, y) {
                    return None;
                }
                let forward = x >= placed.value_rect.center_x();
                self.adjust(index, forward)
            }
            // Clicking a text field focuses it so typing goes there; it does not
            // move the caret, since that needs per-character hit-testing the widget
            // does not carry.
            Widget::Text { .. } => None,
            Widget::Label { .. } => None,
        }
    }

    /// Handle a key press.
    pub fn key(&mut self, key: UiKey) -> KeyResponse {
        match key {
            UiKey::Escape => KeyResponse::Close,

            UiKey::Tab | UiKey::Down => {
                self.move_focus(true);
                KeyResponse::Consumed
            }
            UiKey::ShiftTab | UiKey::Up => {
                self.move_focus(false);
                KeyResponse::Consumed
            }

            UiKey::Backspace | UiKey::Delete | UiKey::Home | UiKey::End => {
                let Some(index) = self.focus.and_then(|id| self.index_of(id)) else {
                    return KeyResponse::Consumed;
                };
                self.edit(index, key)
            }

            UiKey::Left | UiKey::Right => {
                let Some(index) = self.focus.and_then(|id| self.index_of(id)) else {
                    return KeyResponse::Consumed;
                };
                // In a text field the arrows move the caret rather than adjusting a
                // value, which is what anyone typing expects them to do.
                if let Widget::Text { caret, value, .. } = &mut self.placed[index].widget {
                    let len = value.chars().count();
                    *caret = if key == UiKey::Right {
                        (*caret + 1).min(len)
                    } else {
                        caret.saturating_sub(1)
                    };
                    return KeyResponse::Consumed;
                }
                // A toggle responds to left/right too: it is the only sensible
                // reading of "adjust this value" for a boolean.
                if let Widget::Toggle { id, on, .. } = &self.placed[index].widget {
                    let want = key == UiKey::Right;
                    if *on == want {
                        return KeyResponse::Consumed;
                    }
                    return KeyResponse::Action(UiAction::Toggled(*id, want));
                }
                match self.adjust(index, key == UiKey::Right) {
                    Some(action) => KeyResponse::Action(action),
                    None => KeyResponse::Consumed,
                }
            }

            UiKey::Activate => {
                let Some(index) = self.focus.and_then(|id| self.index_of(id)) else {
                    return KeyResponse::Consumed;
                };
                match &self.placed[index].widget {
                    Widget::Button { id, .. } | Widget::Entry { id, .. } => {
                        KeyResponse::Action(UiAction::Pressed(*id))
                    }
                    Widget::Toggle { id, on, .. } => {
                        KeyResponse::Action(UiAction::Toggled(*id, !on))
                    }
                    // Enter on a value widget advances it, which is the least
                    // surprising thing for a control with no other "activate".
                    Widget::Select { .. } | Widget::Stepper { .. } => {
                        match self.adjust(index, true) {
                            Some(action) => KeyResponse::Action(action),
                            None => KeyResponse::Consumed,
                        }
                    }
                    // Enter in a text field commits nothing extra: edits are already
                    // reported as they are typed.
                    Widget::Text { .. } | Widget::Label { .. } | Widget::Shortcut { .. } => {
                        KeyResponse::Consumed
                    }
                }
            }
        }
    }

    /// Step a `Select` or `Stepper` in one direction.
    fn adjust(&self, index: usize, forward: bool) -> Option<UiAction> {
        match &self.placed[index].widget {
            Widget::Select {
                id,
                options,
                index: current,
                ..
            } => {
                if options.is_empty() {
                    return None;
                }
                // Wraps at both ends: with a handful of themes, cycling is quicker
                // than stopping and reversing.
                let next = if forward {
                    (current + 1) % options.len()
                } else {
                    (current + options.len() - 1) % options.len()
                };
                Some(UiAction::Selected(*id, next))
            }

            Widget::Stepper {
                id,
                value,
                min,
                max,
                step,
                ..
            } => {
                let delta = if forward { *step } else { -*step };
                // Clamped here rather than by the caller, so no `Changed` can ever
                // carry an out-of-range value. The same guard exists in
                // `ConfigManager::modify`; this stops the UI from even proposing it.
                let next = (value + delta).clamp(*min, *max);
                // Suppress a no-op at the end of the range: emitting it would mark
                // the config dirty and enable Save for a change that did not happen.
                if (next - value).abs() < f32::EPSILON {
                    return None;
                }
                Some(UiAction::Changed(*id, next))
            }

            _ => None,
        }
    }

    /// Insert a typed character into the focused text field.
    ///
    /// Returns the resulting action, or `None` when focus is not on a text field —
    /// which is how the caller knows whether the character was consumed or should
    /// fall through.
    pub fn type_char(&mut self, c: char) -> Option<UiAction> {
        // Control characters arrive as their own keys; letting them into the buffer
        // would put literal escape bytes in a config value.
        if c.is_control() {
            return None;
        }
        let index = self.focus.and_then(|id| self.index_of(id))?;
        let Widget::Text {
            id, value, caret, ..
        } = &mut self.placed[index].widget
        else {
            return None;
        };

        let byte = char_to_byte(value, *caret);
        value.insert(byte, c);
        *caret += 1;
        Some(UiAction::Edited(*id, value.clone()))
    }

    /// Apply an editing key to a text field.
    fn edit(&mut self, index: usize, key: UiKey) -> KeyResponse {
        let Widget::Text {
            id, value, caret, ..
        } = &mut self.placed[index].widget
        else {
            return KeyResponse::Consumed;
        };

        match key {
            UiKey::Home => {
                *caret = 0;
                KeyResponse::Consumed
            }
            UiKey::End => {
                *caret = value.chars().count();
                KeyResponse::Consumed
            }
            UiKey::Backspace => {
                if *caret == 0 {
                    return KeyResponse::Consumed;
                }
                let byte = char_to_byte(value, *caret - 1);
                value.remove(byte);
                *caret -= 1;
                KeyResponse::Action(UiAction::Edited(*id, value.clone()))
            }
            UiKey::Delete => {
                if *caret >= value.chars().count() {
                    return KeyResponse::Consumed;
                }
                let byte = char_to_byte(value, *caret);
                value.remove(byte);
                KeyResponse::Action(UiAction::Edited(*id, value.clone()))
            }
            _ => KeyResponse::Consumed,
        }
    }

    /// Move focus to the next or previous interactive widget, wrapping.
    fn move_focus(&mut self, forward: bool) {
        let interactive: Vec<WidgetId> = self
            .placed
            .iter()
            .filter(|p| p.is_interactive())
            .filter_map(|p| p.widget.id())
            .collect();

        if interactive.is_empty() {
            self.focus = None;
            return;
        }

        let next = match self
            .focus
            .and_then(|id| interactive.iter().position(|c| *c == id))
        {
            Some(current) => {
                if forward {
                    (current + 1) % interactive.len()
                } else {
                    (current + interactive.len() - 1) % interactive.len()
                }
            }
            // Entering from nowhere starts at the appropriate end, so Shift+Tab
            // into a panel lands on the last control rather than the first.
            None => {
                if forward {
                    0
                } else {
                    interactive.len() - 1
                }
            }
        };
        self.focus = Some(interactive[next]);
    }

    /// Focus a specific widget, if it exists and can take focus.
    pub fn focus(&mut self, id: WidgetId) -> bool {
        match self.index_of(id) {
            Some(i) if self.placed[i].is_interactive() => {
                self.focus = Some(id);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::new(0, 0, 400, 600)
    }

    fn id(n: u32) -> WidgetId {
        WidgetId(n)
    }

    /// A panel with one of each widget kind.
    fn sample() -> Vec<Widget> {
        vec![
            Widget::heading("Appearance"),
            Widget::select(
                id(1),
                "Theme",
                vec!["tuz-dark".into(), "tuz-light".into()],
                0,
            ),
            Widget::stepper(id(2), "Font size", 12.0, 6.0..=32.0, 1.0, 1),
            Widget::toggle(id(3), "Ligatures", false),
            Widget::button(id(4), "Apply"),
        ]
    }

    fn laid_out() -> Ui {
        let mut ui = Ui::new();
        ui.layout(&sample(), area(), 20);
        ui
    }

    fn center_of(ui: &Ui, widget: WidgetId) -> (i32, i32) {
        let r = ui.rect_of(widget).expect("widget should be laid out");
        (r.center_x(), r.center_y())
    }

    // --- layout -----------------------------------------------------------

    #[test]
    fn layout_places_every_widget_inside_the_area() {
        let ui = laid_out();
        assert_eq!(ui.placed().len(), 5);
        for placed in ui.placed() {
            assert!(placed.rect.x >= area().x);
            assert!(placed.rect.right() <= area().right());
        }
    }

    #[test]
    fn rows_do_not_overlap_and_run_top_to_bottom() {
        let ui = laid_out();
        for pair in ui.placed().windows(2) {
            assert!(
                pair[1].rect.y >= pair[0].rect.bottom(),
                "row at {} overlaps the one ending at {}",
                pair[1].rect.y,
                pair[0].rect.bottom()
            );
        }
    }

    #[test]
    fn a_heading_gets_space_above_it_but_not_the_first_one() {
        let widgets = vec![
            Widget::heading("First"),
            Widget::toggle(id(1), "a", false),
            Widget::heading("Second"),
            Widget::toggle(id(2), "b", false),
        ];
        let mut ui = Ui::new();
        let metrics = Metrics::from_cell_height(20);
        ui.layout_with(&widgets, area(), metrics);

        let p = ui.placed();
        // The first heading sits at the padding, with no extra space.
        assert_eq!(p[0].rect.y, area().y + metrics.padding as i32);
        // The second gets `heading_space` beyond the normal row advance.
        let normal_advance = metrics.row_height + metrics.row_gap;
        assert_eq!(
            p[2].rect.y - p[1].rect.y,
            (normal_advance + metrics.heading_space) as i32
        );
    }

    #[test]
    fn the_value_column_is_right_aligned_within_the_row() {
        let ui = laid_out();
        let placed = &ui.placed()[1];
        assert_eq!(placed.value_rect.right(), placed.rect.right());
        assert!(
            placed.value_rect.x > placed.rect.x,
            "value sits on the right"
        );
    }

    #[test]
    fn a_narrow_panel_does_not_produce_a_value_column_wider_than_the_row() {
        let mut ui = Ui::new();
        ui.layout(&sample(), Rect::new(0, 0, 60, 400), 20);
        for placed in ui.placed() {
            assert!(
                placed.value_rect.width <= placed.rect.width,
                "value column {} exceeds row {}",
                placed.value_rect.width,
                placed.rect.width
            );
        }
    }

    #[test]
    fn content_height_covers_every_row() {
        let ui = laid_out();
        let lowest = ui.placed().iter().map(|p| p.rect.bottom()).max().unwrap();
        assert!(
            ui.content_height() as i32 >= lowest - area().y,
            "content height {} does not reach the last row at {lowest}",
            ui.content_height()
        );
    }

    // --- hit testing ------------------------------------------------------

    #[test]
    fn hit_finds_interactive_widgets() {
        let ui = laid_out();
        let (x, y) = center_of(&ui, id(3));
        assert_eq!(ui.hit(x, y), Some(id(3)));
    }

    #[test]
    fn a_label_is_never_hit() {
        // Otherwise clicking a section heading would steal focus from a control.
        let ui = laid_out();
        let heading = ui.placed()[0].rect;
        assert_eq!(ui.hit(heading.center_x(), heading.center_y()), None);
    }

    #[test]
    fn a_disabled_button_is_never_hit() {
        let widgets = vec![Widget::disabled_button(id(9), "Save")];
        let mut ui = Ui::new();
        ui.layout(&widgets, area(), 20);

        let rect = ui.placed()[0].rect;
        assert_eq!(ui.hit(rect.center_x(), rect.center_y()), None);
        assert_eq!(ui.click(rect.center_x(), rect.center_y()), None);
    }

    #[test]
    fn a_point_outside_every_row_hits_nothing() {
        let ui = laid_out();
        assert_eq!(ui.hit(-10, -10), None);
        assert_eq!(ui.hit(10_000, 10_000), None);
    }

    #[test]
    fn pointer_movement_reports_only_real_changes() {
        // The caller redraws on `true`, so a false positive here means repainting
        // continuously while the mouse moves.
        let mut ui = laid_out();
        let (x, y) = center_of(&ui, id(3));

        assert!(ui.set_pointer(x, y), "entering a widget is a change");
        assert!(!ui.set_pointer(x, y + 1), "still the same widget");
        assert!(ui.set_pointer(-100, -100), "leaving is a change");
        assert!(!ui.set_pointer(-200, -200), "still outside");
    }

    #[test]
    fn clearing_the_pointer_reports_whether_it_had_to() {
        let mut ui = laid_out();
        let (x, y) = center_of(&ui, id(3));
        ui.set_pointer(x, y);
        assert!(ui.clear_pointer());
        assert!(!ui.clear_pointer(), "already clear");
    }

    // --- clicking ---------------------------------------------------------

    #[test]
    fn clicking_a_button_reports_a_press() {
        let mut ui = laid_out();
        let (x, y) = center_of(&ui, id(4));
        assert_eq!(ui.click(x, y), Some(UiAction::Pressed(id(4))));
    }

    #[test]
    fn clicking_a_toggle_reports_the_value_it_should_become() {
        let mut ui = laid_out();
        let (x, y) = center_of(&ui, id(3));
        assert_eq!(ui.click(x, y), Some(UiAction::Toggled(id(3), true)));
    }

    #[test]
    fn clicking_also_moves_focus_there() {
        // Keyboard and mouse must not disagree about which widget is current.
        let mut ui = laid_out();
        let (x, y) = center_of(&ui, id(4));
        ui.click(x, y);
        assert_eq!(ui.focused(), Some(id(4)));
    }

    #[test]
    fn which_half_of_the_value_column_was_clicked_sets_the_direction() {
        let mut ui = laid_out();
        let value = ui.placed()[2].value_rect; // the stepper

        let up = ui.click(value.right() - 2, value.center_y());
        assert_eq!(up, Some(UiAction::Changed(id(2), 13.0)));

        let down = ui.click(value.x + 2, value.center_y());
        assert_eq!(down, Some(UiAction::Changed(id(2), 11.0)));
    }

    #[test]
    fn clicking_the_label_side_focuses_without_changing_the_value() {
        // A click meant to select a row must not also modify it.
        let mut ui = laid_out();
        let row = ui.placed()[2].rect;
        let value = ui.placed()[2].value_rect;

        let x = (row.x + value.x) / 2; // left of the value column
        assert_eq!(ui.click(x, row.center_y()), None);
        assert_eq!(ui.focused(), Some(id(2)), "but focus still moved");
    }

    // --- steppers ---------------------------------------------------------

    #[test]
    fn a_stepper_clamps_to_its_range() {
        let widgets = vec![Widget::stepper(id(1), "Size", 31.5, 6.0..=32.0, 1.0, 1)];
        let mut ui = Ui::new();
        ui.layout(&widgets, area(), 20);
        ui.focus(id(1));

        // Stepping up would reach 32.5; it must stop at the maximum.
        match ui.key(UiKey::Right) {
            KeyResponse::Action(UiAction::Changed(_, v)) => assert_eq!(v, 32.0),
            other => panic!("expected a clamped change, got {other:?}"),
        }
    }

    #[test]
    fn a_stepper_at_its_limit_emits_nothing() {
        // Emitting a no-op change would mark the config dirty and enable Save for a
        // change that did not happen.
        let widgets = vec![Widget::stepper(id(1), "Size", 32.0, 6.0..=32.0, 1.0, 1)];
        let mut ui = Ui::new();
        ui.layout(&widgets, area(), 20);
        ui.focus(id(1));

        assert_eq!(ui.key(UiKey::Right), KeyResponse::Consumed);
    }

    #[test]
    fn stepper_values_display_with_the_requested_precision() {
        let integer = Widget::stepper(id(1), "Lines", 10000.0, 0.0..=1e6, 1000.0, 0);
        assert_eq!(integer.value_text().unwrap(), "‹ 10000 ›");

        let decimal = Widget::stepper(id(2), "Opacity", 0.95, 0.0..=1.0, 0.05, 2);
        assert_eq!(decimal.value_text().unwrap(), "‹ 0.95 ›");
    }

    // --- selects ----------------------------------------------------------

    #[test]
    fn a_select_wraps_at_both_ends() {
        let widgets = vec![Widget::select(
            id(1),
            "Theme",
            vec!["a".into(), "b".into(), "c".into()],
            0,
        )];
        let mut ui = Ui::new();
        ui.layout(&widgets, area(), 20);
        ui.focus(id(1));

        // Backwards from the first wraps to the last.
        match ui.key(UiKey::Left) {
            KeyResponse::Action(UiAction::Selected(_, i)) => assert_eq!(i, 2),
            other => panic!("expected a wrap, got {other:?}"),
        }

        // And forwards from the last wraps to the first.
        let widgets = vec![Widget::select(
            id(1),
            "Theme",
            vec!["a".into(), "b".into(), "c".into()],
            2,
        )];
        ui.layout(&widgets, area(), 20);
        ui.focus(id(1));
        match ui.key(UiKey::Right) {
            KeyResponse::Action(UiAction::Selected(_, i)) => assert_eq!(i, 0),
            other => panic!("expected a wrap, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_select_is_inert_rather_than_panicking() {
        // Reachable in practice: the font family list is empty if no font enumerates.
        let widgets = vec![Widget::select(id(1), "Font", vec![], 0)];
        let mut ui = Ui::new();
        ui.layout(&widgets, area(), 20);
        ui.focus(id(1));

        assert_eq!(ui.key(UiKey::Right), KeyResponse::Consumed);
        assert_eq!(ui.placed()[0].widget.value_text().unwrap(), "‹ — ›");
    }

    #[test]
    fn a_stale_select_index_is_clamped_at_construction() {
        // A caller holding an index from a longer list must not make later code
        // index out of bounds.
        let w = Widget::select(id(1), "Theme", vec!["only".into()], 7);
        match w {
            Widget::Select { index, .. } => assert_eq!(index, 0),
            _ => unreachable!(),
        }
    }

    // --- focus ------------------------------------------------------------

    #[test]
    fn tab_moves_forward_skipping_labels() {
        let mut ui = laid_out();
        for expected in [id(1), id(2), id(3), id(4)] {
            ui.key(UiKey::Tab);
            assert_eq!(ui.focused(), Some(expected));
        }
    }

    #[test]
    fn tab_wraps_at_the_end() {
        let mut ui = laid_out();
        for _ in 0..4 {
            ui.key(UiKey::Tab);
        }
        assert_eq!(ui.focused(), Some(id(4)));
        ui.key(UiKey::Tab);
        assert_eq!(ui.focused(), Some(id(1)), "should wrap to the first");
    }

    #[test]
    fn shift_tab_from_nowhere_enters_at_the_last_control() {
        let mut ui = laid_out();
        ui.key(UiKey::ShiftTab);
        assert_eq!(ui.focused(), Some(id(4)));
    }

    #[test]
    fn shift_tab_reverses() {
        let mut ui = laid_out();
        ui.focus(id(3));
        ui.key(UiKey::ShiftTab);
        assert_eq!(ui.focused(), Some(id(2)));
    }

    #[test]
    fn up_and_down_move_focus_like_tab() {
        let mut ui = laid_out();
        ui.key(UiKey::Down);
        assert_eq!(ui.focused(), Some(id(1)));
        ui.key(UiKey::Down);
        assert_eq!(ui.focused(), Some(id(2)));
        ui.key(UiKey::Up);
        assert_eq!(ui.focused(), Some(id(1)));
    }

    #[test]
    fn focus_skips_disabled_buttons() {
        let widgets = vec![
            Widget::toggle(id(1), "a", false),
            Widget::disabled_button(id(2), "Save"),
            Widget::button(id(3), "Apply"),
        ];
        let mut ui = Ui::new();
        ui.layout(&widgets, area(), 20);

        ui.key(UiKey::Tab);
        assert_eq!(ui.focused(), Some(id(1)));
        ui.key(UiKey::Tab);
        assert_eq!(ui.focused(), Some(id(3)), "the disabled button is skipped");
    }

    #[test]
    fn focus_survives_a_relayout_that_moves_the_widget() {
        // Focus is kept by id, not index: a section growing above the focused row
        // must not steal focus from it.
        let mut ui = laid_out();
        ui.focus(id(3));

        let mut grown = vec![
            Widget::heading("New group"),
            Widget::toggle(id(9), "x", false),
        ];
        grown.extend(sample());
        ui.layout(&grown, area(), 20);

        assert_eq!(ui.focused(), Some(id(3)), "focus should follow the widget");
    }

    #[test]
    fn focus_is_dropped_when_its_widget_disappears() {
        let mut ui = laid_out();
        ui.focus(id(3));
        ui.layout(&[Widget::button(id(4), "Apply")], area(), 20);
        assert_eq!(ui.focused(), None);
    }

    #[test]
    fn focusing_a_label_or_an_unknown_id_fails() {
        let mut ui = laid_out();
        assert!(!ui.focus(id(999)));
        assert_eq!(ui.focused(), None);
    }

    #[test]
    fn a_panel_with_no_interactive_widgets_has_no_focus() {
        let mut ui = Ui::new();
        ui.layout(&[Widget::heading("Nothing here")], area(), 20);
        ui.key(UiKey::Tab);
        assert_eq!(ui.focused(), None);
    }

    // --- keys -------------------------------------------------------------

    #[test]
    fn escape_asks_the_caller_to_close() {
        let mut ui = laid_out();
        assert_eq!(ui.key(UiKey::Escape), KeyResponse::Close);
    }

    #[test]
    fn enter_activates_the_focused_widget() {
        let mut ui = laid_out();
        ui.focus(id(4));
        assert_eq!(
            ui.key(UiKey::Activate),
            KeyResponse::Action(UiAction::Pressed(id(4)))
        );

        ui.focus(id(3));
        assert_eq!(
            ui.key(UiKey::Activate),
            KeyResponse::Action(UiAction::Toggled(id(3), true))
        );
    }

    #[test]
    fn enter_on_a_value_widget_advances_it() {
        let mut ui = laid_out();
        ui.focus(id(1));
        assert_eq!(
            ui.key(UiKey::Activate),
            KeyResponse::Action(UiAction::Selected(id(1), 1))
        );
    }

    #[test]
    fn left_and_right_set_a_toggle_rather_than_flipping_it() {
        // Pressing Right twice must not turn a setting back off.
        let mut ui = laid_out();
        ui.focus(id(3));

        assert_eq!(
            ui.key(UiKey::Right),
            KeyResponse::Action(UiAction::Toggled(id(3), true))
        );
        // The widget still reads `false` until the caller applies the change and
        // rebuilds, so asking again is a no-op rather than a flip back.
        let widgets = vec![Widget::toggle(id(3), "Ligatures", true)];
        ui.layout(&widgets, area(), 20);
        ui.focus(id(3));
        assert_eq!(ui.key(UiKey::Right), KeyResponse::Consumed);
        assert_eq!(
            ui.key(UiKey::Left),
            KeyResponse::Action(UiAction::Toggled(id(3), false))
        );
    }

    #[test]
    fn keys_with_no_focus_are_consumed_without_acting() {
        // The panel swallows input while open; an unfocused key must not fall
        // through to the terminal.
        let mut ui = laid_out();
        assert_eq!(ui.key(UiKey::Activate), KeyResponse::Consumed);
        assert_eq!(ui.key(UiKey::Right), KeyResponse::Consumed);
    }

    #[test]
    fn actions_report_the_widget_they_came_from() {
        assert_eq!(UiAction::Pressed(id(7)).id(), id(7));
        assert_eq!(UiAction::Toggled(id(7), true).id(), id(7));
        assert_eq!(UiAction::Selected(id(7), 2).id(), id(7));
        assert_eq!(UiAction::Changed(id(7), 1.0).id(), id(7));
    }

    #[test]
    fn interactivity_is_reported_per_kind() {
        assert!(!Widget::label("x").is_interactive());
        assert!(!Widget::heading("x").is_interactive());
        assert!(Widget::button(id(1), "x").is_interactive());
        assert!(!Widget::disabled_button(id(1), "x").is_interactive());
        assert!(Widget::toggle(id(1), "x", false).is_interactive());
    }

    #[test]
    fn metrics_scale_with_the_cell_height() {
        // A fixed pixel panel looks wrong at other font sizes.
        let small = Metrics::from_cell_height(12);
        let large = Metrics::from_cell_height(30);
        assert!(large.row_height > small.row_height);
        assert!(large.padding > small.padding);
        assert!(large.value_width > small.value_width);
    }
}

#[cfg(test)]
mod scroll_tests {
    use super::*;

    /// More rows than the viewport can show.
    fn many(n: u32) -> Vec<Widget> {
        (0..n)
            .map(|i| Widget::toggle(WidgetId(i), format!("row {i}"), false))
            .collect()
    }

    /// A viewport deliberately shorter than the content.
    fn small() -> Rect {
        Rect::new(0, 0, 400, 120)
    }

    fn scrolled(n: u32) -> Ui {
        let mut ui = Ui::new();
        ui.layout(&many(n), small(), 20);
        ui
    }

    #[test]
    fn content_shorter_than_the_viewport_does_not_scroll() {
        let mut ui = Ui::new();
        ui.layout(&many(1), Rect::new(0, 0, 400, 600), 20);
        assert!(!ui.is_scrollable());
        assert_eq!(ui.max_scroll(), 0);
        assert!(!ui.scroll_by(100), "there is nowhere to scroll");
    }

    #[test]
    fn content_taller_than_the_viewport_scrolls() {
        // The shipped panel had ~20 rows in a viewport this size and silently hid the
        // last seven, Save button included.
        let ui = scrolled(20);
        assert!(ui.is_scrollable());
        assert!(ui.max_scroll() > 0);
    }

    #[test]
    fn scrolling_moves_rows_up_by_exactly_the_offset() {
        let mut ui = scrolled(20);
        let before = ui.placed()[0].rect.y;

        ui.scroll_by(40);
        ui.layout(&many(20), small(), 20);
        assert_eq!(ui.placed()[0].rect.y, before - 40);
    }

    #[test]
    fn scrolling_is_clamped_at_both_ends() {
        let mut ui = scrolled(20);

        assert!(!ui.scroll_by(-100), "already at the top");
        assert_eq!(ui.scroll(), 0);

        ui.scroll_by(100_000);
        assert_eq!(ui.scroll(), ui.max_scroll(), "cannot scroll past the end");
        assert!(!ui.scroll_by(10), "and stays there");
    }

    #[test]
    fn the_last_row_can_reach_the_viewport() {
        // The property that actually matters: at maximum scroll, the final row —
        // which is the Save button in the real panel — must be visible.
        let mut ui = scrolled(20);
        ui.scroll_by(ui.max_scroll() as i32);
        ui.layout(&many(20), small(), 20);

        let last = ui.placed().last().unwrap().rect;
        assert!(
            last.bottom() <= small().bottom(),
            "the last row ends at {} but the viewport ends at {}",
            last.bottom(),
            small().bottom()
        );
        assert!(last.y >= small().y, "and is not scrolled off the top");
    }

    #[test]
    fn content_height_is_measured_independently_of_the_scroll_offset() {
        // Measuring the scrolled positions would make the content appear to shrink as
        // it scrolls, and the clamp would then drift toward zero.
        let mut ui = scrolled(20);
        let unscrolled = ui.content_height();

        ui.scroll_by(60);
        ui.layout(&many(20), small(), 20);
        assert_eq!(ui.content_height(), unscrolled);
    }

    #[test]
    fn growing_the_viewport_re_clamps_the_offset() {
        // Resizing the window taller while scrolled to the end must not leave the
        // view parked past the content.
        let mut ui = scrolled(20);
        ui.scroll_by(ui.max_scroll() as i32);

        ui.layout(&many(20), Rect::new(0, 0, 400, 10_000), 20);
        assert_eq!(ui.scroll(), 0, "everything fits now");
        assert!(!ui.is_scrollable());
    }

    #[test]
    fn shrinking_the_content_re_clamps_the_offset() {
        let mut ui = scrolled(30);
        ui.scroll_by(ui.max_scroll() as i32);
        let far = ui.scroll();

        ui.layout(&many(6), small(), 20);
        assert!(ui.scroll() < far, "the offset should have been pulled back");
        assert!(ui.scroll() <= ui.max_scroll());
    }

    #[test]
    fn hit_testing_follows_the_scrolled_position() {
        // Hit-testing against unscrolled rects would make clicks land on the wrong
        // control the moment the list moves.
        let mut ui = scrolled(20);
        let rect = ui.rect_of(WidgetId(0)).unwrap();
        assert_eq!(ui.hit(rect.center_x(), rect.center_y()), Some(WidgetId(0)));

        ui.scroll_by(200);
        ui.layout(&many(20), small(), 20);

        // The first row has moved off the top, so that point now hits something else
        // or nothing — but never still row 0.
        assert_ne!(ui.hit(rect.center_x(), rect.center_y()), Some(WidgetId(0)));

        // And the row now at that position is the one hit-testing reports.
        let now = ui.hit(rect.center_x(), rect.center_y());
        if let Some(id) = now {
            let moved = ui.rect_of(id).unwrap();
            assert!(moved.contains(rect.center_x(), rect.center_y()));
        }
    }

    #[test]
    fn scroll_to_focus_pulls_a_row_below_the_fold_into_view() {
        let mut ui = scrolled(20);
        let last = WidgetId(19);
        ui.focus(last);

        assert!(
            ui.rect_of(last).unwrap().bottom() > small().bottom(),
            "the last row should start off-screen"
        );

        assert!(ui.scroll_to_focus(small()), "it should have scrolled");
        ui.layout(&many(20), small(), 20);
        assert!(
            ui.rect_of(last).unwrap().bottom() <= small().bottom(),
            "and now be visible"
        );
    }

    #[test]
    fn scroll_to_focus_pulls_a_row_above_the_fold_into_view() {
        let mut ui = scrolled(20);
        ui.scroll_by(ui.max_scroll() as i32);
        ui.layout(&many(20), small(), 20);

        ui.focus(WidgetId(0));
        assert!(ui.scroll_to_focus(small()));
        ui.layout(&many(20), small(), 20);
        assert!(ui.rect_of(WidgetId(0)).unwrap().y >= small().y);
    }

    #[test]
    fn scroll_to_focus_does_nothing_for_an_already_visible_row() {
        // Otherwise every focus move would jitter the list.
        let mut ui = scrolled(20);
        ui.focus(WidgetId(0));
        assert!(!ui.scroll_to_focus(small()));
        assert_eq!(ui.scroll(), 0);
    }

    #[test]
    fn scroll_to_focus_with_no_focus_is_a_no_op() {
        let mut ui = scrolled(20);
        assert!(!ui.scroll_to_focus(small()));
    }

    #[test]
    fn tabbing_through_every_row_keeps_the_focused_one_visible() {
        // The end-to-end property: keyboard navigation must never move the focus ring
        // somewhere the user cannot see.
        let widgets = many(20);
        let mut ui = Ui::new();
        ui.layout(&widgets, small(), 20);

        for step in 0..20 {
            ui.key(UiKey::Tab);
            ui.scroll_to_focus(small());
            ui.layout(&widgets, small(), 20);

            let focused = ui.focused().expect("tab should focus something");
            let rect = ui.rect_of(focused).unwrap();
            assert!(
                rect.y >= small().y && rect.bottom() <= small().bottom(),
                "step {step}: focused row {rect:?} is outside the viewport {:?}",
                small()
            );
        }
    }
}

#[cfg(test)]
mod text_input_tests {
    use super::*;

    fn field(value: &str) -> Ui {
        let mut ui = Ui::new();
        ui.layout(
            &[Widget::text(WidgetId(1), "Program", value, "$SHELL")],
            Rect::new(0, 0, 400, 200),
            20,
        );
        ui.focus(WidgetId(1));
        ui
    }

    fn value_of(ui: &Ui) -> String {
        match &ui.placed()[0].widget {
            Widget::Text { value, .. } => value.clone(),
            other => panic!("expected a text field, got {other:?}"),
        }
    }

    fn caret_of(ui: &Ui) -> usize {
        match &ui.placed()[0].widget {
            Widget::Text { caret, .. } => *caret,
            other => panic!("expected a text field, got {other:?}"),
        }
    }

    #[test]
    fn the_caret_starts_at_the_end_of_an_existing_value() {
        // Someone editing an existing setting wants to append, not prepend.
        let ui = field("/bin/zsh");
        assert_eq!(caret_of(&ui), 8);
    }

    #[test]
    fn typing_inserts_at_the_caret() {
        let mut ui = field("bin");
        assert_eq!(
            ui.type_char('!'),
            Some(UiAction::Edited(WidgetId(1), "bin!".to_owned()))
        );
        assert_eq!(value_of(&ui), "bin!");
        assert_eq!(caret_of(&ui), 4);
    }

    #[test]
    fn typing_in_the_middle_inserts_rather_than_appends() {
        let mut ui = field("ac");
        ui.key(UiKey::Left);
        ui.type_char('b');
        assert_eq!(value_of(&ui), "abc");
    }

    #[test]
    fn control_characters_are_refused() {
        // Otherwise a stray escape byte ends up inside a config value.
        let mut ui = field("x");
        assert_eq!(ui.type_char('\n'), None);
        assert_eq!(ui.type_char('\t'), None);
        assert_eq!(ui.type_char('\u{1b}'), None);
        assert_eq!(value_of(&ui), "x");
    }

    #[test]
    fn typing_with_focus_elsewhere_is_not_consumed() {
        // The `None` is how the caller knows to let the key fall through.
        let mut ui = Ui::new();
        ui.layout(
            &[Widget::toggle(WidgetId(1), "Ligatures", false)],
            Rect::new(0, 0, 400, 200),
            20,
        );
        ui.focus(WidgetId(1));
        assert_eq!(ui.type_char('a'), None);
    }

    #[test]
    fn backspace_deletes_before_the_caret() {
        let mut ui = field("abc");
        assert_eq!(
            ui.key(UiKey::Backspace),
            KeyResponse::Action(UiAction::Edited(WidgetId(1), "ab".to_owned()))
        );
        assert_eq!(caret_of(&ui), 2);
    }

    #[test]
    fn backspace_at_the_start_does_nothing() {
        let mut ui = field("abc");
        ui.key(UiKey::Home);
        assert_eq!(ui.key(UiKey::Backspace), KeyResponse::Consumed);
        assert_eq!(value_of(&ui), "abc");
    }

    #[test]
    fn delete_removes_after_the_caret() {
        let mut ui = field("abc");
        ui.key(UiKey::Home);
        assert_eq!(
            ui.key(UiKey::Delete),
            KeyResponse::Action(UiAction::Edited(WidgetId(1), "bc".to_owned()))
        );
        assert_eq!(caret_of(&ui), 0, "the caret should not move");
    }

    #[test]
    fn delete_at_the_end_does_nothing() {
        let mut ui = field("abc");
        assert_eq!(ui.key(UiKey::Delete), KeyResponse::Consumed);
        assert_eq!(value_of(&ui), "abc");
    }

    #[test]
    fn home_and_end_move_the_caret_to_the_edges() {
        let mut ui = field("hello");
        ui.key(UiKey::Home);
        assert_eq!(caret_of(&ui), 0);
        ui.key(UiKey::End);
        assert_eq!(caret_of(&ui), 5);
    }

    #[test]
    fn arrows_move_the_caret_and_clamp_at_both_ends() {
        let mut ui = field("ab");
        ui.key(UiKey::Left);
        assert_eq!(caret_of(&ui), 1);
        ui.key(UiKey::Left);
        ui.key(UiKey::Left);
        assert_eq!(caret_of(&ui), 0, "cannot go before the start");

        ui.key(UiKey::Right);
        ui.key(UiKey::Right);
        ui.key(UiKey::Right);
        assert_eq!(caret_of(&ui), 2, "cannot go past the end");
    }

    #[test]
    fn arrows_in_a_text_field_do_not_emit_a_value_change() {
        // In a stepper the arrows adjust; in a text field they must only navigate.
        let mut ui = field("ab");
        assert_eq!(ui.key(UiKey::Left), KeyResponse::Consumed);
        assert_eq!(ui.key(UiKey::Right), KeyResponse::Consumed);
    }

    #[test]
    fn multi_byte_characters_are_edited_without_splitting_them() {
        // The caret counts characters but String::remove takes bytes; conflating the
        // two panics on a boundary inside a multi-byte character.
        let mut ui = field("日本語");
        assert_eq!(caret_of(&ui), 3, "three characters, nine bytes");

        assert_eq!(
            ui.key(UiKey::Backspace),
            KeyResponse::Action(UiAction::Edited(WidgetId(1), "日本".to_owned()))
        );

        ui.key(UiKey::Home);
        assert_eq!(
            ui.key(UiKey::Delete),
            KeyResponse::Action(UiAction::Edited(WidgetId(1), "本".to_owned()))
        );

        ui.type_char('é');
        assert_eq!(value_of(&ui), "é本");
    }

    #[test]
    fn editing_an_empty_field_is_safe() {
        let mut ui = field("");
        assert_eq!(caret_of(&ui), 0);
        assert_eq!(ui.key(UiKey::Backspace), KeyResponse::Consumed);
        assert_eq!(ui.key(UiKey::Delete), KeyResponse::Consumed);
        ui.key(UiKey::Left);
        ui.key(UiKey::Right);
        assert_eq!(value_of(&ui), "");
    }

    #[test]
    fn an_empty_field_displays_its_placeholder() {
        let ui = field("");
        assert_eq!(ui.placed()[0].widget.value_text().unwrap(), "$SHELL");

        let ui = field("/bin/fish");
        assert_eq!(ui.placed()[0].widget.value_text().unwrap(), "/bin/fish");
    }

    #[test]
    fn a_text_field_takes_focus_and_participates_in_tab_order() {
        let mut ui = Ui::new();
        ui.layout(
            &[
                Widget::toggle(WidgetId(1), "a", false),
                Widget::text(WidgetId(2), "Program", "", ""),
                Widget::button(WidgetId(3), "Apply"),
            ],
            Rect::new(0, 0, 400, 300),
            20,
        );
        ui.key(UiKey::Tab);
        ui.key(UiKey::Tab);
        assert_eq!(ui.focused(), Some(WidgetId(2)));
    }

    #[test]
    fn clicking_a_text_field_focuses_without_editing() {
        let mut ui = field("abc");
        let rect = ui.rect_of(WidgetId(1)).unwrap();
        assert_eq!(ui.click(rect.center_x(), rect.center_y()), None);
        assert_eq!(ui.focused(), Some(WidgetId(1)));
        assert_eq!(value_of(&ui), "abc");
    }
}

#[cfg(test)]
mod footer_tests {
    use super::*;

    fn body_widgets() -> Vec<Widget> {
        (0..40)
            .map(|i| Widget::toggle(WidgetId(i + 1), format!("row {i}"), false))
            .collect()
    }

    fn footer_widgets() -> Vec<Widget> {
        vec![
            Widget::button(WidgetId(90), "Save to config.toml"),
            Widget::button(WidgetId(91), "Revert"),
            Widget::button(WidgetId(92), "Close"),
        ]
    }

    /// A body long enough to need scrolling, plus a three-button footer.
    fn split_ui(area: Rect) -> Ui {
        let mut ui = Ui::new();
        ui.layout_split(&body_widgets(), &footer_widgets(), area, 20);
        ui
    }

    fn footer_rects(ui: &Ui) -> Vec<Rect> {
        ui.placed()[ui.body().len()..].iter().map(|p| p.rect).collect()
    }

    #[test]
    fn the_footer_stays_put_while_the_body_scrolls() {
        let area = Rect::new(0, 0, 800, 400);
        let mut ui = split_ui(area);

        let before = footer_rects(&ui);
        let first_row = ui.body()[0].rect;

        assert!(ui.scroll_by(120), "the body should be scrollable");
        // Re-lay out at the new offset, the way the next frame would.
        ui.layout_split(&body_widgets(), &footer_widgets(), area, 20);

        assert_eq!(
            before,
            footer_rects(&ui),
            "the footer is pinned; scrolling the body must not move it"
        );
        assert!(
            ui.body()[0].rect.y < first_row.y,
            "the body should have moved up"
        );
    }

    /// A label and its value must never be drawn on top of each other, at any width.
    ///
    /// This is the bug twice over: first because footer rows shared one rect, then
    /// because a value column wider than a narrow row was capped to the whole row.
    #[test]
    fn a_value_column_never_covers_its_label_at_any_width() {
        for width in [120u32, 200, 320, 480, 800, 1600] {
            let mut ui = Ui::new();
            ui.layout_split(
                &[Widget::toggle(WidgetId(1), "Some setting", false)],
                &[Widget::text(WidgetId(2), "Rename to", "Cargo.toml", "")],
                Rect::new(0, 0, width, 400),
                20,
            );

            for row in ui.placed() {
                // Separated one way or the other: beside the label, or below it.
                // What must never happen is the value starting where the label does,
                // which is the two strings drawn on top of each other.
                assert!(
                    row.value_rect.x > row.rect.x || row.value_rect.y > row.rect.y,
                    "at width {width}, {:?} draws its value over its label",
                    row.widget
                );
            }
        }
    }

    #[test]
    fn a_lone_footer_field_spans_the_band_and_keeps_its_value_column_separate() {
        let area = Rect::new(0, 0, 800, 400);
        let mut ui = Ui::new();
        ui.layout_split(
            &body_widgets(),
            &[Widget::text(WidgetId(1), "Rename to", "BUILD-LOG.md", "")],
            area,
            20,
        );

        let field = &ui.placed()[ui.body().len()];
        // Sized to the words "Rename to" it would leave a handful of characters for
        // the name, which is not a field anyone can type a filename into.
        assert!(
            field.rect.width > area.width / 2,
            "a prompt should take the band, got {}",
            field.rect.width
        );
        // A prompt in a narrow column stacks: the label takes the upper row and the
        // field the lower one, so the name gets the full width to be typed into.
        assert!(
            field.value_rect.y > field.rect.y,
            "the label should sit above the field, not beside it"
        );
        assert_eq!(field.value_rect.width, field.rect.width);
        assert!(
            field.value_rect.bottom() <= field.rect.bottom(),
            "the field must stay inside the row it was given"
        );
        // Room for both rows, or the label would be drawn into zero height.
        assert!(field.rect.height >= field.value_rect.height * 2);
    }

    #[test]
    fn a_row_of_footer_buttons_still_shares_one_rect_for_box_and_hit_area() {
        let ui = split_ui(Rect::new(0, 0, 800, 400));
        for row in &ui.placed()[ui.body().len()..] {
            // Buttons draw their box at `rect`, so the two agreeing is what keeps the
            // pressable area and the visible box identical.
            assert_eq!(row.rect, row.value_rect);
        }
    }

    #[test]
    fn footer_buttons_are_sized_to_their_labels_not_stretched() {
        let footer = footer_rects(&split_ui(Rect::new(0, 0, 800, 400)));
        assert_eq!(footer.len(), 3);

        // Equal thirds of the width would be a huge target that still reads wrong: the
        // label floats in an expanse of box with nothing marking the edges.
        assert!(
            footer[0].width > footer[1].width,
            "'Save to config.toml' is a longer label than 'Revert'"
        );
        assert!(footer[1].width > footer[2].width);

        // And they must not overlap, or the leftmost swallows its neighbours' clicks.
        assert!(footer[0].right() <= footer[1].x);
        assert!(footer[1].right() <= footer[2].x);
    }

    #[test]
    fn a_footer_button_is_clicked_by_the_rect_it_is_drawn_at() {
        let mut ui = split_ui(Rect::new(0, 0, 800, 400));
        let rect = ui.placed()[ui.body().len()].rect;

        // Every corner of the box must register, not just the middle: the drawn area
        // and the pressable area used to be different rectangles.
        for (x, y) in [
            (rect.x + 1, rect.y + 1),
            (rect.right() - 1, rect.y + 1),
            (rect.x + 1, rect.bottom() - 1),
            (rect.right() - 1, rect.bottom() - 1),
        ] {
            assert!(
                ui.hit(x, y).is_some(),
                "({x}, {y}) is inside the drawn button but does not hit it"
            );
        }
        assert!(matches!(
            ui.click(rect.x + 2, rect.y + 2),
            Some(UiAction::Pressed(_))
        ));
    }

    #[test]
    fn the_pinned_band_comes_out_of_the_scroll_viewport() {
        let area = Rect::new(0, 0, 800, 400);
        let with_footer = split_ui(area);

        let mut without = Ui::new();
        without.layout_split(&body_widgets(), &[], area, 20);

        // If the footer did not reserve its band the body would lay out underneath it,
        // and the last rows would be permanently hidden behind the buttons.
        assert!(
            with_footer.max_scroll() > without.max_scroll(),
            "reserving the band must shrink the scrolling viewport"
        );
        assert_eq!(without.footer_area().height, 0);
        assert!(with_footer.footer_area().height > 0);
    }

    #[test]
    fn footer_buttons_keep_clear_of_the_windows_bottom_edge() {
        let ui = split_ui(Rect::new(0, 0, 800, 400));
        let band = ui.footer_area();

        // The window's resize handle is a band a handful of pixels deep along each
        // edge, and it takes clicks before anything drawn there. A button flush with
        // the bottom would lose its lowest rows to it, so the footer keeps its padding
        // below the buttons rather than centring them in the band.
        for rect in footer_rects(&ui) {
            assert!(
                band.bottom() - rect.bottom() >= 8,
                "button {rect:?} comes too close to the bottom of {band:?}"
            );
        }
    }

    #[test]
    fn focusing_a_footer_button_does_not_chase_it_with_the_scroll() {
        let area = Rect::new(0, 0, 800, 400);
        let mut ui = split_ui(area);
        let scroll_area = Rect::new(
            area.x,
            area.y,
            area.width,
            area.height - ui.footer_area().height,
        );

        ui.focus(WidgetId(92));
        // The footer sits below the scroll area and is always visible, so scrolling
        // toward it would chase a target that can never enter the viewport.
        assert!(!ui.scroll_to_focus(scroll_area));
        assert_eq!(ui.scroll(), 0);
    }
}
