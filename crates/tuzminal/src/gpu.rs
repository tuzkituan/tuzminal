//! wgpu device, surface and swapchain management.
//!
//! Kept separate from the event loop so surface reconfiguration and colorspace
//! handling have one obvious home. Nothing here knows about terminals; it owns
//! the GPU resources a window needs and the rules for putting a
//! correctly-converted color on screen.

use anyhow::{Context, Result};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tuz_config::{Config, GpuBackend, PowerPreference, Rgba};
use winit::window::Window;

/// What happened when a frame was submitted.
///
/// Deliberately not `Result`: most non-success cases are routine (an occluded or
/// resizing window) and should skip a frame rather than look like errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameOutcome {
    /// Drawn and presented.
    Presented,
    /// Nothing was drawn and nothing is wrong — the window is occluded, or the
    /// compositor timed out.
    Skipped,
    /// Presented, but the swapchain has been reconfigured and the frame should be
    /// drawn again to be correct.
    Redraw,
    /// The device is unusable and the application should exit.
    Fatal,
}

/// GPU state tied to one window's surface.
pub struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,

    /// True when the surface format applies the sRGB encode on write, which
    /// decides whether colors must be linearized first.
    format_is_srgb: bool,
    /// True when the compositor expects channels already multiplied by alpha.
    /// Getting this wrong makes a transparent window look milky.
    premultiplied_alpha: bool,

    adapter_info: wgpu::AdapterInfo,

    /// Timings for the last frame, for diagnosing a slow resize.
    ///
    /// Kept here rather than measured by the caller because these are the two calls that
    /// talk to the compositor — a swapchain reconfiguration and an image acquisition —
    /// and they are the only part of a frame whose cost has nothing to do with how much
    /// work the frame contains.
    last_configure: Duration,
    last_acquire: Duration,
    last_present: Duration,
}

impl Gpu {
    /// Whether this config asks for rounded window corners.
    ///
    /// Decorations win: with the compositor drawing the frame, our rounding would cut
    /// holes inside its square border rather than shaping the window.
    fn rounds_corners(cfg: &Config) -> bool {
        !cfg.window.decorations && cfg.window.corner_radius > 0.0
    }

