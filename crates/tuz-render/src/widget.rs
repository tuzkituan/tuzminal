//! Drawing widgets and the settings panel.
//!
//! Built entirely from the primitives that already exist — [`Instance::solid`] and
//! [`crate::text::draw_in_box`] — so the panel joins the same single draw call as
//! terminal content and chrome. No new pipeline, no second pass.
//!
//! Every color comes from the theme. A panel with hardcoded colors is the fastest
//! way to look broken the moment someone switches theme, and this terminal ships two.

use crate::instance::{ColorSpace, Instance};
use crate::text::{self, Align};
use tuz_config::{Rgba, Theme};
use tuz_font::{FontSystem, Style};
use tuz_layout::Rect;
use tuz_ui::{EntryKind, Placed, Ui, Widget, WidgetId};

/// Inset for text inside a row.
const PADDING: f32 = 8.0;

/// Thickness of the panel border and the focus ring.
const BORDER: f32 = 1.0;
const FOCUS_RING: f32 = 2.0;

/// Draw the panel frame: a dimming layer over the terminal, then the panel itself.
///
/// The dim layer is one quad over the whole window. It serves two purposes: it makes
/// the panel readable against arbitrary terminal content, and it signals that the
/// terminal behind is not accepting input.
pub fn draw_panel_frame(
    out: &mut Vec<Instance>,
    window: Rect,
    panel: Rect,
    theme: &Theme,
    colors: ColorSpace,
) {
    // Dim at fixed alpha rather than through `colors.convert`, which would also
    // apply window opacity and make the dim vanish on a transparent window.
    let [r, g, b, _] = if colors.srgb {
        theme.background.to_linear()
    } else {
        theme.background.to_unorm()
    };
    out.push(Instance {
        position: [window.x as f32, window.y as f32],
        size: [window.width as f32, window.height as f32],
        uv: [0.0; 4],
        color: [r, g, b, 0.72],
        flags: 0,
        corner_radius: 0.0,
        rotation: 0.0,
        stroke_width: 0.0,
    });

    // Border first, then the interior inset by the border width, which draws an
    // outline with two quads instead of four.
    out.push(Instance::solid(
        panel.x as f32,
        panel.y as f32,
        panel.width as f32,
        panel.height as f32,
        colors.convert_opaque(theme.split_divider()),
    ));
    let inner = panel.inset(BORDER as u32, BORDER as u32);
    out.push(Instance::solid(
        inner.x as f32,
        inner.y as f32,
        inner.width as f32,
        inner.height as f32,
        colors.convert_opaque(theme.background),
    ));
}

/// Draw a title bar across the top of the panel.
///
/// Returns the rect below it, which is where widgets go.
/// Fill a settings page and return the area left for content.
///
/// Unlike [`draw_panel_frame`] there is no scrim and no border: a page fills its tab,
/// so there is nothing behind it to dim and no edge to outline — the tab strip
/// already says where it begins.
/// `radius` rounds the bottom corners, for a page that runs to the bottom of a
/// borderless window. A page fills its tab edge to edge, so unlike a terminal pane —
/// whose grid is inset by the window padding — it paints the corner pixels itself and
/// would square off the window's curve if it ignored them.
pub fn draw_page_frame(
    out: &mut Vec<Instance>,
    page: Rect,
    theme: &Theme,
    colors: ColorSpace,
    radius: f32,
) {
    out.push(Instance::rounded(
        page.x as f32,
        page.y as f32,
        page.width as f32,
        page.height as f32,
        colors.convert(theme.background),
        radius,
        crate::instance::FLAG_ROUND_BOTTOM,
    ));
}

/// Draw a dropdown: a bordered box, its rows, and the selected one highlighted.
///
/// Its own function rather than a `Ui`: a menu is one column of labels that closes as
/// soon as you pick something, and routing it through the general widget layout would
/// bring scrolling, focus rings and a footer for a list of four shells.
#[allow(clippy::too_many_arguments)]
pub fn draw_menu(
    out: &mut Vec<Instance>,
    fonts: &mut FontSystem,
    rect: Rect,
    rows: &[(Rect, &str)],
    selected: usize,
    theme: &Theme,
    colors: ColorSpace,
) {
    // Border then interior, the same two-quad outline the panel frame uses.
    out.push(Instance::solid(
        rect.x as f32,
        rect.y as f32,
        rect.width as f32,
        rect.height as f32,
        colors.convert_opaque(theme.split_divider()),
    ));
    let inner = rect.inset(BORDER as u32, BORDER as u32);
    out.push(Instance::solid(
        inner.x as f32,
        inner.y as f32,
        inner.width as f32,
        inner.height as f32,
        colors.convert_opaque(theme.background_focused()),
    ));

    for (index, (row, label)) in rows.iter().enumerate() {
        if index == selected {
            out.push(Instance::solid(
                row.x as f32,
                row.y as f32,
                row.width as f32,
                row.height as f32,
                colors.convert_opaque(theme.cursor()),
            ));
        }
        text::draw_in_box(
            out,
            fonts,
            label,
            *row,
            PADDING,
            Align::Left,
            if index == selected {
                theme.background
            } else {
                theme.foreground
            },
            colors,
            Style::Regular,
        );
    }
}

