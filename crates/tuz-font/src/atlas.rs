//! A dynamic glyph atlas.
//!
//! Glyphs are packed into one large RGBA texture using a **shelf** allocator:
//! rows ("shelves") are filled left to right, and a new shelf opens above the
//! previous one when the current row runs out of width. For glyphs of a terminal
//! font — all roughly the same height — this wastes very little space while
//! staying trivial enough to be obviously correct, unlike a full skyline packer.
//!
//! Two details that are easy to get wrong and expensive to debug:
//!
//! - **Padding.** Every glyph gets a 1px transparent border. Without it, linear
//!   filtering samples the neighbouring glyph and characters grow faint ghosts of
//!   their neighbours along their edges.
//! - **Dirty tracking.** Uploading the whole texture every frame would dominate
//!   the frame time, so the atlas records the bounding box of newly written pixels
//!   and the renderer uploads only that.

/// Bytes per pixel. The atlas is RGBA8 rather than R8 so color emoji and
/// monochrome glyphs can share one texture and one draw call.
pub const BYTES_PER_PIXEL: usize = 4;

/// Transparent border around each glyph, to stop linear filtering bleeding
/// between neighbours.
const PADDING: u32 = 1;

/// A rectangular region of the atlas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtlasRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl AtlasRect {
    fn union(self, other: AtlasRect) -> AtlasRect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = (self.x + self.width).max(other.x + other.width);
        let bottom = (self.y + self.height).max(other.y + other.height);
        AtlasRect {
            x,
            y,
            width: right - x,
            height: bottom - y,
        }
    }
}

/// Why an insertion failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtlasError {
    /// No shelf has room and no new shelf fits. The caller should
    /// [`Atlas::reset`] and re-rasterize, or grow the atlas.
    Full,
    /// The glyph is larger than the atlas itself, so it can never fit.
    TooLarge,
}

pub struct Atlas {
    width: u32,
    height: u32,
    data: Vec<u8>,

    /// Bottom edge of the shelf currently being filled.
    shelf_y: u32,
    /// Height of the current shelf, set by the tallest glyph placed on it.
    shelf_height: u32,
    /// Next free x on the current shelf.
    cursor_x: u32,

    /// Bounding box of pixels written since the last [`Atlas::take_dirty`].
    dirty: Option<AtlasRect>,
    /// Number of glyphs currently packed, for diagnostics.
    count: usize,
}

impl Atlas {
    /// Create an empty, fully transparent atlas.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![0; width as usize * height as usize * BYTES_PER_PIXEL],
            shelf_y: 0,
            shelf_height: 0,
            cursor_x: 0,
            // The initial all-zero texture still has to reach the GPU, or the
            // first frame samples undefined memory.
            dirty: Some(AtlasRect {
                x: 0,
                y: 0,
                width,
                height,
            }),
            count: 0,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn data(&self) -> &[u8] {
        &self.data
    }
    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Fraction of the atlas consumed by shelves so far.
    pub fn utilization(&self) -> f32 {
        if self.height == 0 {
            return 0.0;
        }
        (self.shelf_y + self.shelf_height) as f32 / self.height as f32
    }

    /// Pack `pixels` (RGBA8, `width * height * 4` bytes) into the atlas.
    ///
    /// Returns the rect the glyph occupies, excluding its padding.
    pub fn insert(
        &mut self,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) -> Result<AtlasRect, AtlasError> {
        debug_assert_eq!(
            pixels.len(),
            width as usize * height as usize * BYTES_PER_PIXEL,
            "pixel buffer size must match the declared glyph size"
        );

        // Zero-sized glyphs (a space) are legitimate; give them an empty rect at
        // the origin rather than failing or consuming shelf space.
        if width == 0 || height == 0 {
            return Ok(AtlasRect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            });
        }

        let padded_w = width + PADDING * 2;
        let padded_h = height + PADDING * 2;

        if padded_w > self.width || padded_h > self.height {
            return Err(AtlasError::TooLarge);
        }

        // Open a new shelf when this glyph does not fit on the current one.
        if self.cursor_x + padded_w > self.width {
            self.shelf_y += self.shelf_height;
            self.shelf_height = 0;
            self.cursor_x = 0;
        }

        // A taller glyph extends the current shelf, which can push it past the
        // bottom of the atlas.
        let needed_height = padded_h.max(self.shelf_height);
        if self.shelf_y + needed_height > self.height {
            return Err(AtlasError::Full);
        }
        self.shelf_height = needed_height;

        let x = self.cursor_x + PADDING;
        let y = self.shelf_y + PADDING;

        for row in 0..height {
            let src = row as usize * width as usize * BYTES_PER_PIXEL;
            let dst = ((y + row) as usize * self.width as usize + x as usize) * BYTES_PER_PIXEL;
            let len = width as usize * BYTES_PER_PIXEL;
            self.data[dst..dst + len].copy_from_slice(&pixels[src..src + len]);
        }

        self.cursor_x += padded_w;
        self.count += 1;

