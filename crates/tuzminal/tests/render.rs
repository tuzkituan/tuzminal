//! End-to-end rendering tests against a real GPU, without a window.
//!
//! Unit tests can prove the right instances are generated; only reading pixels
//! back proves they are actually drawn. These render to an offscreen texture and
//! inspect the result, which is what catches the failures that produce a
//! correct-looking instance buffer and a blank window: a wrong vertex layout, a
//! bad clip-space transform, an inverted UV, a shader that samples nothing.
//!
//! Skipped (not failed) when no GPU adapter is available, since a headless CI box
//! without a software rasterizer genuinely cannot run them.

use tuz_config::{Config, Rgba, Theme};
use tuz_core::{Session, TermSize};
use tuz_font::FontSystem;
use tuz_layout::PaneId;
use tuz_render::{build_pane, ColorSpace, Instance, PaneGeometry, Renderer};

const WIDTH: u32 = 256;
const HEIGHT: u32 = 128;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

struct Harness {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl Harness {
    /// Create a headless device, or `None` when no adapter exists.
    fn new() -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
            ..Default::default()
        }))
        .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("tuz-test-device"),
            ..Default::default()
        }))
        .ok()?;
        Some(Self { device, queue })
    }

    /// Draw `instances` over a cleared background and read the pixels back.
    fn render(&self, renderer: &mut Renderer, instances: &[Instance], clear: Rgba) -> Vec<u8> {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tuz-test-target"),
            size: wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        renderer.set_viewport(&self.queue, WIDTH, HEIGHT);
        renderer.upload_instances(&self.device, &self.queue, instances);

        // Readback rows must be padded to 256 bytes.
        let bytes_per_row = (WIDTH * 4).next_multiple_of(256);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tuz-test-readback"),
            size: (bytes_per_row * HEIGHT) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let [r, g, b, a] = clear.to_unorm();
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tuz-test-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: r as f64,
                            g: g as f64,
                            b: b as f64,
                            a: a as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            renderer.draw(&mut pass, 0..instances.len() as u32);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(HEIGHT),
                },
            },
            wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll failed");

        let data = slice.get_mapped_range().expect("mapping should succeed");
        // Strip the row padding so callers can index by x/y.
        let mut pixels = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
        for row in 0..HEIGHT {
            let start = (row * bytes_per_row) as usize;
            pixels.extend_from_slice(&data[start..start + (WIDTH * 4) as usize]);
        }
        drop(data);
        buffer.unmap();
        pixels
    }
}

fn pixel(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * WIDTH + x) * 4) as usize;
    pixels[i..i + 4].try_into().unwrap()
}

/// Count pixels that differ noticeably from the background.
fn non_background(pixels: &[u8], bg: Rgba) -> usize {
    pixels
        .chunks_exact(4)
        .filter(|p| {
            let d = (p[0] as i32 - bg.r as i32).abs()
                + (p[1] as i32 - bg.g as i32).abs()
                + (p[2] as i32 - bg.b as i32).abs();
            d > 24
        })
        .count()
}

fn fonts() -> FontSystem {
    FontSystem::new(
        &tuz_config::Font {
            family: "monospace".to_owned(),
            size: 16.0,
            ..Default::default()
        },
        1.0,
    )
    .expect("a monospace font is required for these tests")
}

fn geometry(fonts: &FontSystem) -> PaneGeometry {
    let m = fonts.metrics();
    PaneGeometry {
        origin: (0.0, 0.0),
        cell_width: m.width as f32,
        cell_height: m.height as f32,
    }
}

/// Snapshot a detached session after feeding it `bytes`.
fn frame_for(bytes: &[u8], cols: u16, rows: u16, theme: &Theme) -> tuz_core::TerminalFrame {
    let session = Session::detached(PaneId(1), TermSize::new(cols, rows, 8, 16));
    session.feed_for_test(bytes);
    let term = session.term().lock();
    tuz_core::snapshot(&term, theme, &Config::default(), true, true)
}