/// Draw the rule separating a pinned footer from the content above it.
pub fn draw_footer_divider(
    out: &mut Vec<Instance>,
    footer: Rect,
    theme: &Theme,
    colors: ColorSpace,
    radius: f32,
) {
    if footer.height == 0 {
        return;
    }
    // The footer sits on the page's bottom edge and is drawn over it, so it inherits
    // the same rounding — otherwise it paints the curve the page just made square
    // again.
    out.push(Instance::rounded(
        footer.x as f32,
        footer.y as f32,
        footer.width as f32,
        footer.height as f32,
        colors.convert_opaque(theme.background_focused()),
        radius,
        crate::instance::FLAG_ROUND_BOTTOM,
    ));
    out.push(Instance::solid(
        footer.x as f32,
        footer.y as f32,
        footer.width as f32,
        BORDER,
        colors.convert_opaque(theme.split_divider()),
    ));
}

/// `inset` is how far the title is indented, and must be the same value the rows
/// below are laid out with — `Metrics::padding`. Deriving it here from the font
/// instead is off by whatever `line_height` is set to, and a heading indented
/// differently from the content it heads reads as a misalignment rather than a choice.
#[allow(clippy::too_many_arguments)]
pub fn draw_panel_title(
    out: &mut Vec<Instance>,
    fonts: &mut FontSystem,
    panel: Rect,
    title: &str,
    theme: &Theme,
    colors: ColorSpace,
    inset: f32,
) -> Rect {
    let height = fonts.metrics().height + 12;
    let bar = Rect::new(panel.x, panel.y, panel.width, height.min(panel.height));

    out.push(Instance::solid(
        bar.x as f32,
        bar.y as f32,
        bar.width as f32,
        bar.height as f32,
        colors.convert_opaque(theme.background_focused()),
    ));
    // A rule under the title, so it reads as a header rather than a first row.
    out.push(Instance::solid(
        bar.x as f32,
        bar.bottom() as f32 - BORDER,
        bar.width as f32,
        BORDER,
        colors.convert_opaque(theme.split_divider()),
    ));

    text::draw_in_box(
        out,
        fonts,
        title,
        bar,
        inset,
        Align::Left,
        theme.foreground,
        colors,
        Style::Bold,
    );

    Rect::new(
        panel.x,
        bar.bottom(),
        panel.width,
        panel.height.saturating_sub(bar.height),
    )
}

/// Draw every widget in `ui`.
///
/// Backgrounds for all rows go down before any text, following the same painter's
/// order rule as the cell renderer: no row's highlight can paint over a neighbour's
/// label.
pub fn draw_widgets(
    out: &mut Vec<Instance>,
    fonts: &mut FontSystem,
    ui: &Ui,
    theme: &Theme,
    colors: ColorSpace,
) {
    draw_widgets_in(out, fonts, ui.placed(), ui, theme, colors);
}

/// Draw a subset of a `Ui`'s rows.
///
/// Exists so a page can draw its scrolling body and its pinned footer as separate
/// ranges: the body is clipped to the scroll area, the footer must not be, and one
/// call covering both would have to be clipped as a unit.
pub fn draw_widgets_in(
    out: &mut Vec<Instance>,
    fonts: &mut FontSystem,
    rows: &[Placed],
    ui: &Ui,
    theme: &Theme,
    colors: ColorSpace,
) {
    let focus = ui.focused();
    let hover = ui.hovered();
    let pressed = ui.pressed();

    // Rows outside the viewport are skipped rather than drawn and scissored away.
    // Layout places every row at its natural offset with no clamping, so a directory
    // of five hundred files would rasterize five hundred rows of glyphs every frame
    // for the twenty on screen. The clip rect hides them; it does not make them free.
    let visible = ui.viewport();
    let rows: Vec<&Placed> = rows
        .iter()
        .filter(|p| intersects(p.rect, visible))
        .collect();

    // Backgrounds for every row before any text, so no row's background can paint
    // over the label of the row above it.
    for placed in &rows {
        draw_row_background(out, placed, focus, hover, pressed, theme, colors);
    }
    for placed in &rows {
        let focused = placed.widget.id().is_some() && placed.widget.id() == focus;
        let held = placed.widget.id().is_some() && placed.widget.id() == pressed;
        draw_row_text(out, fonts, placed, focused, held, theme, colors);
    }
}

/// Whether two rects share any area. A row exactly level with the edge counts as out.
fn intersects(a: Rect, b: Rect) -> bool {
    a.x < b.right() && a.right() > b.x && a.y < b.bottom() && a.bottom() > b.y
}