        let rect = AtlasRect {
            x,
            y,
            width,
            height,
        };
        // Mark the padded region: the border pixels matter to the sampler even
        // though they are not part of the glyph.
        let padded = AtlasRect {
            x: x - PADDING,
            y: y - PADDING,
            width: padded_w,
            height: padded_h,
        };
        self.dirty = Some(match self.dirty {
            Some(d) => d.union(padded),
            None => padded,
        });

        Ok(rect)
    }

    /// Normalized texture coordinates for a rect: `[u0, v0, u1, v1]`.
    pub fn uv(&self, rect: AtlasRect) -> [f32; 4] {
        let w = self.width as f32;
        let h = self.height as f32;
        [
            rect.x as f32 / w,
            rect.y as f32 / h,
            (rect.x + rect.width) as f32 / w,
            (rect.y + rect.height) as f32 / h,
        ]
    }

    /// Take the region written since the last call, for uploading.
    pub fn take_dirty(&mut self) -> Option<AtlasRect> {
        self.dirty.take()
    }

    /// Rows `y..y+height` of the atlas, for a partial texture upload.
    ///
    /// Whole rows rather than a tight rect, because a sub-rect upload needs a
    /// row-stride copy while full rows are one contiguous slice.
    pub fn rows(&self, y: u32, height: u32) -> &[u8] {
        let start = y as usize * self.width as usize * BYTES_PER_PIXEL;
        let len = height as usize * self.width as usize * BYTES_PER_PIXEL;
        &self.data[start..start + len]
    }

    /// Drop every glyph and start over.
    ///
    /// Called when the atlas fills up or the font size changes. The caller must
    /// discard its glyph cache at the same time, or cached coordinates will point
    /// at stale pixels.
    pub fn reset(&mut self) {
        self.data.fill(0);
        self.shelf_y = 0;
        self.shelf_height = 0;
        self.cursor_x = 0;
        self.count = 0;
        self.dirty = Some(AtlasRect {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A solid block of one color, for checking placement.
    fn pixels(w: u32, h: u32, value: u8) -> Vec<u8> {
        vec![value; (w * h) as usize * BYTES_PER_PIXEL]
    }

    fn pixel_at(atlas: &Atlas, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * atlas.width() as usize + x as usize) * BYTES_PER_PIXEL;
        atlas.data()[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn a_new_atlas_is_transparent_and_fully_dirty() {
        let mut a = Atlas::new(64, 64);
        assert!(a.is_empty());
        assert!(a.data().iter().all(|&b| b == 0));
        // The initial upload must happen or the first frame samples garbage.
        assert_eq!(
            a.take_dirty(),
            Some(AtlasRect {
                x: 0,
                y: 0,
                width: 64,
                height: 64
            })
        );
        assert_eq!(a.take_dirty(), None, "dirty state should be consumed");
    }

    #[test]
    fn insertion_offsets_by_the_padding() {
        let mut a = Atlas::new(64, 64);
        let r = a.insert(4, 4, &pixels(4, 4, 0xff)).unwrap();
        // Not at (0,0): the padding border comes first.
        assert_eq!(
            r,
            AtlasRect {
                x: 1,
                y: 1,
                width: 4,
                height: 4
            }
        );
        assert_eq!(a.len(), 1);
    }

    #[test]
    fn pixels_land_where_the_rect_says() {
        let mut a = Atlas::new(64, 64);
        let r = a.insert(2, 2, &pixels(2, 2, 0xab)).unwrap();

        for dy in 0..2 {
            for dx in 0..2 {
                assert_eq!(
                    pixel_at(&a, r.x + dx, r.y + dy),
                    [0xab; 4],
                    "glyph pixel at +{dx},+{dy}"
                );
            }
        }
    }

    #[test]
    fn glyphs_are_separated_by_transparent_padding() {
        // The bug this prevents: linear filtering sampling the neighbouring
        // glyph, which makes every character grow faint edges of its neighbour.
        let mut a = Atlas::new(64, 64);
        let first = a.insert(4, 4, &pixels(4, 4, 0xff)).unwrap();
        let second = a.insert(4, 4, &pixels(4, 4, 0xff)).unwrap();

        assert!(second.x > first.x + first.width, "glyphs must not touch");
        let gap_x = first.x + first.width;
        assert_eq!(
            pixel_at(&a, gap_x, first.y),
            [0, 0, 0, 0],
            "the column between glyphs must stay transparent"
        );
    }

    #[test]
    fn a_full_shelf_wraps_to_a_new_one() {
        let mut a = Atlas::new(32, 64);
        // Each glyph occupies 10+2 = 12px, so two fit on a 32px shelf.
        let a1 = a.insert(10, 10, &pixels(10, 10, 1)).unwrap();
        let a2 = a.insert(10, 10, &pixels(10, 10, 2)).unwrap();
        let a3 = a.insert(10, 10, &pixels(10, 10, 3)).unwrap();

        assert_eq!(a1.y, a2.y, "first two share a shelf");
        assert!(a3.y > a1.y, "the third should start a new shelf");
        assert_eq!(a3.x, a1.x, "a new shelf restarts at the left edge");
    }

    #[test]
    fn a_new_shelf_clears_the_previous_shelf_height() {
        let mut a = Atlas::new(32, 128);
        // A tall glyph sets the shelf height, then wrapping must not inherit it
        // as an offset twice.
        a.insert(20, 30, &pixels(20, 30, 1)).unwrap();
        let second = a.insert(20, 10, &pixels(20, 10, 2)).unwrap();
        assert_eq!(
            second.y,
            30 + 2 + 1,
            "new shelf sits just above the tall one"
        );
    }

    #[test]
    fn a_glyph_bigger_than_the_atlas_is_rejected_as_too_large() {
        let mut a = Atlas::new(16, 16);
        assert_eq!(
            a.insert(32, 4, &pixels(32, 4, 1)),
            Err(AtlasError::TooLarge)
        );
        assert_eq!(
            a.insert(4, 32, &pixels(4, 32, 1)),
            Err(AtlasError::TooLarge)
        );
        // Exactly atlas-sized once padding is counted is also too large.
        assert_eq!(
            a.insert(16, 16, &pixels(16, 16, 1)),
            Err(AtlasError::TooLarge)
        );
    }

    #[test]
    fn running_out_of_vertical_space_reports_full_not_too_large() {
        // The distinction matters: Full is recoverable by resetting, TooLarge is
        // not, and conflating them causes an infinite reset loop.
        let mut a = Atlas::new(16, 16);
        assert!(a.insert(10, 10, &pixels(10, 10, 1)).is_ok());
        assert_eq!(a.insert(10, 10, &pixels(10, 10, 2)), Err(AtlasError::Full));
    }

    #[test]
    fn a_zero_sized_glyph_succeeds_without_consuming_space() {
        // A space has no pixels but still needs a cache entry.
        let mut a = Atlas::new(32, 32);
        let r = a.insert(0, 0, &[]).unwrap();
        assert_eq!(r.width, 0);
        assert_eq!(r.height, 0);
        assert!(a.is_empty(), "an empty glyph should not occupy a slot");
    }

    #[test]
    fn dirty_regions_accumulate_into_one_bounding_box() {
        let mut a = Atlas::new(64, 64);
        a.take_dirty(); // clear the initial full-texture upload

        a.insert(4, 4, &pixels(4, 4, 1)).unwrap();
        a.insert(4, 4, &pixels(4, 4, 2)).unwrap();

        let d = a.take_dirty().expect("insertions should mark dirty");
        // Must cover both glyphs and their padding.
        assert_eq!(d.x, 0);
        assert_eq!(d.y, 0);
        assert!(d.width >= 12, "should span both glyphs, got {}", d.width);
        assert!(d.height >= 6);
    }

    #[test]
    fn uv_coordinates_are_normalized() {
        let a = Atlas::new(100, 200);
        let uv = a.uv(AtlasRect {
            x: 10,
            y: 20,
            width: 30,
            height: 40,
        });
        assert_eq!(uv, [0.1, 0.1, 0.4, 0.3]);
    }

    #[test]
    fn rows_returns_the_expected_slice_length() {
        let a = Atlas::new(8, 8);
        assert_eq!(a.rows(0, 8).len(), 8 * 8 * BYTES_PER_PIXEL);
        assert_eq!(a.rows(2, 3).len(), 3 * 8 * BYTES_PER_PIXEL);
    }

    #[test]
    fn reset_clears_everything_and_marks_a_full_upload() {
        let mut a = Atlas::new(32, 32);
        a.insert(4, 4, &pixels(4, 4, 0xff)).unwrap();
        a.take_dirty();

        a.reset();
        assert!(a.is_empty());
        assert!(a.data().iter().all(|&b| b == 0), "pixels must be cleared");
        assert_eq!(
            a.take_dirty(),
            Some(AtlasRect {
                x: 0,
                y: 0,
                width: 32,
                height: 32
            }),
            "the cleared texture must be re-uploaded"
        );

        // And allocation restarts from the origin.
        let r = a.insert(4, 4, &pixels(4, 4, 1)).unwrap();
        assert_eq!((r.x, r.y), (1, 1));
    }

    #[test]
    fn utilization_grows_as_shelves_fill() {
        let mut a = Atlas::new(32, 100);
        assert_eq!(a.utilization(), 0.0);
        a.insert(10, 48, &pixels(10, 48, 1)).unwrap();
        assert!(a.utilization() > 0.4, "got {}", a.utilization());
    }

    #[test]
    fn many_insertions_stay_within_bounds() {
        // A fuzz-ish check that the packer never writes outside the buffer: any
        // arithmetic slip here is a panic or silent corruption.
        let mut a = Atlas::new(256, 256);
        let mut inserted = 0;
        for i in 0..500u32 {
            let w = 1 + i % 17;
            let h = 1 + i % 23;
            match a.insert(w, h, &pixels(w, h, (i % 256) as u8)) {
                Ok(r) => {
                    assert!(r.x + r.width <= 256);
                    assert!(r.y + r.height <= 256);
                    inserted += 1;
                }
                Err(AtlasError::Full) => break,
                Err(e) => panic!("unexpected {e:?} at glyph {i}"),
            }
        }
        assert!(inserted > 50, "only packed {inserted} glyphs");
    }
}
