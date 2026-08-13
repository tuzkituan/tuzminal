//! Quad instances and the code that builds them from a terminal snapshot.
//!
//! Everything the terminal draws is a rectangle: cell backgrounds, glyph bitmaps,
//! underlines, strikethroughs, the cursor, split dividers. Representing them all
//! as one instance type means one buffer and one draw call per frame.
//!
//! **Order matters.** Instances are drawn in buffer order with no depth test, so
//! this module appends in painter's order: backgrounds, then glyph decorations,
//! then glyphs, then the cursor.

use tuz_config::Rgba;
use tuz_core::{RenderCell, TerminalFrame, Underline};
use tuz_font::{CellMetrics, FontSystem, Style};

/// The instance is textured and should sample the glyph atlas.
pub const FLAG_TEXTURED: u32 = 1;
/// The glyph carries its own color (emoji) and must not be tinted.
pub const FLAG_COLOR_GLYPH: u32 = 2;

/// One quad. Field order and padding must match `cell.wgsl`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Instance {
    /// Top-left corner in physical pixels.
    pub position: [f32; 2],
    pub size: [f32; 2],
    /// `[u0, v0, u1, v1]`, unused for solid rects.
    pub uv: [f32; 4],
    /// Linear-space RGBA.
    pub color: [f32; 4],
    pub flags: u32,
    /// Explicit padding to a 16-byte boundary, required by the vertex layout.
    pub _padding: [u32; 3],
}

impl Instance {
    /// A solid rectangle.
    pub fn solid(x: f32, y: f32, width: f32, height: f32, color: [f32; 4]) -> Self {
        Self {
            position: [x, y],
            size: [width, height],
            uv: [0.0; 4],
            color,
            flags: 0,
            _padding: [0; 3],
        }
    }

    /// A textured rectangle sampling the glyph atlas.
    pub fn textured(
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        uv: [f32; 4],
        color: [f32; 4],
        color_glyph: bool,
    ) -> Self {
        Self {
            position: [x, y],
            size: [width, height],
            uv,
            color,
            flags: FLAG_TEXTURED | if color_glyph { FLAG_COLOR_GLYPH } else { 0 },
            _padding: [0; 3],
        }
    }

    /// The vertex buffer layout matching this struct.
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
            0 => Float32x2, // position
            1 => Float32x2, // size
            2 => Float32x4, // uv
            3 => Float32x4, // color
            4 => Uint32,    // flags
        ];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Instance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRS,
        }
    }
}

/// How to convert theme colors for the current surface.
#[derive(Debug, Clone, Copy)]
pub struct ColorSpace {
    /// True when the surface applies the sRGB encode, so colors must be
    /// linearized first. Getting this backwards washes out every color on screen.
    pub srgb: bool,
    /// Multiplied into every alpha, for window transparency.
    pub opacity: f32,
}

impl ColorSpace {
    pub fn convert(&self, color: Rgba) -> [f32; 4] {
        let mut c = if self.srgb {
            color.to_linear()
        } else {
            color.to_unorm()
        };
        c[3] *= self.opacity.clamp(0.0, 1.0);
        c
    }

    /// Like [`convert`](Self::convert) but forcing full opacity.
    ///
    /// Text must stay readable in a transparent window: fading glyphs along with
    /// the background is what makes translucent terminals unusable.
    pub fn convert_opaque(&self, color: Rgba) -> [f32; 4] {
        if self.srgb {
            color.to_linear()
        } else {
            color.to_unorm()
        }
    }
}

/// Geometry for one pane's content area.
#[derive(Debug, Clone, Copy)]
pub struct PaneGeometry {
    /// Top-left of the cell grid, in physical pixels.
    pub origin: (f32, f32),
    pub cell_width: f32,
    pub cell_height: f32,
}

impl PaneGeometry {
    fn cell_x(&self, col: u16) -> f32 {
        self.origin.0 + col as f32 * self.cell_width
    }
    fn cell_y(&self, row: u16) -> f32 {
        self.origin.1 + row as f32 * self.cell_height
    }
}