#[test]
fn a_solid_quad_lands_at_the_right_pixels() {
    // The most basic check, and the one that isolates the clip-space transform:
    // if the Y flip or the pixel-to-NDC maths is wrong, the rect appears mirrored
    // or offset and every later test fails confusingly.
    let Some(h) = Harness::new() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let mut renderer = Renderer::new(&h.device, FORMAT, fonts().atlas());

    let red = [1.0, 0.0, 0.0, 1.0];
    // A 20x10 rect at (10, 20), i.e. near the TOP-left, not the bottom.
    let instances = [Instance::solid(10.0, 20.0, 20.0, 10.0, red)];
    let pixels = h.render(&mut renderer, &instances, Rgba::BLACK);

    assert_eq!(pixel(&pixels, 15, 25), [255, 0, 0, 255], "inside the rect");
    assert_eq!(pixel(&pixels, 5, 25), [0, 0, 0, 255], "left of the rect");
    assert_eq!(pixel(&pixels, 15, 5), [0, 0, 0, 255], "above the rect");
    assert_eq!(pixel(&pixels, 15, 40), [0, 0, 0, 255], "below the rect");
    assert_eq!(pixel(&pixels, 40, 25), [0, 0, 0, 255], "right of the rect");
}

#[test]
fn quad_edges_are_exact_with_no_off_by_one() {
    let Some(h) = Harness::new() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let mut renderer = Renderer::new(&h.device, FORMAT, fonts().atlas());

    let white = [1.0, 1.0, 1.0, 1.0];
    let instances = [Instance::solid(0.0, 0.0, 4.0, 4.0, white)];
    let pixels = h.render(&mut renderer, &instances, Rgba::BLACK);

    // Half-open: [0,4) covered, 4 not.
    assert_eq!(pixel(&pixels, 0, 0), [255; 4], "first pixel");
    assert_eq!(pixel(&pixels, 3, 3), [255; 4], "last covered pixel");
    assert_eq!(pixel(&pixels, 4, 0), [0, 0, 0, 255], "one past the edge");
    assert_eq!(pixel(&pixels, 0, 4), [0, 0, 0, 255], "one below the edge");
}

#[test]
fn text_is_actually_rasterized_to_the_framebuffer() {
    // The end-to-end proof: terminal bytes -> grid -> glyphs -> atlas -> pixels.
    let Some(h) = Harness::new() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let theme = Theme::builtin_default();
    let mut fonts = fonts();
    let mut renderer = Renderer::new(&h.device, FORMAT, fonts.atlas());

    let frame = frame_for(b"Hello, world!", 40, 4, &theme);
    let geom = geometry(&fonts);
    let mut instances = Vec::new();
    build_pane(
        &mut instances,
        &frame,
        &mut fonts,
        geom,
        ColorSpace {
            srgb: false,
            opacity: 1.0,
        },
    );
    assert!(!instances.is_empty(), "text should produce instances");

    // The atlas must reach the GPU before the draw, or every glyph samples an
    // empty texture and the framebuffer stays blank.
    renderer.upload_atlas(&h.device, &h.queue, fonts.atlas_mut());
    let pixels = h.render(&mut renderer, &instances, theme.background);

    let lit = non_background(&pixels, theme.background);
    assert!(
        lit > 50,
        "expected glyph coverage on screen, only {lit} pixels differ from the background"
    );

    // And it is in the top-left region where row 0 was drawn, not scattered.
    let m = fonts.metrics();
    let first_row: usize = (0..m.height.min(HEIGHT))
        .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
        .filter(|(x, y)| {
            let p = pixel(&pixels, *x, *y);
            let bg = theme.background;
            (p[0] as i32 - bg.r as i32).abs() + (p[1] as i32 - bg.g as i32).abs() > 24
        })
        .count();
    assert!(
        first_row > 20,
        "row 0 should hold most of the text, got {first_row}"
    );
}

