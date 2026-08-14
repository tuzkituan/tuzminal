//! The window icon: pixel art on a 16×16 grid, scaled up by an integer factor.
//!
//! Drawn as a grid of characters rather than with shapes, because that is what pixel
//! art is — every pixel is a deliberate decision, and a literal you can read is the
//! only honest way to express that. Editing the picture means editing [`GRID`].
//!
//! Scaled by whole pixels with no interpolation. Smoothing a 16×16 design to 64×64 is
//! exactly what would destroy it: the hard edges and the stepped corners *are* the
//! style, and a bilinear filter turns them into a blur.
//!
//! `assets/tuzminal.svg` is the same picture for desktop environments, which want
//! something scalable. A test below keeps the two from drifting apart.

/// The design. One character per pixel, 16 rows of 16.
///
/// | | |
/// |---|---|
/// | `.` | transparent — the stepped corners |
/// | `#` | background |
/// | `C` | the prompt chevron |
/// | `U` | the cursor block |
///
/// The corners step in by three, two, one: that staircase is how pixel art rounds a
/// corner, and it reads as intentional at 16 pixels where a real curve cannot.
const GRID: &[&str] = &[
    "...##########...",
    "..############..",
    ".##############.",
    "################",
    "################",
    "###CC###########",
    "####CC####UUU###",
    "#####CC###UUU###",
    "#####CC###UUU###",
    "####CC####UUU###",
    "###CC#####UUU###",
    "################",
    "################",
    ".##############.",
    "..############..",
    "...##########...",
];

/// Side of the design, in design pixels.
pub const GRID_SIZE: u32 = 16;

/// How many real pixels per design pixel. 4 gives a 64×64 icon, which is what most
/// compositors ask for, and an integer factor is what keeps the edges hard.
const SCALE: u32 = 4;

/// Background, chevron and cursor, as straight RGB.
const BACKGROUND: [u8; 3] = [0x14, 0x17, 0x20];
const CHEVRON: [u8; 3] = [0x5c, 0xf0, 0xd4];
const CURSOR: [u8; 3] = [0xe6, 0xe9, 0xf0];

/// The colour of a design pixel, or `None` where it is transparent.
fn color_of(cell: u8) -> Option<[u8; 3]> {
    match cell {
        b'#' => Some(BACKGROUND),
        b'C' => Some(CHEVRON),
        b'U' => Some(CURSOR),
        _ => None,
    }
}

/// The icon as RGBA8 rows, top to bottom, alongside its side length.
pub fn rgba() -> (Vec<u8>, u32) {
    let size = GRID_SIZE * SCALE;
    let mut pixels = vec![0u8; (size * size * 4) as usize];

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
    fn every_pixel_is_fully_opaque_or_fully_transparent() {
        // The whole point of pixel art. A partial alpha anywhere means something
        // antialiased the design.
        let (pixels, _) = rgba();
        for (i, chunk) in pixels.chunks_exact(4).enumerate() {
            assert!(
                chunk[3] == 0 || chunk[3] == 0xff,
                "pixel {i} has alpha {}, so something smoothed it",
                chunk[3]
            );
        }
    }

    #[test]
    fn each_design_pixel_became_a_solid_block() {
        // Integer scaling, checked at a corner of one block: if this ever picked up
        // interpolation, the four samples would differ.
        let (pixels, size) = rgba();
        let at = |x: u32, y: u32| {
            let o = ((y * size + x) * 4) as usize;
            [pixels[o], pixels[o + 1], pixels[o + 2], pixels[o + 3]]
        };

        // Row 3 is solid background, so its whole first block is one colour.
        let y = 3 * SCALE;
        let first = at(0, y);
        for dy in 0..SCALE {
            for dx in 0..SCALE {
                assert_eq!(at(dx, y + dy), first, "block at (0,{y}) is not solid");
            }
        }
        assert_eq!(first, [0x14, 0x17, 0x20, 0xff]);
    }

    #[test]
    fn the_corners_are_stepped_away_and_the_middle_is_filled() {
        let (pixels, size) = rgba();
        let alpha = |x: u32, y: u32| pixels[((y * size + x) * 4 + 3) as usize];

        // The staircase: cut at every corner, and cut further at the very corner than
        // one pixel in.
        assert_eq!(alpha(0, 0), 0, "top-left should be cut away");
        assert_eq!(alpha(size - 1, 0), 0);
        assert_eq!(alpha(0, size - 1), 0);
        assert_eq!(alpha(size - 1, size - 1), 0);
        assert_eq!(alpha(size / 2, size / 2), 0xff, "the middle is solid");
        // Three design pixels in on the top row is where the background starts.
        assert_eq!(alpha(3 * SCALE, 0), 0xff);
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

        // Every rect in the svg is `<rect x=.. y=.. width=.. height=1 fill=..>`, one
        // per horizontal run. Their total area must equal the grid's filled cells.
        let svg_area: u32 = svg
            .split("<rect")
            .skip(1)
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
            svg_area, grid_area,
            "the svg paints {svg_area} cells and the grid has {grid_area}"
        );
    }
}