#[allow(clippy::too_many_arguments)]
fn draw_row_background(
    out: &mut Vec<Instance>,
    placed: &Placed,
    focus: Option<WidgetId>,
    hover: Option<WidgetId>,
    ui_pressed: Option<WidgetId>,
    theme: &Theme,
    colors: ColorSpace,
) {
    let id = placed.widget.id();
    let focused = id.is_some() && id == focus;
    let hovered = id.is_some() && id == hover;
    let pressed = id.is_some() && id == ui_pressed;

    // The selected row of a list, filled before anything else so the ring and the
    // hover highlight both sit on top of it. This is what makes an arrow keypress
    // visible: without it the selection moves and the list looks unchanged.
    if let Widget::Entry { selected: true, .. } = placed.widget {
        out.push(Instance::solid(
            placed.rect.x as f32,
            placed.rect.y as f32,
            placed.rect.width as f32,
            placed.rect.height as f32,
            colors.convert_opaque(theme.background_focused()),
        ));
    }

    // A focus ring rather than a fill: a filled focus indicator competes with the
    // hover highlight and with a button's own border.
    //
    // Skipped for a text field, which already draws a box around itself — a ring
    // around that box is two nested borders, and once the label stacks above the
    // field the outer one encircles the label too, which reads as a mistake. The
    // field's own border carries the focus color instead.
    if focused && !matches!(placed.widget, Widget::Text { .. }) {
        let ring = colors.convert_opaque(theme.cursor());
        let r = placed.rect;
        out.push(Instance::solid(
            r.x as f32,
            r.y as f32,
            r.width as f32,
            FOCUS_RING,
            ring,
        ));
        out.push(Instance::solid(
            r.x as f32,
            r.bottom() as f32 - FOCUS_RING,
            r.width as f32,
            FOCUS_RING,
            ring,
        ));
        out.push(Instance::solid(
            r.x as f32,
            r.y as f32,
            FOCUS_RING,
            r.height as f32,
            ring,
        ));
        out.push(Instance::solid(
            r.right() as f32 - FOCUS_RING,
            r.y as f32,
            FOCUS_RING,
            r.height as f32,
            ring,
        ));
    }

    if hovered {
        out.push(Instance::solid(
            placed.rect.x as f32,
            placed.rect.y as f32,
            placed.rect.width as f32,
            placed.rect.height as f32,
            colors.convert_opaque(theme.background_focused()),
        ));
    }

    // A text field gets a sunken box so it reads as editable rather than as a value
    // someone forgot to make a control.
    if let Widget::Text { .. } = placed.widget {
        let r = placed.value_rect;
        out.push(Instance::solid(
            r.x as f32,
            r.y as f32,
            r.width as f32,
            r.height as f32,
            // The border doubles as the focus indicator, which is why the ring above
            // skips this widget.
            colors.convert_opaque(if focused {
                theme.cursor()
            } else {
                theme.split_divider()
            }),
        ));
        let inner = r.inset(BORDER as u32, BORDER as u32);
        out.push(Instance::solid(
            inner.x as f32,
            inner.y as f32,
            inner.width as f32,
            inner.height as f32,
            colors.convert_opaque(theme.background),
        ));
    }

    // A button gets a visible box so it reads as pressable, which a bare label does
    // not. Disabled buttons get the box too, just dimmed by their text color.
    if let Widget::Button { .. } = placed.widget {
        let r = button_rect(placed);
        out.push(Instance::solid(
            r.x as f32,
            r.y as f32,
            r.width as f32,
            r.height as f32,
            colors.convert_opaque(theme.split_divider()),
        ));
        let inner = r.inset(BORDER as u32, BORDER as u32);
        out.push(Instance::solid(
            inner.x as f32,
            inner.y as f32,
            inner.width as f32,
            inner.height as f32,
            colors.convert_opaque(if pressed {
                // Inverted, the same signal the toolbar buttons use. Hover alone
                // cannot show a click: the pointer is already there.
                theme.cursor()
            } else if hovered {
                theme.background_focused()
            } else {
                theme.background
            }),
        ));
    }
}

/// Buttons occupy the value column rather than the whole row, so a row of them does
/// not look like a row of full-width bars.
/// A button's box is its whole row.
///
/// It used to be `value_rect`, the narrow right-hand column, while hit-testing used
/// the full row — so the visible box and the clickable area were different rectangles
/// and neither one explained the other. They are the same rect now, which is the only
/// arrangement where what you see is what you can press.
fn button_rect(placed: &Placed) -> Rect {
    placed.rect
}