#[test]
fn a_colored_cell_background_paints_its_cell() {
    let Some(h) = Harness::new() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let theme = Theme::builtin_default();
    let mut fonts = fonts();
    let mut renderer = Renderer::new(&h.device, FORMAT, fonts.atlas());

    // A red background behind two spaces: background only, no glyph coverage.
    let frame = frame_for(b"\x1b[41m  ", 40, 4, &theme);
    let geom = geometry(&fonts);
    let mut instances = Vec::new();
    build_pane(
        &mut instances,
        &frame,
        &mut fonts,
        geom,
        ColorSpace {
            srgb: false,
            opacity: 1.0,
        },
    );

    renderer.upload_atlas(&h.device, &h.queue, fonts.atlas_mut());
    let pixels = h.render(&mut renderer, &instances, theme.background);

    let m = fonts.metrics();
    let inside = pixel(&pixels, m.width / 2, m.height / 2);
    let expected = theme.normal.red;
    let diff = (inside[0] as i32 - expected.r as i32).abs()
        + (inside[1] as i32 - expected.g as i32).abs()
        + (inside[2] as i32 - expected.b as i32).abs();
    assert!(
        diff < 12,
        "cell background should be theme red {expected:?}, got {inside:?}"
    );
}

#[test]
fn glyphs_are_tinted_by_the_cells_foreground_color() {
    // Monochrome glyphs are cached as white coverage and tinted in the shader, so
    // one cached bitmap serves every color. If the tint were baked in on the CPU,
    // or the multiply were dropped, text would come out white regardless of SGR.
    let Some(h) = Harness::new() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let theme = Theme::builtin_default();
    let mut fonts = fonts();
    let mut renderer = Renderer::new(&h.device, FORMAT, fonts.atlas());

    // Green text on the default background. Plain ASCII with heavy stems, because
    // U+2588 FULL BLOCK is missing from many monospace fonts and would silently
    // draw nothing — which is how this test failed the first time.
    let frame = frame_for(b"\x1b[32mWWWMMM", 40, 4, &theme);
    let geom = geometry(&fonts);
    let mut instances = Vec::new();
    build_pane(
        &mut instances,
        &frame,
        &mut fonts,
        geom,
        ColorSpace {
            srgb: false,
            opacity: 1.0,
        },
    );
    renderer.upload_atlas(&h.device, &h.queue, fonts.atlas_mut());
    let pixels = h.render(&mut renderer, &instances, theme.background);

    // Find the most-covered pixel in the first row and check its hue.
    let m = fonts.metrics();
    let mut best = [0u8; 4];
    // Must start below every possible score, or a dark background beats it and the
    // test reports "no green" without distinguishing that from "nothing drawn".
    let mut best_score = i32::MIN;
    for y in 0..m.height.min(HEIGHT) {
        for x in 0..(m.width * 3).min(WIDTH) {
            let p = pixel(&pixels, x, y);
            // Green *dominance*, not absolute green: a plain `g - r - b` scores
            // theme green (152,195,121) below the dark background, because green
            // text still carries substantial red and blue.
            let score = 2 * p[1] as i32 - p[0] as i32 - p[2] as i32;
            if score > best_score {
                best_score = score;
                best = p;
            }
        }
    }

    assert!(
        non_background(&pixels, theme.background) > 20,
        "no glyph coverage at all; the tint assertion below would be meaningless"
    );
    assert!(
        best[1] > best[0] && best[1] > best[2],
        "green text should render green-dominant, got {best:?}"
    );
    // And specifically the theme's green, not an arbitrary green.
    let expected = theme.normal.green;
    assert!(
        best[1] as i32 >= expected.g as i32 / 2,
        "green channel {} is too weak for theme green {expected:?}",
        best[1]
    );
}

