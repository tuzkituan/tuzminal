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
use tuz_config::Theme;
use tuz_font::{FontSystem, Style};
use tuz_layout::Rect;
use tuz_ui::{Placed, Ui, Widget, WidgetId};

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
        _padding: [0; 3],
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
pub fn draw_panel_title(
    out: &mut Vec<Instance>,
    fonts: &mut FontSystem,
    panel: Rect,
    title: &str,
    theme: &Theme,
    colors: ColorSpace,
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
        PADDING * 2.0,
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
    let focus = ui.focused();
    let hover = ui.hovered();

    for placed in ui.placed() {
        draw_row_background(out, placed, focus, hover, theme, colors);
    }
    for placed in ui.placed() {
        draw_row_text(out, fonts, placed, theme, colors);
    }
}

fn draw_row_background(
    out: &mut Vec<Instance>,
    placed: &Placed,
    focus: Option<WidgetId>,
    hover: Option<WidgetId>,
    theme: &Theme,
    colors: ColorSpace,
) {
    let id = placed.widget.id();
    let focused = id.is_some() && id == focus;
    let hovered = id.is_some() && id == hover;

    // A focus ring rather than a fill: a filled focus indicator competes with the
    // hover highlight and with a button's own border.
    if focused {
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
            colors.convert_opaque(if hovered {
                theme.background_focused()
            } else {
                theme.background
            }),
        ));
    }
}

/// Buttons occupy the value column rather than the whole row, so a row of them does
/// not look like a row of full-width bars.
fn button_rect(placed: &Placed) -> Rect {
    placed.value_rect
}

fn draw_row_text(
    out: &mut Vec<Instance>,
    fonts: &mut FontSystem,
    placed: &Placed,
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

        Widget::Button { label, enabled, .. } => {
            text::draw_in_box(
                out,
                fonts,
                label,
                button_rect(placed),
                PADDING,
                Align::Center,
                if *enabled {
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
    fn a_button_is_drawn_as_a_box_inside_the_value_column() {
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

        assert!(
            out.iter()
                .any(|i| i.flags == 0 && i.size[0] == placed.value_rect.width as f32),
            "the button box should match the value column width"
        );
        assert!(
            placed.value_rect.width < placed.rect.width,
            "and be narrower than the row"
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
}