#[allow(clippy::too_many_arguments)]
fn draw_row_text(
    out: &mut Vec<Instance>,
    fonts: &mut FontSystem,
    placed: &Placed,
    focused: bool,
    pressed: bool,
    theme: &Theme,
    colors: ColorSpace,
) {
    match &placed.widget {
        Widget::Label { text, heading } => {
            text::draw_in_box(
                out,
                fonts,
                text,
                placed.rect,
                PADDING,
                Align::Left,
                // A heading in the cursor color reads as a section marker; body
                // labels stay in the normal foreground.
                if *heading {
                    theme.cursor()
                } else {
                    theme.foreground
                },
                colors,
                if *heading {
                    Style::Bold
                } else {
                    Style::Regular
                },
            );
        }

        // What it does on the left, the keys on the right. The keys are dimmed
        // because you scan the descriptions and only then read across.
        Widget::Shortcut { label, keys } => {
            text::draw_in_box(
                out,
                fonts,
                label,
                placed.rect,
                PADDING,
                Align::Left,
                theme.foreground,
                colors,
                Style::Regular,
            );
            text::draw_in_box(
                out,
                fonts,
                keys,
                placed.rect,
                PADDING,
                Align::Right,
                theme.bright.black,
                colors,
                Style::Regular,
            );
        }

        // An explicit arm, not because the wildcard below would fail to compile —
        // it would not, and that is the danger. It ends in `_ => ""`, so a new
        // variant renders as a blank row with no error anywhere.
        Widget::Entry {
            kind,
            label,
            detail,
            ..
        } => {
            let icon_width = placed.rect.height as f32;
            let icon = Rect::new(
                placed.rect.x,
                placed.rect.y,
                icon_width as u32,
                placed.rect.height,
            );
            crate::icon::entry(
                out,
                *kind,
                icon,
                colors.convert_opaque(match kind {
                    EntryKind::Directory | EntryKind::Parent => theme.cursor(),
                    EntryKind::Symlink => theme.normal.cyan,
                    EntryKind::File => theme.bright.black,
                }),
            );

            // The detail column is measured off the right, and the name gets what is
            // left — a long file name should crowd the size, not overrun it.
            let detail_width = text::measure(detail, fonts.metrics().width as f32);
            let name = Rect::new(
                icon.right(),
                placed.rect.y,
                placed
                    .rect
                    .width
                    .saturating_sub(icon.width)
                    .saturating_sub(detail_width as u32 + PADDING as u32),
                placed.rect.height,
            );
            text::draw_in_box(
                out,
                fonts,
                label,
                name,
                PADDING / 2.0,
                Align::Left,
                theme.foreground,
                colors,
                Style::Regular,
            );

            if !detail.is_empty() {
                text::draw_in_box(
                    out,
                    fonts,
                    detail,
                    placed.rect,
                    PADDING,
                    Align::Right,
                    theme.bright.black,
                    colors,
                    Style::Regular,
                );
            }
        }

        Widget::Button { label, enabled, .. } => {
            text::draw_in_box(
                out,
                fonts,
                label,
                button_rect(placed),
                PADDING,
                Align::Center,
                if pressed {
                    // The fill inverted to the accent colour, so the label has to
                    // invert with it or it disappears into its own button.
                    theme.background
                } else if *enabled {
                    theme.foreground
                } else {
                    // Dimmed rather than hidden: a disabled button should still say
                    // what it would do.
                    theme.bright.black
                },
                colors,
                Style::Regular,
            );
        }

        Widget::Text {
            label,
            value,
            caret,
            placeholder,
            ..
        } => {
            // The label goes wherever the value is not. Stacked, that is the band
            // above the field; side by side, it is the whole row with the value
            // right-aligned over it. One rule, both layouts.
            let label_rect = if placed.value_rect.y > placed.rect.y {
                Rect::new(
                    placed.rect.x,
                    placed.rect.y,
                    placed.rect.width,
                    (placed.value_rect.y - placed.rect.y) as u32,
                )
            } else {
                placed.rect
            };
            text::draw_in_box(
                out,
                fonts,
                label,
                label_rect,
                PADDING,
                Align::Left,
                theme.foreground,
                colors,
                Style::Regular,
            );

            let shown = if value.is_empty() { placeholder } else { value };
            text::draw_in_box(
                out,
                fonts,
                shown,
                placed.value_rect,
                PADDING,
                Align::Left,
                if value.is_empty() {
                    // A placeholder that looks like a value is worse than none.
                    theme.bright.black
                } else {
                    theme.foreground
                },
                colors,
                Style::Regular,
            );

            // The caret is drawn only when the field has focus: a bar in an
            // unfocused field suggests typing would go there.
            if focused {
                let cell = fonts.metrics().width as f32;
                let x = placed.value_rect.x as f32 + PADDING + *caret as f32 * cell;
                // Clamped so a caret past the visible width sits at the edge rather
                // than drawing outside the field.
                let limit = placed.value_rect.right() as f32 - PADDING;
                out.push(Instance::solid(
                    x.min(limit),
                    placed.value_rect.y as f32 + 4.0,
                    2.0,
                    (placed.value_rect.height as f32 - 8.0).max(1.0),
                    colors.convert_opaque(theme.cursor()),
                ));
            }
        }

        _ => {
            // Label on the left, value on the right.
            let label = match &placed.widget {
                Widget::Toggle { label, .. }
                | Widget::Select { label, .. }
                | Widget::Stepper { label, .. } => label.as_str(),
                _ => "",
            };
            text::draw_in_box(
                out,
                fonts,
                label,
                placed.rect,
                PADDING,
                Align::Left,
                theme.foreground,
                colors,
                Style::Regular,
            );

            if let Some(value) = placed.widget.value_text() {
                text::draw_in_box(
                    out,
                    fonts,
                    &value,
                    placed.value_rect,
                    PADDING,
                    Align::Right,
                    // Values in the cursor color so the editable part of each row is
                    // obvious at a glance.
                    theme.cursor(),
                    colors,
                    Style::Regular,
                );
            }
        }
    }
}

/// A transient message shown over the terminal.
pub struct Toast<'a> {
    pub text: &'a str,
    /// Tints the left edge: info, warning or error.
    pub accent: Rgba,
    /// 0.0 fully faded, 1.0 fully opaque. Drives the fade-out.
    pub opacity: f32,
}