#[test]
fn a_scissor_rect_confines_drawing_to_one_pane() {
    // What keeps a wide glyph or an italic descender in the last column from
    // bleeding into the neighbouring split.
    let Some(h) = Harness::new() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let mut renderer = Renderer::new(&h.device, FORMAT, fonts().atlas());

    // A rect covering the whole surface, clipped to the left half.
    let instances = [Instance::solid(
        0.0,
        0.0,
        WIDTH as f32,
        HEIGHT as f32,
        [1.0, 1.0, 1.0, 1.0],
    )];

    let texture = h.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    renderer.set_viewport(&h.queue, WIDTH, HEIGHT);
    renderer.upload_instances(&h.device, &h.queue, &instances);

    let bytes_per_row = (WIDTH * 4).next_multiple_of(256);
    let buffer = h.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (bytes_per_row * HEIGHT) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = h
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_scissor_rect(0, 0, WIDTH / 2, HEIGHT);
        renderer.draw(&mut pass, 0..1);
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    h.queue.submit(Some(encoder.finish()));

    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    h.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll failed");
    let data = slice.get_mapped_range().expect("mapping should succeed");

    let at = |x: u32, y: u32| -> [u8; 4] {
        let i = (y * bytes_per_row + x * 4) as usize;
        data[i..i + 4].try_into().unwrap()
    };
    assert_eq!(at(10, 10), [255; 4], "inside the scissor rect");
    assert_eq!(
        at(WIDTH - 10, 10),
        [0, 0, 0, 255],
        "outside the scissor rect must be untouched"
    );
    drop(data);
    buffer.unmap();
}

#[test]
fn an_empty_instance_buffer_draws_nothing_and_does_not_crash() {
    // An idle frame with no visible cells is normal; it must not be a validation
    // error or a panic.
    let Some(h) = Harness::new() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let mut renderer = Renderer::new(&h.device, FORMAT, fonts().atlas());
    let pixels = h.render(&mut renderer, &[], Rgba::rgb(1, 2, 3));

    assert_eq!(
        pixel(&pixels, 10, 10),
        [1, 2, 3, 255],
        "only the clear color"
    );
}