/// Append the instances for one pane.
///
/// Returns how many instances were added, which the caller uses for diagnostics
/// and to decide whether the frame is worth submitting.
pub fn build_pane(
    out: &mut Vec<Instance>,
    frame: &TerminalFrame,
    fonts: &mut FontSystem,
    geom: PaneGeometry,
    colors: ColorSpace,
) -> usize {
    let start = out.len();
    let metrics = fonts.metrics();

    // Pass 1: backgrounds. All of them before any glyph, so a cell's background
    // can never paint over its neighbour's glyph.
    for cell in &frame.cells {
        if cell.bg == frame.background {
            // The surface was already cleared to the default background.
            continue;
        }
        let width = if cell.flags.wide {
            geom.cell_width * 2.0
        } else {
            geom.cell_width
        };
        out.push(Instance::solid(
            geom.cell_x(cell.col),
            geom.cell_y(cell.row),
            width,
            geom.cell_height,
            colors.convert(cell.bg),
        ));
    }

    // Pass 2: decorations, under the glyphs so a descender crosses an underline
    // rather than being hidden by it.
    for cell in &frame.cells {
        push_decorations(out, cell, geom, metrics, colors);
    }

    // Pass 3: glyphs.
    for cell in &frame.cells {
        push_glyphs(out, cell, fonts, geom, metrics, colors);
    }

    // Pass 4: the cursor, last so it sits on top of its cell's glyph.
    if let Some(cursor) = frame.cursor {
        push_cursor(out, &cursor, frame, fonts, geom, metrics, colors);
    }

    out.len() - start
}

fn push_decorations(
    out: &mut Vec<Instance>,
    cell: &RenderCell,
    geom: PaneGeometry,
    metrics: CellMetrics,
    colors: ColorSpace,
) {
    let width = if cell.flags.wide {
        geom.cell_width * 2.0
    } else {
        geom.cell_width
    };
    let x = geom.cell_x(cell.col);
    let baseline = geom.cell_y(cell.row) + metrics.baseline();

    // SGR 58 can color an underline independently of the text.
    let line_color = colors.convert_opaque(cell.underline_color.unwrap_or(cell.fg));

    let thickness = metrics.underline_thickness.max(1.0);
    match cell.flags.underline {
        Underline::None => {}
        Underline::Single => {
            out.push(Instance::solid(
                x,
                baseline + metrics.underline_offset,
                width,
                thickness,
                line_color,
            ));
        }
        Underline::Double => {
            let gap = thickness * 2.0;
            out.push(Instance::solid(
                x,
                baseline + metrics.underline_offset,
                width,
                thickness,
                line_color,
            ));
            out.push(Instance::solid(
                x,
                baseline + metrics.underline_offset + gap,
                width,
                thickness,
                line_color,
            ));
        }
        Underline::Dotted | Underline::Dashed => {
            // Approximated with segments rather than a texture: at terminal sizes
            // the difference is invisible and it needs no extra atlas space.
            let (dash, gap) = if cell.flags.underline == Underline::Dotted {
                (thickness, thickness)
            } else {
                (thickness * 3.0, thickness * 2.0)
            };
            let mut dx = 0.0;
            while dx < width {
                let seg = dash.min(width - dx);
                out.push(Instance::solid(
                    x + dx,
                    baseline + metrics.underline_offset,
                    seg,
                    thickness,
                    line_color,
                ));
                dx += dash + gap;
            }
        }
        Underline::Curly => {
            // A sawtooth approximation of an undercurl: alternating short
            // segments above and below the underline position.
            let step = (thickness * 2.0).max(2.0);
            let amplitude = thickness;
            let mut dx = 0.0;
            let mut up = true;
            while dx < width {
                let seg = step.min(width - dx);
                let dy = if up { -amplitude } else { amplitude };
                out.push(Instance::solid(
                    x + dx,
                    baseline + metrics.underline_offset + dy,
                    seg,
                    thickness,
                    line_color,
                ));
                dx += step;
                up = !up;
            }
        }
    }

    if cell.flags.strikeout {
        out.push(Instance::solid(
            x,
            baseline - metrics.strikeout_offset,
            width,
            metrics.strikeout_thickness.max(1.0),
            colors.convert_opaque(cell.fg),
        ));
    }
}

