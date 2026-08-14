//! The window icon: pixel-art content on a normally rounded tile.
//!
//! The content is a 16×16 character grid scaled by whole pixels, because that is what
//! pixel art is — every pixel a deliberate decision, in a literal you can read.
//! Editing the picture means editing [`GRID`].
//!
//! The corners are *not* pixel art. An earlier version stepped them in a staircase,
//! which is the strictly correct way to round a corner on a pixel grid and looks like
//! a damaged square at icon sizes. The tile is a rounded rectangle with a real radius
//! and an antialiased edge; only the glyphs inside it are hard-edged.
//!
//! `assets/tuzminal.svg` is the same picture for desktop environments, which want
//! something scalable. A test below keeps the two from drifting apart.

/// The design's foreground. One character per pixel, 16 rows of 16.
///
/// | | |
/// |---|---|
/// | `.` | background shows through |
/// | `C` | the prompt chevron |
/// | `U` | the cursor block |
///
/// The background is not in this grid: it is a rounded rectangle covering the whole
/// icon, drawn with a real radius. Stepping the corners in a staircase — the strictly
/// pixel-art way to round them — reads as a damaged square at icon sizes rather than
/// as a rounded one, so the corner is smooth and only the content is pixels.
const GRID: &[&str] = &[
    "................",
    "................",
    "................",
    "................",
    "................",
    "...CC...........",
    "....CC....UUU...",
    ".....CC...UUU...",
    ".....CC...UUU...",
    "....CC....UUU...",
    "...CC.....UUU...",
    "................",
    "................",
    "................",
    "................",
    "................",
];

/// Side of the design, in design pixels.
pub const GRID_SIZE: u32 = 16;

/// How many real pixels per design pixel. 4 gives a 64×64 icon, which is what most
/// compositors ask for, and an integer factor is what keeps the edges hard.
const SCALE: u32 = 4;

/// Corner radius, as a fraction of the icon's side.
///
/// About a fifth, which is what desktop icons have settled on. Expressed as a
/// fraction so it holds at any [`SCALE`].
const RADIUS_FRACTION: f32 = 0.21;

/// Background, chevron and cursor, as straight RGB.
const BACKGROUND: [u8; 3] = [0x14, 0x17, 0x20];
const CHEVRON: [u8; 3] = [0x5c, 0xf0, 0xd4];
const CURSOR: [u8; 3] = [0xe6, 0xe9, 0xf0];

/// The colour of a foreground cell, or `None` where the background shows through.
fn color_of(cell: u8) -> Option<[u8; 3]> {
    match cell {
        b'C' => Some(CHEVRON),
        b'U' => Some(CURSOR),
        _ => None,
    }
}

/// Coverage of the rounded background at a point, antialiased over one pixel.
///
/// The one place smoothing is wanted: a curve quantised to whole pixels is a
/// staircase, and a staircase is what made the previous icon look broken.
fn background_coverage(x: f32, y: f32, size: f32) -> f32 {
    let radius = size * RADIUS_FRACTION;
    let half = size / 2.0;

    // Standard rounded-box signed distance.
    let px = (x - half).abs() - (half - radius);
    let py = (y - half).abs() - (half - radius);
    let outside = (px.max(0.0).powi(2) + py.max(0.0).powi(2)).sqrt();
    let distance = outside + px.max(py).min(0.0) - radius;

    (0.5 - distance).clamp(0.0, 1.0)
}