#[test]
fn two_split_panes_render_side_by_side_with_a_divider() {
    // Proves the multi-pane path end to end: real layout geometry, two independent
    // terminals, per-pane scissor clipping, and the divider between them. No
    // keystroke synthesis is available here, so the split is driven through the
    // same `Layout` API the keybinding handler calls.
    let Some(h) = Harness::new() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let theme = Theme::builtin_default();
    let mut fonts = fonts();
    let mut renderer = Renderer::new(&h.device, FORMAT, fonts.atlas());
    let metrics = fonts.metrics();

    // Split the window in two, exactly as `Action::SplitRight` does.
    let (mut layout, left) = tuz_layout::Layout::new();
    let right = layout
        .split(tuz_layout::Direction::Right)
        .expect("split should succeed");

    let opts = tuz_layout::LayoutOptions {
        padding_x: 0,
        padding_y: 0,
        center_grid: false,
        divider_width: 2,
        tab_bar_height: 0,
        status_bar_height: 0,
        tab_width: 180,
        min_tab_width: 60,
        buttons: Vec::new(),
        cell: tuz_layout::CellSize {
            width: metrics.width,
            height: metrics.height,
        },
    };
    let frame = layout.compute(tuz_layout::Rect::from_size(WIDTH, HEIGHT), &opts);
    assert_eq!(frame.panes.len(), 2);
    assert_eq!(frame.dividers.len(), 1);

    let colors = ColorSpace {
        srgb: false,
        opacity: 1.0,
    };

    // Distinct solid backgrounds per pane, so which pane painted which pixel is
    // unambiguous: red on the left, blue on the right.
    let mut instances = Vec::new();
    let mut ranges = Vec::new();
    for (geom, sgr) in frame.panes.iter().zip([&b"\x1b[41m"[..], &b"\x1b[44m"[..]]) {
        let mut bytes = sgr.to_vec();
        bytes.extend(std::iter::repeat_n(b' ', geom.cols as usize));
        let pane_frame = frame_for(&bytes, geom.cols, geom.rows, &theme);

        let start = instances.len() as u32;
        build_pane(
            &mut instances,
            &pane_frame,
            &mut fonts,
            PaneGeometry {
                origin: (geom.content.x as f32, geom.content.y as f32),
                cell_width: metrics.width as f32,
                cell_height: metrics.height as f32,
            },
            colors,
        );
        ranges.push((geom.rect, start..instances.len() as u32));
    }

    // The divider, drawn unclipped.
    let divider_start = instances.len() as u32;
    for divider in &frame.dividers {
        instances.push(Instance::solid(
            divider.rect.x as f32,
            divider.rect.y as f32,
            divider.rect.width as f32,
            divider.rect.height as f32,
            colors.convert(Rgba::rgb(0, 255, 0)),
        ));
    }
    let divider_end = instances.len() as u32;

    renderer.upload_atlas(&h.device, &h.queue, fonts.atlas_mut());
    renderer.set_viewport(&h.queue, WIDTH, HEIGHT);
    renderer.upload_instances(&h.device, &h.queue, &instances);

    let texture = h.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bytes_per_row = (WIDTH * 4).next_multiple_of(256);
    let buffer = h.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (bytes_per_row * HEIGHT) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = h
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        for (rect, range) in &ranges {
            pass.set_scissor_rect(
                rect.x as u32,
                rect.y as u32,
                rect.width.min(WIDTH),
                rect.height.min(HEIGHT),
            );
            renderer.draw(&mut pass, range.clone());
        }
        pass.set_scissor_rect(0, 0, WIDTH, HEIGHT);
        renderer.draw(&mut pass, divider_start..divider_end);
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    h.queue.submit(Some(encoder.finish()));

    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    h.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll failed");
    let data = slice.get_mapped_range().expect("mapping should succeed");
    let at = |x: u32, y: u32| -> [u8; 4] {
        let i = (y * bytes_per_row + x * 4) as usize;
        data[i..i + 4].try_into().unwrap()
    };

    let left_rect = frame.pane(left).unwrap().rect;
    let right_rect = frame.pane(right).unwrap().rect;
    // Only row 0 was filled, so sample inside it rather than at mid-height.
    let mid_y = metrics.height / 2;

    let l = at(left_rect.x as u32 + 4, mid_y);
    assert!(l[0] > l[2], "left pane should be red-dominant, got {l:?}");

    let r = at(right_rect.x as u32 + 4, mid_y);
    assert!(r[2] > r[0], "right pane should be blue-dominant, got {r:?}");

    // The divider sits between them and is neither pane's color.
    let d = at(frame.dividers[0].rect.x as u32, mid_y);
    assert!(
        d[1] > d[0] && d[1] > d[2],
        "divider should be green, got {d:?}"
    );

    drop(data);
    buffer.unmap();
}