fn push_glyphs(
    out: &mut Vec<Instance>,
    cell: &RenderCell,
    fonts: &mut FontSystem,
    geom: PaneGeometry,
    metrics: CellMetrics,
    colors: ColorSpace,
) {
    if cell.is_blank() {
        return;
    }

    let style = Style::new(cell.flags.bold, cell.flags.italic);
    let x = geom.cell_x(cell.col);
    let baseline = geom.cell_y(cell.row) + metrics.baseline();
    let fg = colors.convert_opaque(cell.fg);

    // The base glyph, then any combining marks stacked in the same cell.
    let mut chars = Vec::with_capacity(1 + cell.zerowidth.len());
    chars.push(cell.ch);
    chars.extend_from_slice(&cell.zerowidth);

    for c in chars {
        let Some((font, glyph_id)) = fonts.font_for_char(c, style) else {
            continue;
        };
        let Some(glyph) = fonts.rasterize(font, glyph_id, style) else {
            continue;
        };
        if glyph.is_blank() {
            continue;
        }

        out.push(Instance::textured(
            x + glyph.left,
            baseline - glyph.top,
            glyph.rect.width as f32,
            glyph.rect.height as f32,
            glyph.uv,
            fg,
            glyph.color,
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn push_cursor(
    out: &mut Vec<Instance>,
    cursor: &tuz_core::RenderCursor,
    frame: &TerminalFrame,
    fonts: &mut FontSystem,
    geom: PaneGeometry,
    metrics: CellMetrics,
    colors: ColorSpace,
) {
    use tuz_config::CursorShape;

    let x = geom.cell_x(cursor.col);
    let y = geom.cell_y(cursor.row);
    let w = geom.cell_width;
    let h = geom.cell_height;
    // The cursor stays opaque even in a transparent window; a see-through cursor
    // is very hard to find.
    let color = colors.convert_opaque(cursor.color);

    match cursor.shape {
        CursorShape::Block => {
            out.push(Instance::solid(x, y, w, h, color));
            // Redraw the covered glyph in the cursor's text color, or the
            // character under a block cursor becomes invisible.
            if let Some(cell) = frame
                .cells
                .iter()
                .find(|c| c.col == cursor.col && c.row == cursor.row)
            {
                if !cell.is_blank() {
                    let style = Style::new(cell.flags.bold, cell.flags.italic);
                    if let Some((font, glyph_id)) = fonts.font_for_char(cell.ch, style) {
                        if let Some(glyph) = fonts.rasterize(font, glyph_id, style) {
                            if !glyph.is_blank() {
                                out.push(Instance::textured(
                                    x + glyph.left,
                                    y + metrics.baseline() - glyph.top,
                                    glyph.rect.width as f32,
                                    glyph.rect.height as f32,
                                    glyph.uv,
                                    colors.convert_opaque(cursor.text_color),
                                    glyph.color,
                                ));
                            }
                        }
                    }
                }
            }
        }
        CursorShape::Beam => {
            let thickness = (w * cursor.thickness).max(1.0);
            out.push(Instance::solid(x, y, thickness, h, color));
        }
        CursorShape::Underline => {
            let thickness = (h * cursor.thickness).max(1.0);
            out.push(Instance::solid(x, y + h - thickness, w, thickness, color));
        }
        CursorShape::HollowBlock => {
            // Four thin rects rather than a shader outline: no extra pipeline and
            // it scales correctly with the cell size.
            let t = 1.0_f32.max(w * 0.06);
            out.push(Instance::solid(x, y, w, t, color));
            out.push(Instance::solid(x, y + h - t, w, t, color));
            out.push(Instance::solid(x, y, t, h, color));
            out.push(Instance::solid(x + w - t, y, t, h, color));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuz_config::{Config, Theme};
    use tuz_core::{Session, TermSize};

    fn colors() -> ColorSpace {
        ColorSpace {
            srgb: false,
            opacity: 1.0,
        }
    }

    fn geometry() -> PaneGeometry {
        PaneGeometry {
            origin: (0.0, 0.0),
            cell_width: 8.0,
            cell_height: 16.0,
        }
    }

    #[test]
    fn instance_is_16_byte_aligned_for_the_vertex_layout() {
        // A mismatch here silently corrupts every attribute after the offset.
        assert_eq!(std::mem::size_of::<Instance>() % 16, 0);
        assert_eq!(std::mem::size_of::<Instance>(), 64);
    }

    #[test]
    fn solid_and_textured_set_the_right_flags() {
        let s = Instance::solid(0.0, 0.0, 1.0, 1.0, [1.0; 4]);
        assert_eq!(s.flags, 0);

        let t = Instance::textured(0.0, 0.0, 1.0, 1.0, [0.0; 4], [1.0; 4], false);
        assert_eq!(t.flags, FLAG_TEXTURED);

        let e = Instance::textured(0.0, 0.0, 1.0, 1.0, [0.0; 4], [1.0; 4], true);
        assert_eq!(e.flags, FLAG_TEXTURED | FLAG_COLOR_GLYPH);
    }

    #[test]
    fn opacity_fades_backgrounds_but_never_text() {
        // The reason `convert_opaque` exists: fading glyphs along with the
        // background makes a translucent terminal unreadable.
        let c = ColorSpace {
            srgb: false,
            opacity: 0.5,
        };
        assert_eq!(c.convert(Rgba::WHITE)[3], 0.5);
        assert_eq!(c.convert_opaque(Rgba::WHITE)[3], 1.0);
    }

    #[test]
    fn srgb_conversion_is_selected_by_the_flag() {
        let linear = ColorSpace {
            srgb: true,
            opacity: 1.0,
        };
        let unorm = ColorSpace {
            srgb: false,
            opacity: 1.0,
        };
        let mid = Rgba::rgb(128, 128, 128);
        assert!(linear.convert(mid)[0] < unorm.convert(mid)[0]);
    }

    #[test]
    fn a_default_background_cell_emits_no_background_quad() {
        // The clear color already covers it; emitting a quad per cell would put
        // thousands of redundant instances in every frame.
        let theme = Theme::builtin_default();
        let frame = TerminalFrame {
            cells: vec![RenderCell {
                col: 0,
                row: 0,
                ch: ' ',
                zerowidth: vec![],
                fg: theme.foreground,
                bg: theme.background,
                underline_color: None,
                flags: Default::default(),
            }],
            cursor: None,
            columns: 10,
            rows: 2,
            display_offset: 0,
            background: theme.background,
        };

        let mut out = Vec::new();
        let mut fonts = test_fonts();
        build_pane(&mut out, &frame, &mut fonts, geometry(), colors());
        assert!(out.is_empty(), "got {} instances", out.len());
    }

    /// Panics rather than skipping: an earlier version returned `Result` and every
    /// test began `let Ok(fonts) = test_fonts() else { return }`, so this whole
    /// module silently reported success without running while font resolution was
    /// broken — which is how the assertion below stayed wrong.
    fn test_fonts() -> FontSystem {
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

    fn frame_from(bytes: &[u8], cols: u16, rows: u16) -> TerminalFrame {
        let session = Session::detached(tuz_layout::PaneId(1), TermSize::new(cols, rows, 8, 16));
        session.feed_for_test(bytes);
        let theme = Theme::builtin_default();
        let term = session.term().lock();
        tuz_core::snapshot(&term, &theme, &Config::default(), true, true)
    }

    #[test]
    fn text_produces_glyph_instances() {
        let mut fonts = test_fonts();
        let frame = frame_from(b"abc", 20, 3);

        let mut out = Vec::new();
        let n = build_pane(&mut out, &frame, &mut fonts, geometry(), colors());

        assert!(n >= 3, "expected at least one instance per glyph, got {n}");
        assert!(
            out.iter().filter(|i| i.flags & FLAG_TEXTURED != 0).count() >= 3,
            "glyphs should be textured"
        );
    }

    #[test]
    fn backgrounds_are_appended_before_glyphs() {
        // Painter's order: a later cell's background must never cover an earlier
        // cell's glyph, so all solids come first.
        let mut fonts = test_fonts();
        // Red background across three cells with text on top.
        let frame = frame_from(b"\x1b[41mabc", 20, 3);

        let mut out = Vec::new();
        build_pane(&mut out, &frame, &mut fonts, geometry(), colors());

        let expected_backgrounds = frame
            .cells
            .iter()
            .filter(|c| c.bg != frame.background)
            .count();
        assert!(expected_backgrounds >= 3, "the setup should color 3 cells");

        // The leading run of instances must be exactly the cell backgrounds.
        for (i, inst) in out.iter().take(expected_backgrounds).enumerate() {
            assert_eq!(inst.flags, 0, "instance {i} should be a solid background");
        }

        let first_textured = out
            .iter()
            .position(|i| i.flags & FLAG_TEXTURED != 0)
            .expect("glyphs should be emitted");
        assert!(
            first_textured >= expected_backgrounds,
            "a glyph at {first_textured} precedes the {expected_backgrounds} backgrounds"
        );
        // Deliberately not phrased as "the last solid precedes the first glyph":
        // the cursor is also a full-cell solid and is appended *after* the glyphs
        // by design, so that weaker phrasing fails for the wrong reason.
    }

    #[test]
    fn cells_land_at_their_grid_position() {
        let mut fonts = test_fonts();
        let frame = frame_from(b"\x1b[41m \x1b[0m\r\n\x1b[41m ", 20, 3);

        let mut out = Vec::new();
        build_pane(&mut out, &frame, &mut fonts, geometry(), colors());

        let bgs: Vec<_> = out.iter().filter(|i| i.flags == 0).collect();
        assert!(bgs.iter().any(|i| i.position == [0.0, 0.0]));
        // Second row starts one cell height down.
        assert!(bgs.iter().any(|i| i.position == [0.0, 16.0]));
    }

    #[test]
    fn a_wide_glyph_gets_a_double_width_background() {
        let mut fonts = test_fonts();
        // Red background behind a CJK character.
        let frame = frame_from("\x1b[41m日".as_bytes(), 20, 3);

        let mut out = Vec::new();
        build_pane(&mut out, &frame, &mut fonts, geometry(), colors());

        assert!(
            out.iter()
                .any(|i| i.flags == 0 && i.size[0] == geometry().cell_width * 2.0),
            "a wide cell's background must span two columns"
        );
    }

    #[test]
    fn an_underline_adds_a_thin_solid_rect() {
        let mut fonts = test_fonts();
        let frame = frame_from(b"\x1b[4mx", 20, 3);

        let mut out = Vec::new();
        build_pane(&mut out, &frame, &mut fonts, geometry(), colors());

        assert!(
            out.iter()
                .any(|i| i.flags == 0 && i.size[1] < geometry().cell_height / 2.0),
            "expected a thin underline rect"
        );
    }

    #[test]
    fn a_curly_underline_emits_multiple_segments() {
        let mut fonts = test_fonts();
        let plain = {
            let mut v = Vec::new();
            build_pane(
                &mut v,
                &frame_from(b"\x1b[4mx", 20, 3),
                &mut fonts,
                geometry(),
                colors(),
            );
            v.len()
        };
        let curly = {
            let mut v = Vec::new();
            build_pane(
                &mut v,
                &frame_from(b"\x1b[4:3mx", 20, 3),
                &mut fonts,
                geometry(),
                colors(),
            );
            v.len()
        };
        assert!(curly > plain, "undercurl should use several segments");
    }

    #[test]
    fn a_block_cursor_redraws_the_glyph_underneath() {
        // Without this the character under the cursor vanishes.
        let mut fonts = test_fonts();
        let mut frame = frame_from(b"x", 20, 3);
        // Put the cursor on the 'x' rather than after it.
        if let Some(c) = frame.cursor.as_mut() {
            c.col = 0;
            c.shape = tuz_config::CursorShape::Block;
        }

        let mut out = Vec::new();
        build_pane(&mut out, &frame, &mut fonts, geometry(), colors());

        // The last textured instance is the re-drawn glyph, in cursor_text color.
        let last_glyph = out
            .iter()
            .rev()
            .find(|i| i.flags & FLAG_TEXTURED != 0)
            .expect("a glyph should be drawn over the cursor");
        let expected = colors().convert_opaque(frame.cursor.unwrap().text_color);
        assert_eq!(last_glyph.color, expected);
    }

    #[test]
    fn a_hollow_block_cursor_draws_four_edges() {
        let mut fonts = test_fonts();
        let mut frame = frame_from(b"", 20, 3);
        if let Some(c) = frame.cursor.as_mut() {
            c.shape = tuz_config::CursorShape::HollowBlock;
        }

        let mut out = Vec::new();
        build_pane(&mut out, &frame, &mut fonts, geometry(), colors());
        assert_eq!(out.len(), 4, "an outline needs exactly four rects");
    }

    #[test]
    fn a_beam_cursor_is_narrow_and_full_height() {
        let mut fonts = test_fonts();
        let mut frame = frame_from(b"", 20, 3);
        if let Some(c) = frame.cursor.as_mut() {
            c.shape = tuz_config::CursorShape::Beam;
            c.thickness = 0.15;
        }

        let mut out = Vec::new();
        build_pane(&mut out, &frame, &mut fonts, geometry(), colors());

        assert_eq!(out.len(), 1);
        assert!(out[0].size[0] < geometry().cell_width);
        assert_eq!(out[0].size[1], geometry().cell_height);
    }

    #[test]
    fn no_cursor_means_no_cursor_instances() {
        let mut fonts = test_fonts();
        let mut frame = frame_from(b"", 20, 3);
        frame.cursor = None;

        let mut out = Vec::new();
        let n = build_pane(&mut out, &frame, &mut fonts, geometry(), colors());
        assert_eq!(n, 0);
    }

    #[test]
    fn the_pane_origin_offsets_everything() {
        let mut fonts = test_fonts();
        let frame = frame_from(b"\x1b[41m ", 20, 3);

        let mut out = Vec::new();
        build_pane(
            &mut out,
            &frame,
            &mut fonts,
            PaneGeometry {
                origin: (100.0, 50.0),
                cell_width: 8.0,
                cell_height: 16.0,
            },
            colors(),
        );

        assert!(
            out.iter()
                .all(|i| i.position[0] >= 100.0 && i.position[1] >= 50.0),
            "every instance must respect the pane origin"
        );
    }
}