/// The icon as RGBA8 rows, top to bottom, alongside its side length.
pub fn rgba() -> (Vec<u8>, u32) {
    let size = GRID_SIZE * SCALE;
    let mut pixels = vec![0u8; (size * size * 4) as usize];

    // The background first, with a smooth corner.
    for y in 0..size {
        for x in 0..size {
            let coverage = background_coverage(x as f32 + 0.5, y as f32 + 0.5, size as f32);
            if coverage <= 0.0 {
                continue;
            }
            let o = ((y * size + x) * 4) as usize;
            pixels[o] = BACKGROUND[0];
            pixels[o + 1] = BACKGROUND[1];
            pixels[o + 2] = BACKGROUND[2];
            pixels[o + 3] = (coverage * 255.0).round() as u8;
        }
    }

    // Then the content, in hard-edged blocks over the top.
    for (row, line) in GRID.iter().enumerate() {
        for (col, cell) in line.bytes().enumerate() {
            let Some([r, g, b]) = color_of(cell) else {
                continue;
            };
            // One design pixel becomes a SCALE×SCALE block. No interpolation: the
            // hard edge is the point.
            for dy in 0..SCALE {
                for dx in 0..SCALE {
                    let x = col as u32 * SCALE + dx;
                    let y = row as u32 * SCALE + dy;
                    let o = ((y * size + x) * 4) as usize;
                    pixels[o] = r;
                    pixels[o + 1] = g;
                    pixels[o + 2] = b;
                    pixels[o + 3] = 0xff;
                }
            }
        }
    }

    (pixels, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_is_square_and_uses_only_known_cells() {
        assert_eq!(GRID.len(), GRID_SIZE as usize);
        for (row, line) in GRID.iter().enumerate() {
            assert_eq!(
                line.len(),
                GRID_SIZE as usize,
                "row {row} is {} wide, not {GRID_SIZE}",
                line.len()
            );
            for (col, cell) in line.bytes().enumerate() {
                assert!(
                    matches!(cell, b'.' | b'#' | b'C' | b'U'),
                    "row {row} col {col} is {:?}, which is not a known cell",
                    cell as char
                );
            }
        }
    }

    #[test]
    fn the_icon_is_a_square_rgba_buffer() {
        let (pixels, size) = rgba();
        assert_eq!(size, GRID_SIZE * SCALE);
        assert_eq!(
            pixels.len(),
            (size * size * 4) as usize,
            "winit reads the buffer as exactly size*size*4 and panics otherwise"
        );
    }

    #[test]
    fn the_content_is_hard_edged_even_though_the_tile_is_not() {
        // Where the two styles meet. Every content pixel must be fully opaque — a
        // partial alpha there would mean the glyphs got smoothed along with the
        // corners, which is the blur this design exists to avoid.
        let (pixels, size) = rgba();
        for (row, line) in GRID.iter().enumerate() {
            for (col, cell) in line.bytes().enumerate() {
                let Some([r, g, b]) = color_of(cell) else {
                    continue;
                };
                for dy in 0..SCALE {
                    for dx in 0..SCALE {
                        let x = col as u32 * SCALE + dx;
                        let y = row as u32 * SCALE + dy;
                        let o = ((y * size + x) * 4) as usize;
                        assert_eq!(
                            [pixels[o], pixels[o + 1], pixels[o + 2], pixels[o + 3]],
                            [r, g, b, 0xff],
                            "content pixel at ({x}, {y}) is not crisp"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_corners_are_rounded_smoothly_and_the_middle_is_filled() {
        let (pixels, size) = rgba();
        let alpha = |x: u32, y: u32| pixels[((y * size + x) * 4 + 3) as usize];

        assert_eq!(alpha(0, 0), 0, "the very corner is outside the radius");
        assert_eq!(alpha(size - 1, 0), 0);
        assert_eq!(alpha(0, size - 1), 0);
        assert_eq!(alpha(size - 1, size - 1), 0);
        assert_eq!(alpha(size / 2, size / 2), 0xff, "the middle is solid");
        // Mid-edge is flat, so it must be fully covered right to the boundary.
        assert_eq!(alpha(0, size / 2), 0xff, "the left edge should be straight");
        assert_eq!(alpha(size / 2, 0), 0xff, "the top edge should be straight");

        // The curve is antialiased, which is the whole difference from the staircase
        // this replaced. Scanned over the corner square rather than along a line: the
        // arc bulges toward the corner, so a straight chord between the two points
        // where it meets the edges misses it entirely.
        let radius = (size as f32 * RADIUS_FRACTION).ceil() as u32;
        let partial = (0..radius)
            .flat_map(|y| (0..radius).map(move |x| (x, y)))
            .filter(|(x, y)| {
                let a = alpha(*x, *y);
                a > 0 && a < 0xff
            })
            .count();
        assert!(
            partial > 0,
            "no partial alpha anywhere in the corner, so it is stepped rather than curved"
        );
    }

    #[test]
    fn the_prompt_and_the_cursor_are_both_drawn() {
        let (pixels, _) = rgba();
        let count = |want: [u8; 3]| {
            pixels
                .chunks_exact(4)
                .filter(|c| [c[0], c[1], c[2]] == want && c[3] == 0xff)
                .count()
        };

        // Twelve chevron cells and fifteen cursor cells in the grid, each a block of
        // SCALE². Without this the icon is a rounded square, which is a shape rather
        // than an icon for a terminal.
        let block = (SCALE * SCALE) as usize;
        assert_eq!(count(CHEVRON), 12 * block);
        assert_eq!(count(CURSOR), 15 * block);
    }

    /// The desktop icon and the window icon must be the same picture.
    ///
    /// They are separate files — one scalable for app grids, one rasterised for the
    /// window — so nothing but a test stops them drifting. This compares what each
    /// actually paints rather than trusting that both were edited.
    #[test]
    fn the_svg_paints_the_same_pixels_as_the_grid() {
        let svg = include_str!("../../../assets/tuzminal.svg");

        assert!(
            svg.contains(&format!("viewBox=\"0 0 {GRID_SIZE} {GRID_SIZE}\"")),
            "the svg should use the design's own coordinate system"
        );
        assert!(
            svg.contains("shape-rendering=\"crispEdges\""),
            "without this a renderer will antialias the pixel edges away"
        );

        // The tile: one rounded rect covering everything, with the same radius the
        // raster uses. `rx` in grid units, so it scales with the viewBox.
        let rx = format!("rx=\"{:.2}\"", GRID_SIZE as f32 * RADIUS_FRACTION);
        assert!(
            svg.contains(&rx),
            "the svg tile should carry {rx} to match the raster's corner"
        );

        // The content: one rect per horizontal run, whose areas must sum to the
        // grid's foreground cells. The tile rect is excluded by colour, since it is
        // the only one painted in the background.
        let content_area: u32 = svg
            .split("<rect")
            .skip(1)
            .filter(|rect| !rect.contains("#141720"))
            .map(|rect| {
                let field = |name: &str| -> u32 {
                    rect.split(&format!("{name}=\""))
                        .nth(1)
                        .and_then(|rest| rest.split('"').next())
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0)
                };
                field("width") * field("height")
            })
            .sum();

        let grid_area = GRID
            .iter()
            .flat_map(|line| line.bytes())
            .filter(|c| color_of(*c).is_some())
            .count() as u32;

        assert_eq!(
            content_area, grid_area,
            "the svg paints {content_area} content cells and the grid has {grid_area}"
        );
    }
}