#[test]
fn the_tab_bar_draws_a_strip_with_a_distinguished_active_tab() {
    // The chrome equivalent of the text test: prove the tab bar reaches the
    // framebuffer, that the active tab differs from the inactive one, and that the
    // strip does not paint over the pane area below it.
    let Some(h) = Harness::new() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let theme = Theme::builtin_default();
    let mut fonts = fonts();
    let mut renderer = Renderer::new(&h.device, FORMAT, fonts.atlas());
    let metrics = fonts.metrics();

    let bar = tuz_layout::Rect::new(0, 0, WIDTH, metrics.height + 8);
    let tabs = tuz_layout::tab_rects(bar, 2, 100, 60);
    assert_eq!(tabs.len(), 2);

    let labels = vec![
        tuz_render::TabLabel {
            title: "one",
            active: true,
            has_activity: false,
            show_close: false,
            close_hovered: false,
        },
        tuz_render::TabLabel {
            title: "two",
            active: false,
            has_activity: true,
            show_close: false,
            close_hovered: false,
        },
    ];

    let colors = ColorSpace {
        srgb: false,
        opacity: 1.0,
    };
    let mut instances = Vec::new();
    tuz_render::draw_tab_bar(
        &mut instances,
        &mut fonts,
        bar,
        &tabs,
        &[],
        &labels,
        &theme,
        colors,
        0.0,
    );
    assert!(!instances.is_empty());

    renderer.upload_atlas(&h.device, &h.queue, fonts.atlas_mut());
    // Clear to pure black so anything drawn is unambiguous.
    let pixels = h.render(&mut renderer, &instances, Rgba::BLACK);

    let mid_y = bar.height / 2;

    // The active tab uses the focused pane background.
    let active = pixel(&pixels, tabs[0].x as u32 + 4, mid_y);
    let expected = theme.background_focused();
    let diff = (active[0] as i32 - expected.r as i32).abs()
        + (active[1] as i32 - expected.g as i32).abs()
        + (active[2] as i32 - expected.b as i32).abs();
    assert!(
        diff < 12,
        "active tab should be {expected:?}, got {active:?}"
    );

    // The inactive tab is visibly different from the active one.
    let inactive = pixel(&pixels, tabs[1].x as u32 + 20, mid_y);
    assert_ne!(
        active, inactive,
        "the active and inactive tabs must be distinguishable"
    );

    // The marker bar sits along the bottom edge of the active tab.
    let marker = pixel(&pixels, tabs[0].x as u32 + 4, bar.height - 1);
    let cursor = theme.cursor();
    let marker_diff = (marker[0] as i32 - cursor.r as i32).abs()
        + (marker[1] as i32 - cursor.g as i32).abs()
        + (marker[2] as i32 - cursor.b as i32).abs();
    assert!(
        marker_diff < 24,
        "expected the active marker in the cursor color {cursor:?}, got {marker:?}"
    );

    // Titles were rasterized.
    let lit = non_background(&pixels, theme.split_divider());
    assert!(
        lit > 30,
        "expected tab titles on screen, only {lit} lit pixels"
    );

    // And nothing was drawn below the strip.
    let below = pixel(&pixels, WIDTH / 2, bar.height + 4);
    assert_eq!(
        below,
        [0, 0, 0, 255],
        "the tab bar must not paint into the pane area"
    );
}

#[test]
fn the_status_bar_draws_plugin_segments_with_their_own_colors() {
    let Some(h) = Harness::new() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let theme = Theme::builtin_default();
    let mut fonts = fonts();
    let mut renderer = Renderer::new(&h.device, FORMAT, fonts.atlas());
    let metrics = fonts.metrics();

    let bar = tuz_layout::Rect::new(0, 0, WIDTH, metrics.height + 4);
    let items = vec![tuz_render::StatusItem {
        text: "ok",
        foreground: Some("#000000"),
        background: Some("#00ff00"),
    }];

    let colors = ColorSpace {
        srgb: false,
        opacity: 1.0,
    };
    let mut instances = Vec::new();
    tuz_render::draw_status_bar(&mut instances, &mut fonts, bar, &items, &theme, colors);

    renderer.upload_atlas(&h.device, &h.queue, fonts.atlas_mut());
    let pixels = h.render(&mut renderer, &instances, Rgba::BLACK);

    // Right-anchored, so the plugin's green background is near the right edge.
    let mut found_green = false;
    for x in (WIDTH / 2)..WIDTH {
        let p = pixel(&pixels, x, bar.height / 2);
        if p[1] > 200 && p[0] < 80 && p[2] < 80 {
            found_green = true;
            break;
        }
    }
    assert!(
        found_green,
        "the plugin's segment background should be visible in the right half"
    );
}