    /// Create a device and configure the window's surface.
    ///
    /// Blocking: wgpu's adapter and device requests are async, but there is
    /// nothing useful to do during startup and an async runtime would buy
    /// nothing.
    pub fn new(window: Arc<Window>, cfg: &Config) -> Result<Self> {
        let size = window.inner_size();

        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = backends_for(cfg.performance.gpu_backend);
        // Picks up WGPU_VALIDATION and friends so a user can turn on validation
        // without a rebuild.
        desc.flags = desc.flags.with_env();
        let instance = wgpu::Instance::new(desc);

        let surface = instance
            .create_surface(window.clone())
            .context("failed to create a GPU surface for the window")?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: match cfg.performance.power_preference {
                PowerPreference::LowPower => wgpu::PowerPreference::LowPower,
                PowerPreference::HighPerformance => wgpu::PowerPreference::HighPerformance,
            },
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            ..Default::default()
        }))
        .context(
            "no suitable GPU adapter found; try `performance.gpu_backend = \"gl\"` \
             in config.toml",
        )?;

        let adapter_info = adapter.get_info();
        log::info!(
            "GPU: {} ({:?}, {:?})",
            adapter_info.name,
            adapter_info.device_type,
            adapter_info.backend
        );

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("tuzminal-device"),
            // A terminal renderer needs nothing beyond the baseline. Staying on
            // default limits keeps the widest hardware working, including the GL
            // fallback path.
            ..Default::default()
        }))
        .context("failed to create a GPU device")?;

        // Surfaces reject zero-sized configuration, and a freshly mapped Wayland
        // window can legitimately report 0x0 before its first configure event.
        let width = size.width.max(1);
        let height = size.height.max(1);

        let mut config = surface
            .get_default_config(&adapter, width, height)
            .context("the GPU adapter does not support this window's surface")?;

        let caps = surface.get_capabilities(&adapter);

        // Prefer an sRGB format so the hardware does the gamma encode for us;
        // `Rgba::to_linear` then feeds it the values it expects.
        if let Some(srgb) = caps.formats.iter().copied().find(|f| f.is_srgb()) {
            config.format = srgb;
        }
        let format_is_srgb = config.format.is_srgb();

        // Rounded corners need a transparent surface for the same reason opacity
        // does: the pixels outside the curve are written with zero alpha, and an
        // opaque alpha mode turns them black instead of letting the desktop through.
        let want_transparency = cfg.window.opacity < 1.0 || Self::rounds_corners(cfg);
        let (alpha_mode, premultiplied_alpha) =
            pick_alpha_mode(&caps.alpha_modes, want_transparency);
        config.alpha_mode = alpha_mode;
        config.present_mode = pick_present_mode(&caps.present_modes, cfg.performance.vsync);
        // Two frames in flight, which is also wgpu's default.
        //
        // This was 1 for a while, on the reasoning that fewer buffered frames means less
        // latency per keystroke. That is backwards. With only one image in flight there is
        // nothing to acquire until the display has finished with the previous frame, so
        // the client *blocks* for a refresh interval before it can begin drawing — the
        // opposite of low latency. Measured, going from 1 to 2 halved the time spent in
        // `get_current_texture` and nearly doubled the frame rate; 3 was no better than 2.
        config.desired_maximum_frame_latency = 2;

        log::debug!(
            "surface: {:?} {:?} {:?} ({width}x{height})",
            config.format,
            config.present_mode,
            config.alpha_mode,
        );

        surface.configure(&device, &config);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            format_is_srgb,
            premultiplied_alpha,
            adapter_info,
            last_configure: Duration::ZERO,
            last_acquire: Duration::ZERO,
            last_present: Duration::ZERO,
        })
    }

    // The handles `tuz-render` needs to build pipelines and upload buffers.
    // Unused until that crate exists in M1.
    #[allow(dead_code)]
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }
    #[allow(dead_code)]
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }
    #[allow(dead_code)]
    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }
    pub fn adapter_info(&self) -> &wgpu::AdapterInfo {
        &self.adapter_info
    }

    /// Resize the swapchain. Ignores zero-sized requests, which arrive when a
    /// window is minimized and would otherwise be a validation error.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            log::trace!("ignoring resize to {width}x{height}");
            return;
        }
        if (width, height) == (self.config.width, self.config.height) {
            return;
        }
        self.config.width = width;
        self.config.height = height;

        // Timed because this is the expensive one and the easiest to overlook. A
        // `configure` destroys and recreates the swapchain, which on Vulkan means the
        // driver may wait for the GPU to finish with the old images first — nothing to do
        // with how much work the frame itself does.
        let started = Instant::now();
        self.surface.configure(&self.device, &self.config);
        self.last_configure = started.elapsed();
    }

    /// How long the last swapchain reconfiguration took.
    pub fn last_configure(&self) -> Duration {
        self.last_configure
    }

    /// How long the last frame spent waiting to acquire an image.
    pub fn last_acquire(&self) -> Duration {
        self.last_acquire
    }

    /// How long the last frame spent submitting and presenting.
    pub fn last_present(&self) -> Duration {
        self.last_present
    }

    /// Re-apply presentation settings after a config reload.
    pub fn reconfigure(&mut self, cfg: &Config) {
        self.config.present_mode = if cfg.performance.vsync {
            wgpu::PresentMode::AutoVsync
        } else {
            wgpu::PresentMode::AutoNoVsync
        };
        self.surface.configure(&self.device, &self.config);
    }

    /// Convert a theme color into the space this surface expects.
    ///
    /// Two corrections happen here and both are invisible until they are wrong:
    /// sRGB values must be linearized for an `*Srgb` surface, and channels must
    /// be premultiplied when the compositor blends that way.
    pub fn resolve_color(&self, color: Rgba, opacity: f32) -> wgpu::Color {
        let [r, g, b, _] = if self.format_is_srgb {
            color.to_linear()
        } else {
            color.to_unorm()
        };
        let a = (opacity.clamp(0.0, 1.0) * (color.a as f32 / 255.0)) as f64;
        let (r, g, b) = (r as f64, g as f64, b as f64);

        if self.premultiplied_alpha {
            wgpu::Color {
                r: r * a,
                g: g * a,
                b: b * a,
                a,
            }
        } else {
            wgpu::Color { r, g, b, a }
        }
    }

    /// Acquire a frame, clear it to `color`, and let `draw` record into the pass.
    ///
    /// The render pass is handed to the caller rather than owned here so one pass
    /// can cover every pane, with the caller setting scissor rects between draws.
    /// Acquiring and presenting stay in this module because the recoverable
    /// failure modes are surface concerns, not renderer concerns.
    pub fn render(
        &mut self,
        color: wgpu::Color,
        draw: impl FnOnce(&mut wgpu::RenderPass<'_>),
    ) -> FrameOutcome {
        use wgpu::CurrentSurfaceTexture as Acquired;

        // Acquisition blocks until the presentation engine has an image free, so this
        // is where waiting on the display shows up rather than in any of our own work.
        let acquire_started = Instant::now();
        let acquired = self.surface.get_current_texture();
        self.last_acquire = acquire_started.elapsed();

        let (frame, outcome) = match acquired {
            Acquired::Success(frame) => (frame, FrameOutcome::Presented),
            // Usable this frame, but the surface has drifted from its config.
            // Present it, then reconfigure so the next frame is correct.
            Acquired::Suboptimal(frame) => (frame, FrameOutcome::Redraw),

            // Routine: nothing to draw into right now.
            Acquired::Timeout | Acquired::Occluded => return FrameOutcome::Skipped,

            // Recoverable: reconfigure and draw again.
            Acquired::Outdated | Acquired::Lost => {
                self.recover();
                return FrameOutcome::Redraw;
            }

            Acquired::Validation => {
                log::error!("surface acquisition failed validation");
                return FrameOutcome::Fatal;
            }
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tuzminal-frame"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tuz-cells"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Clearing as the load op is free on tiled GPUs and saves
                        // drawing a full-screen background quad.
                        load: wgpu::LoadOp::Clear(color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            draw(&mut pass);
        }

        let present_started = Instant::now();
        self.queue.submit(Some(encoder.finish()));
        // Presenting lives on the queue as of wgpu 30, not on the texture.
        self.queue.present(frame);
        self.last_present = present_started.elapsed();

        if outcome == FrameOutcome::Redraw {
            self.recover();
        }
        outcome
    }

    /// Recover from a lost or outdated swapchain by reconfiguring it.
    pub fn recover(&mut self) {
        log::debug!("reconfiguring the surface");
        self.surface.configure(&self.device, &self.config);
    }
}

fn backends_for(backend: GpuBackend) -> wgpu::Backends {
    match backend {
        // Honors WGPU_BACKEND, so a user can override without editing config.
        GpuBackend::Auto => wgpu::Backends::from_env().unwrap_or_default(),
        GpuBackend::Vulkan => wgpu::Backends::VULKAN,
        GpuBackend::Metal => wgpu::Backends::METAL,
        GpuBackend::Dx12 => wgpu::Backends::DX12,
        GpuBackend::Gl => wgpu::Backends::GL,
    }
}

/// Choose an alpha mode, falling back to opaque when transparency is
/// unsupported. Returns whether colors must be premultiplied.
fn pick_alpha_mode(
    available: &[wgpu::CompositeAlphaMode],
    want_transparency: bool,
) -> (wgpu::CompositeAlphaMode, bool) {
    use wgpu::CompositeAlphaMode as Mode;

    if !want_transparency {
        return (Mode::Opaque, false);
    }
    // Preferred first: PreMultiplied is what Wayland compositors expect.
    for mode in [Mode::PreMultiplied, Mode::PostMultiplied, Mode::Inherit] {
        if available.contains(&mode) {
            return (mode, mode == Mode::PreMultiplied);
        }
    }
    log::warn!("this compositor does not support transparency; ignoring window.opacity");
    (Mode::Opaque, false)
}

fn pick_present_mode(available: &[wgpu::PresentMode], vsync: bool) -> wgpu::PresentMode {
    if vsync {
        // Mailbox is vsync in the sense that matters — it never tears, because it only
        // ever shows a whole frame at a vblank — but it does not make the *client* wait
        // for one. Fifo does: `get_current_texture` blocks until the display has released
        // an image, which was measured at 15.4ms of every 17.8ms frame on this machine.
        // A window cannot follow a pointer it spends a whole refresh interval behind.
        //
        // Measured with `--resize-bench`, mean per frame:
        //
        //     Fifo,    latency 1     acquire 15.36ms    frame 17.81ms
        //     Fifo,    latency 2     acquire  7.42ms    frame  9.91ms
        //     Mailbox, latency 2     acquire   144µs    frame  1.51ms
        //
        // The cost is one more image in flight, which for a terminal-sized surface is a
        // few megabytes. Fifo is the fallback because it is the only mode every backend
        // is required to support, so vsync can never fail outright.
        if available.contains(&wgpu::PresentMode::Mailbox) {
            return wgpu::PresentMode::Mailbox;
        }
        return wgpu::PresentMode::AutoVsync;
    }
    for mode in [
        wgpu::PresentMode::Immediate,
        wgpu::PresentMode::Mailbox,
        wgpu::PresentMode::AutoNoVsync,
    ] {
        if available.contains(&mode) {
            return mode;
        }
    }
    log::warn!("no tearing-capable present mode available; falling back to vsync");
    wgpu::PresentMode::AutoVsync
}

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::CompositeAlphaMode as Mode;
    use wgpu::PresentMode as Present;

    #[test]
    fn opaque_is_chosen_when_transparency_is_not_wanted() {
        let (mode, premul) = pick_alpha_mode(&[Mode::PreMultiplied], false);
        assert_eq!(mode, Mode::Opaque);
        assert!(!premul);
    }

    #[test]
    fn premultiplied_is_preferred_for_transparency() {
        let (mode, premul) = pick_alpha_mode(&[Mode::PostMultiplied, Mode::PreMultiplied], true);
        assert_eq!(mode, Mode::PreMultiplied);
        assert!(premul, "premultiplication must be reported to the caller");
    }

    #[test]
    fn post_multiplied_is_used_when_premultiplied_is_absent() {
        let (mode, premul) = pick_alpha_mode(&[Mode::PostMultiplied], true);
        assert_eq!(mode, Mode::PostMultiplied);
        assert!(!premul, "post-multiplied must not premultiply");
    }

    #[test]
    fn transparency_falls_back_to_opaque_rather_than_failing() {
        // A compositor without alpha support should cost the user their
        // transparency, not their terminal.
        let (mode, premul) = pick_alpha_mode(&[Mode::Opaque], true);
        assert_eq!(mode, Mode::Opaque);
        assert!(!premul);
    }

    #[test]
    fn vsync_resolves_without_consulting_capabilities() {
        assert_eq!(pick_present_mode(&[], true), Present::AutoVsync);
    }

    #[test]
    fn disabling_vsync_picks_a_tearing_mode_when_available() {
        assert_eq!(
            pick_present_mode(&[Present::Immediate], false),
            Present::Immediate
        );
        assert_eq!(
            pick_present_mode(&[Present::Mailbox], false),
            Present::Mailbox
        );
    }

    #[test]
    fn disabling_vsync_falls_back_when_no_tearing_mode_exists() {
        assert_eq!(
            pick_present_mode(&[Present::Fifo], false),
            Present::AutoVsync
        );
    }

    #[test]
    fn explicit_backend_selection_is_honored() {
        assert_eq!(backends_for(GpuBackend::Vulkan), wgpu::Backends::VULKAN);
        assert_eq!(backends_for(GpuBackend::Gl), wgpu::Backends::GL);
        assert_eq!(backends_for(GpuBackend::Metal), wgpu::Backends::METAL);
    }
}