/// Draw toasts stacked from the bottom-right.
///
/// Bottom-right because the top-left is where terminal output actually is; a
/// notification over the prompt would cover the thing the user is looking at.
pub fn draw_toasts(
    out: &mut Vec<Instance>,
    fonts: &mut FontSystem,
    toasts: &[Toast<'_>],
    window: Rect,
    theme: &Theme,
    colors: ColorSpace,
) {
    if toasts.is_empty() {
        return;
    }

    let metrics = fonts.metrics();
    let height = metrics.height + 12;
    let margin = 12;
    let accent_width = 3.0;
    let mut bottom = window.bottom() - margin;

    for toast in toasts {
        let text_width = text::measure(toast.text, metrics.width as f32) as u32;
        let width = (text_width + (PADDING * 4.0) as u32).min(window.width.saturating_sub(24));
        let rect = Rect::new(
            window.right() - width as i32 - margin,
            bottom - height as i32,
            width,
            height,
        );
        // Stop stacking once they would run off the top rather than drawing over
        // the tab bar.
        if rect.y < window.y {
            break;
        }

        let fade = |mut c: [f32; 4]| {
            c[3] *= toast.opacity.clamp(0.0, 1.0);
            c
        };

        out.push(Instance::solid(
            rect.x as f32,
            rect.y as f32,
            rect.width as f32,
            rect.height as f32,
            fade(colors.convert_opaque(theme.background_focused())),
        ));
        // Accent stripe down the left edge, which is how the severity reads at a
        // glance without colouring the whole box.
        out.push(Instance::solid(
            rect.x as f32,
            rect.y as f32,
            accent_width,
            rect.height as f32,
            fade(colors.convert_opaque(toast.accent)),
        ));

        let text_rect = Rect::new(
            rect.x + accent_width as i32,
            rect.y,
            rect.width.saturating_sub(accent_width as u32),
            rect.height,
        );
        let mut color = colors.convert_opaque(theme.foreground);
        color[3] *= toast.opacity.clamp(0.0, 1.0);

        let shown = text::truncate(
            toast.text,
            text_rect.width as f32 - PADDING * 2.0,
            metrics.width as f32,
        );
        let baseline = rect.y as f32
            + ((rect.height as f32 - metrics.height as f32) / 2.0).max(0.0)
            + metrics.ascent;
        text::draw(
            out,
            fonts,
            &shown,
            text_rect.x as f32 + PADDING,
            baseline,
            theme.foreground,
            ColorSpace {
                srgb: colors.srgb,
                opacity: toast.opacity,
            },
            Style::Regular,
        );

        bottom = rect.y - 6;
    }
}

/// Draw a scrollbar on the right of `area`, if the content overflows.
///
/// Without this there is no indication that anything is below the fold, which is how
/// a panel silently hides its own Save button.
pub fn draw_scrollbar(
    out: &mut Vec<Instance>,
    ui: &Ui,
    area: Rect,
    theme: &Theme,
    colors: ColorSpace,
) {
    if !ui.is_scrollable() || area.height == 0 {
        return;
    }

    let width = 4.0;
    let x = area.right() as f32 - width;

    // Track.
    out.push(Instance::solid(
        x,
        area.y as f32,
        width,
        area.height as f32,
        colors.convert_opaque(theme.split_divider()),
    ));

    // Thumb, sized by the visible fraction and floored so it stays grabbable when
    // the content is very long.
    let visible = area.height as f32 / ui.content_height().max(1) as f32;
    let thumb_height = (area.height as f32 * visible).max(16.0);
    let travel = area.height as f32 - thumb_height;
    let progress = ui.scroll() as f32 / ui.max_scroll().max(1) as f32;

    out.push(Instance::solid(
        x,
        area.y as f32 + travel * progress,
        width,
        thumb_height,
        colors.convert_opaque(theme.bright.black),
    ));
}

/// Centre a panel of the given size within `window`, clamped to fit.
pub fn center_panel(window: Rect, width: u32, height: u32) -> Rect {
    let w = width.min(window.width);
    let h = height.min(window.height);
    Rect::new(
        window.x + ((window.width.saturating_sub(w)) / 2) as i32,
        window.y + ((window.height.saturating_sub(h)) / 2) as i32,
        w,
        h,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuz_ui::UiKey;

    fn fonts() -> FontSystem {
        FontSystem::new(
            &tuz_config::Font {
                family: "monospace".to_owned(),
                size: 14.0,
                ..Default::default()
            },
            1.0,
        )
        .expect("a monospace font is required for these tests")
    }

    fn colors() -> ColorSpace {
        ColorSpace {
            srgb: false,
            opacity: 1.0,
        }
    }

    fn sample_ui() -> Ui {
        let widgets = vec![
            Widget::heading("Appearance"),
            Widget::toggle(WidgetId(1), "Ligatures", false),
            Widget::select(WidgetId(2), "Theme", vec!["a".into(), "b".into()], 0),
            Widget::stepper(WidgetId(3), "Font size", 12.0, 6.0..=32.0, 1.0, 1),
            Widget::button(WidgetId(4), "Apply"),
        ];
        let mut ui = Ui::new();
        ui.layout(&widgets, Rect::new(0, 0, 400, 400), 20);
        ui
    }

    #[test]
    fn the_dim_layer_covers_the_whole_window() {
        // A partial dim would leave terminal content bright beside the panel.
        let mut out = Vec::new();
        let window = Rect::new(0, 0, 800, 600);
        draw_panel_frame(
            &mut out,
            window,
            Rect::new(100, 100, 400, 300),
            &Theme::builtin_default(),
            colors(),
        );

        let dim = &out[0];
        assert_eq!(dim.position, [0.0, 0.0]);
        assert_eq!(dim.size, [800.0, 600.0]);
        assert!(dim.color[3] < 1.0, "the dim layer must be translucent");
        assert!(dim.color[3] > 0.4, "but opaque enough to be readable over");
    }

    #[test]
    fn the_dim_stays_visible_on_a_transparent_window() {
        // Routing the dim through `convert` would multiply in window opacity and make
        // it disappear exactly when it is most needed.
        let mut out = Vec::new();
        let transparent = ColorSpace {
            srgb: false,
            opacity: 0.1,
        };
        draw_panel_frame(
            &mut out,
            Rect::new(0, 0, 800, 600),
            Rect::new(100, 100, 400, 300),
            &Theme::builtin_default(),
            transparent,
        );
        assert!(out[0].color[3] > 0.4, "got alpha {}", out[0].color[3]);
    }

    #[test]
    fn the_panel_is_drawn_as_a_border_plus_an_interior() {
        let mut out = Vec::new();
        let panel = Rect::new(100, 100, 400, 300);
        draw_panel_frame(
            &mut out,
            Rect::new(0, 0, 800, 600),
            panel,
            &Theme::builtin_default(),
            colors(),
        );

        // dim, border, interior
        assert_eq!(out.len(), 3);
        assert_eq!(out[1].size, [400.0, 300.0]);
        assert!(
            out[2].size[0] < out[1].size[0] && out[2].size[1] < out[1].size[1],
            "the interior must be inset inside the border"
        );
    }

    #[test]
    fn the_title_bar_returns_the_area_left_for_widgets() {
        let mut fonts = fonts();
        let mut out = Vec::new();
        let panel = Rect::new(0, 0, 400, 300);

        let body = draw_panel_title(
            &mut out,
            &mut fonts,
            panel,
            "Settings",
            &Theme::builtin_default(),
            colors(),
            20.0,
        );

        assert!(body.y > panel.y, "the body must start below the title");
        assert_eq!(body.bottom(), panel.bottom());
        assert_eq!(body.height, panel.height - (body.y - panel.y) as u32);
    }

    #[test]
    fn a_panel_shorter_than_its_title_does_not_underflow() {
        let mut fonts = fonts();
        let mut out = Vec::new();
        let body = draw_panel_title(
            &mut out,
            &mut fonts,
            Rect::new(0, 0, 400, 4),
            "Settings",
            &Theme::builtin_default(),
            colors(),
            20.0,
        );
        assert_eq!(body.height, 0);
    }

    #[test]
    fn every_widget_produces_something_to_draw() {
        let mut fonts = fonts();
        let mut out = Vec::new();
        draw_widgets(
            &mut out,
            &mut fonts,
            &sample_ui(),
            &Theme::builtin_default(),
            colors(),
        );

        let glyphs = out
            .iter()
            .filter(|i| i.flags & crate::FLAG_TEXTURED != 0)
            .count();
        // Five rows of labels plus three value readouts plus a button label.
        assert!(glyphs > 30, "expected widget text, got {glyphs} glyphs");
    }

    #[test]
    fn backgrounds_are_all_appended_before_any_text() {
        // Same painter's-order rule as the cell renderer: a row's highlight must not
        // paint over a neighbour's label.
        let mut fonts = fonts();
        let mut ui = sample_ui();
        ui.focus(WidgetId(1));

        let mut out = Vec::new();
        draw_widgets(
            &mut out,
            &mut fonts,
            &ui,
            &Theme::builtin_default(),
            colors(),
        );

        let first_text = out
            .iter()
            .position(|i| i.flags & crate::FLAG_TEXTURED != 0)
            .expect("there should be text");
        let last_solid = out
            .iter()
            .rposition(|i| i.flags == 0)
            .expect("there should be solids");
        assert!(
            last_solid < first_text,
            "a background at {last_solid} came after text at {first_text}"
        );
    }

    #[test]
    fn the_focused_widget_gets_a_ring_in_the_cursor_color() {
        let theme = Theme::builtin_default();
        let mut fonts = fonts();
        let mut ui = sample_ui();
        ui.focus(WidgetId(3));

        let mut out = Vec::new();
        draw_widgets(&mut out, &mut fonts, &ui, &theme, colors());

        let ring = colors().convert_opaque(theme.cursor());
        let edges = out
            .iter()
            .filter(|i| i.flags == 0 && i.color == ring)
            .filter(|i| i.size[0] == FOCUS_RING || i.size[1] == FOCUS_RING)
            .count();
        assert_eq!(edges, 4, "a ring is four edges");
    }

    #[test]
    fn nothing_is_ringed_when_nothing_has_focus() {
        let theme = Theme::builtin_default();
        let mut fonts = fonts();
        let mut out = Vec::new();
        draw_widgets(&mut out, &mut fonts, &sample_ui(), &theme, colors());

        let ring = colors().convert_opaque(theme.cursor());
        let edges = out
            .iter()
            .filter(|i| i.flags == 0 && i.color == ring)
            .filter(|i| i.size[0] == FOCUS_RING || i.size[1] == FOCUS_RING)
            .count();
        assert_eq!(edges, 0);
    }

    #[test]
    fn the_hovered_row_is_highlighted() {
        let theme = Theme::builtin_default();
        let mut fonts = fonts();
        let mut ui = sample_ui();
        let rect = ui.rect_of(WidgetId(2)).unwrap();
        ui.set_pointer(rect.center_x(), rect.center_y());

        let mut out = Vec::new();
        draw_widgets(&mut out, &mut fonts, &ui, &theme, colors());

        let highlight = colors().convert_opaque(theme.background_focused());
        assert!(
            out.iter()
                .any(|i| i.flags == 0 && i.color == highlight && i.size[0] == rect.width as f32),
            "the hovered row should be filled"
        );
    }

    #[test]
    fn a_buttons_box_is_exactly_the_rect_it_is_clicked_by() {
        // Full-width buttons would look like bars rather than controls.
        let mut fonts = fonts();
        let ui = sample_ui();
        let placed = ui
            .placed()
            .iter()
            .find(|p| matches!(p.widget, Widget::Button { .. }))
            .unwrap();

        let mut out = Vec::new();
        draw_widgets(
            &mut out,
            &mut fonts,
            &ui,
            &Theme::builtin_default(),
            colors(),
        );

        // `Ui::click` hit-tests against `rect`, so the box has to be drawn at `rect`
        // too. When these were different rectangles the visible button and the
        // pressable area did not line up, and part of what looked like a button
        // simply did not respond.
        assert!(
            out.iter()
                .any(|i| i.flags == 0 && i.size[0] == placed.rect.width as f32),
            "the button box must be drawn at the rect that hit-testing uses"
        );
        assert!(
            !out.iter().any(|i| i.flags == 0
                && i.size[0] == placed.value_rect.width as f32
                && placed.value_rect.width != placed.rect.width),
            "and never at the narrower value column"
        );
    }

    #[test]
    fn a_disabled_button_still_shows_its_label() {
        let theme = Theme::builtin_default();
        let mut fonts = fonts();
        let mut ui = Ui::new();
        ui.layout(
            &[Widget::disabled_button(WidgetId(1), "Save")],
            Rect::new(0, 0, 400, 100),
            20,
        );

        let mut out = Vec::new();
        draw_widgets(&mut out, &mut fonts, &ui, &theme, colors());

        let dimmed = colors().convert_opaque(theme.bright.black);
        assert!(
            out.iter()
                .any(|i| i.flags & crate::FLAG_TEXTURED != 0 && i.color == dimmed),
            "a disabled button should be dimmed, not blank"
        );
    }

    #[test]
    fn values_are_drawn_in_the_value_column() {
        let mut fonts = fonts();
        let ui = sample_ui();
        let stepper = ui
            .placed()
            .iter()
            .find(|p| matches!(p.widget, Widget::Stepper { .. }))
            .unwrap();

        let mut out = Vec::new();
        draw_widgets(
            &mut out,
            &mut fonts,
            &ui,
            &Theme::builtin_default(),
            colors(),
        );

        // Some glyph must fall inside the value column of that row.
        assert!(
            out.iter().any(|i| {
                i.flags & crate::FLAG_TEXTURED != 0
                    && i.position[0] >= stepper.value_rect.x as f32
                    && i.position[1] >= stepper.value_rect.y as f32
                    && i.position[1] < stepper.value_rect.bottom() as f32
            }),
            "the stepper value should render in its value column"
        );
    }

    #[test]
    fn widgets_stay_inside_the_area_they_were_laid_out_in() {
        let area = Rect::new(0, 0, 400, 400);
        let mut fonts = fonts();
        let mut out = Vec::new();
        draw_widgets(
            &mut out,
            &mut fonts,
            &sample_ui(),
            &Theme::builtin_default(),
            colors(),
        );

        for inst in &out {
            assert!(
                inst.position[0] >= area.x as f32 - 1.0
                    && inst.position[0] + inst.size[0] <= area.right() as f32 + 1.0,
                "instance at {:?} size {:?} escapes the panel horizontally",
                inst.position,
                inst.size
            );
        }
    }

    #[test]
    fn a_value_change_is_reflected_the_next_time_it_is_drawn() {
        // The immediate-mode payoff: no retained state to go stale.
        let theme = Theme::builtin_default();
        let mut fonts = fonts();
        let mut ui = Ui::new();

        let count_glyphs = |ui: &Ui, fonts: &mut FontSystem| {
            let mut out = Vec::new();
            draw_widgets(&mut out, fonts, ui, &theme, colors());
            out.iter()
                .filter(|i| i.flags & crate::FLAG_TEXTURED != 0)
                .count()
        };

        ui.layout(
            &[Widget::stepper(
                WidgetId(1),
                "Size",
                9.0,
                0.0..=100.0,
                1.0,
                0,
            )],
            Rect::new(0, 0, 400, 100),
            20,
        );
        let one_digit = count_glyphs(&ui, &mut fonts);

        // Rebuild with a wider value; the drawn text must follow.
        ui.layout(
            &[Widget::stepper(
                WidgetId(1),
                "Size",
                100.0,
                0.0..=100.0,
                1.0,
                0,
            )],
            Rect::new(0, 0, 400, 100),
            20,
        );
        let three_digits = count_glyphs(&ui, &mut fonts);

        assert!(
            three_digits > one_digit,
            "a wider value should draw more glyphs ({three_digits} vs {one_digit})"
        );
    }

    #[test]
    fn keyboard_focus_and_drawing_agree() {
        // Tabbing then drawing must ring the widget the UI reports as focused.
        let theme = Theme::builtin_default();
        let mut fonts = fonts();
        let mut ui = sample_ui();
        ui.key(UiKey::Tab);
        let focused = ui.focused().expect("tab should focus something");
        let rect = ui.rect_of(focused).unwrap();

        let mut out = Vec::new();
        draw_widgets(&mut out, &mut fonts, &ui, &theme, colors());

        let ring = colors().convert_opaque(theme.cursor());
        assert!(
            out.iter().any(|i| {
                i.flags == 0 && i.color == ring && (i.position[1] - rect.y as f32).abs() < 1.0
            }),
            "the ring should be on the focused row"
        );
    }

    // --- centering --------------------------------------------------------

    #[test]
    fn a_panel_is_centered_in_the_window() {
        let panel = center_panel(Rect::new(0, 0, 800, 600), 400, 300);
        assert_eq!(panel, Rect::new(200, 150, 400, 300));
    }

    #[test]
    fn a_panel_larger_than_the_window_is_clamped_to_it() {
        // Otherwise the panel hangs off both edges and its buttons are unreachable.
        let panel = center_panel(Rect::new(0, 0, 300, 200), 900, 700);
        assert_eq!(panel, Rect::new(0, 0, 300, 200));
    }

    #[test]
    fn centering_respects_a_window_origin() {
        let panel = center_panel(Rect::new(50, 20, 400, 200), 200, 100);
        assert_eq!(panel, Rect::new(150, 70, 200, 100));
    }
    #[test]
    fn a_focused_text_field_has_one_border_not_a_ring_around_a_box() {
        let theme = Theme::builtin_default();
        let mut fonts = fonts();
        let mut ui = Ui::new();
        let area = Rect::new(0, 0, 400, 200);
        ui.layout(
            &[Widget::text(WidgetId(1), "Rename to", "README.md", "")],
            area,
            20,
        );
        ui.focus(WidgetId(1));

        let mut out = Vec::new();
        draw_widgets(&mut out, &mut fonts, &ui, &theme, colors());

        let row = ui.placed()[0].rect;
        let ring = colors().convert_opaque(theme.cursor());
        // The generic focus ring spans the row's full width. A text field draws its
        // own box instead, so nothing that wide should be painted in the ring color —
        // otherwise the field ends up inside a second border.
        assert!(
            !out.iter()
                .any(|i| i.color == ring && i.size[0] == row.width as f32),
            "a focused field should not also get a ring around its whole row"
        );
        // The box itself is still there, and carries the focus color.
        assert!(
            out.iter().any(|i| i.color == ring),
            "the field's own border should show focus"
        );
    }

    #[test]
    fn an_unfocused_text_field_keeps_a_neutral_border() {
        let theme = Theme::builtin_default();
        let mut fonts = fonts();
        let mut ui = Ui::new();
        ui.layout(
            &[Widget::text(WidgetId(1), "Rename to", "README.md", "")],
            Rect::new(0, 0, 400, 200),
            20,
        );

        let mut out = Vec::new();
        draw_widgets(&mut out, &mut fonts, &ui, &theme, colors());

        let ring = colors().convert_opaque(theme.cursor());
        assert!(
            !out.iter().any(|i| i.color == ring),
            "an unfocused field must not look focused"
        );
    }

    #[test]
    fn the_panel_title_lines_up_with_the_rows_beneath_it() {
        let theme = Theme::builtin_default();
        let mut fonts = fonts();
        let panel = Rect::new(0, 0, 400, 300);

        let mut out = Vec::new();
        let body = draw_panel_title(
            &mut out,
            &mut fonts,
            panel,
            "Settings",
            &theme,
            colors(),
            // Derived, not the literal cell height. Those were the same number until the
            // panel metrics were snapped to a grid, and the point of this test is that
            // the title tracks `Metrics::padding` wherever that lands.
            tuz_ui::Metrics::from_cell_height(20).padding as f32,
        );
        let title_x = out
            .iter()
            .filter(|i| i.flags & crate::FLAG_TEXTURED != 0)
            .map(|i| i.position[0])
            .fold(f32::MAX, f32::min);

        let mut ui = Ui::new();
        ui.layout(&[Widget::toggle(WidgetId(1), "Theme", false)], body, 20);
        let row_x = ui.placed()[0].rect.x as f32;

        // A heading indented differently from what it heads reads as a misalignment.
        // Within a pixel: the title is a glyph origin, the row a rect edge.
        assert!(
            (title_x - row_x).abs() <= 1.0,
            "title starts at {title_x}, rows at {row_x}"
        );
    }

    #[test]
    fn a_pressed_button_inverts_so_a_click_is_visible() {
        let theme = Theme::builtin_default();
        let mut fonts = fonts();
        let mut ui = Ui::new();
        ui.layout(
            &[Widget::button(WidgetId(1), "Import")],
            Rect::new(0, 0, 400, 100),
            20,
        );

        let draw = |ui: &Ui, fonts: &mut FontSystem| {
            let mut out = Vec::new();
            draw_widgets(&mut out, fonts, ui, &theme, colors());
            out
        };

        let idle = draw(&ui, &mut fonts);
        ui.set_pressed(Some(WidgetId(1)));
        let held = draw(&ui, &mut fonts);

        let accent = colors().convert_opaque(theme.cursor());
        // Hover cannot show a click — the pointer is already over the button — so the
        // pressed look has to differ from every other state.
        assert!(
            !idle.iter().any(|i| i.color == accent),
            "an idle button should not look pressed"
        );
        assert!(
            held.iter().any(|i| i.color == accent),
            "a held button should invert"
        );
    }

    #[test]
    fn hover_and_press_are_different_looks_for_a_widget_button() {
        let theme = Theme::builtin_default();
        let mut fonts = fonts();
        let mut ui = Ui::new();
        let area = Rect::new(0, 0, 400, 100);
        ui.layout(&[Widget::button(WidgetId(1), "Import")], area, 20);

        let rect = ui.placed()[0].rect;
        ui.set_pointer(rect.x + 2, rect.y + 2);
        let mut hovered = Vec::new();
        draw_widgets(&mut hovered, &mut fonts, &ui, &theme, colors());

        ui.set_pressed(Some(WidgetId(1)));
        let mut held = Vec::new();
        draw_widgets(&mut held, &mut fonts, &ui, &theme, colors());

        let fill = |out: &[Instance]| {
            out.iter()
                .filter(|i| i.flags & crate::FLAG_TEXTURED == 0)
                .map(|i| i.color)
                .collect::<Vec<_>>()
        };
        assert_ne!(fill(&hovered), fill(&held));
    }
}