#[test]
fn the_settings_panel_draws_over_a_dimmed_terminal() {
    // The chrome tests cover the strip; this covers the panel. What it catches is a
    // panel that computes correct widget rects and then draws nothing, or draws
    // outside itself over the terminal.
    let Some(h) = Harness::new() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let theme = Theme::builtin_default();
    let mut fonts = fonts();
    let mut renderer = Renderer::new(&h.device, FORMAT, fonts.atlas());
    let metrics = fonts.metrics();

    let window = tuz_layout::Rect::from_size(WIDTH, HEIGHT);
    let panel = tuz_render::center_panel(window, WIDTH * 3 / 4, HEIGHT * 3 / 4);

    let widgets = vec![
        tuz_ui::Widget::heading("Appearance"),
        tuz_ui::Widget::toggle(tuz_ui::WidgetId(1), "Ligatures", true),
        tuz_ui::Widget::button(tuz_ui::WidgetId(2), "Save"),
    ];
    let mut ui = tuz_ui::Ui::new();
    ui.focus(tuz_ui::WidgetId(1));

    let colors = ColorSpace {
        srgb: false,
        opacity: 1.0,
    };

    // Terminal content underneath, so the dim layer has something to dim.
    let mut instances = vec![Instance::solid(
        0.0,
        0.0,
        WIDTH as f32,
        HEIGHT as f32,
        colors.convert(Rgba::rgb(0, 200, 0)),
    )];

    tuz_render::draw_panel_frame(&mut instances, window, panel, &theme, colors);
    let body = tuz_render::draw_panel_title(
        &mut instances,
        &mut fonts,
        panel,
        "Tuzminal Settings",
        &theme,
        colors,
    );
    ui.layout(&widgets, body, metrics.height);
    ui.focus(tuz_ui::WidgetId(1));
    tuz_render::draw_widgets(&mut instances, &mut fonts, &ui, &theme, colors);

    renderer.upload_atlas(&h.device, &h.queue, fonts.atlas_mut());
    let pixels = h.render(&mut renderer, &instances, Rgba::BLACK);

    // Outside the panel, the green terminal content is dimmed but still green.
    let outside = pixel(&pixels, 2, HEIGHT / 2);
    assert!(
        outside[1] > outside[0] && outside[1] > outside[2],
        "outside the panel should still be green-dominant, got {outside:?}"
    );
    assert!(
        outside[1] < 200,
        "and dimmed rather than full brightness, got {}",
        outside[1]
    );

    // Inside the panel, the terminal behind is fully covered. Asserting "not green"
    // rather than an exact color, because any given pixel may land on a widget
    // background, a button border or the focus ring — the property that matters is
    // that the panel is opaque over the terminal.
    let inside = pixel(&pixels, panel.center_x() as u32, panel.center_y() as u32);
    assert!(
        !(inside[1] > inside[0] + 20 && inside[1] > inside[2] + 20),
        "the panel should hide the green terminal behind it, got {inside:?}"
    );
    // And it is dark, i.e. theme-derived rather than the bright content below.
    assert!(
        inside[1] < 120,
        "the panel interior should be dark, got {inside:?}"
    );

    // The focus ring is drawn in the cursor color somewhere inside the panel.
    let cursor = theme.cursor();
    let mut found_ring = false;
    for y in panel.y..panel.bottom() {
        for x in panel.x..panel.right() {
            let p = pixel(&pixels, x as u32, y as u32);
            let d = (p[0] as i32 - cursor.r as i32).abs()
                + (p[1] as i32 - cursor.g as i32).abs()
                + (p[2] as i32 - cursor.b as i32).abs();
            if d < 20 {
                found_ring = true;
                break;
            }
        }
        if found_ring {
            break;
        }
    }
    assert!(found_ring, "the focused row should have a visible ring");

    // And widget text was rasterized.
    let lit = non_background(&pixels, theme.background);
    assert!(lit > 100, "expected panel text, only {lit} lit pixels");
}
