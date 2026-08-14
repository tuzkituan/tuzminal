//! The GPU renderer for Tuzminal.
//!
//! Everything on screen — cell backgrounds, glyphs, underlines, the cursor, split
//! dividers — is an instanced quad in a single buffer, drawn by a single pipeline
//! in one call. See [`instance`] for how a terminal snapshot becomes instances,
//! and `shaders/cell.wgsl` for the shader.
//!
//! # Why one pipeline
//!
//! Splitting backgrounds and glyphs into separate pipelines is the obvious design
//! and it is slower: it doubles the draw calls and adds a pipeline switch per
//! pane. Since both are textured quads differing only in whether they sample the
//! atlas, a flag in the instance is enough.

pub mod chrome;
pub mod instance;
pub mod text;
pub mod widget;

pub use chrome::{
    draw_chrome_buttons, draw_status_bar, draw_tab_bar, draw_tooltip, StatusItem, TabLabel,
};
pub use instance::{
    build_pane, ColorSpace, Instance, PaneGeometry, FLAG_COLOR_GLYPH, FLAG_TEXTURED,
};
pub use text::{draw_in_box, measure, Align};
pub use widget::{
    center_panel, draw_panel_frame, draw_panel_title, draw_scrollbar, draw_toasts, draw_widgets,
    Toast,
};

use tuz_font::atlas::{Atlas, BYTES_PER_PIXEL};

/// Uniforms shared by every instance. Must match `Uniforms` in the shader.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    screen: [f32; 2],
    _padding: [f32; 2],
}

/// Owns the pipeline, the atlas texture and the instance buffer.
pub struct Renderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    bind_group_layout: wgpu::BindGroupLayout,

    uniform_buffer: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    /// Capacity in instances, so growth can be amortized instead of reallocating
    /// every frame the content changes size.
    instance_capacity: usize,

    atlas_texture: wgpu::Texture,
    atlas_size: (u32, u32),
    sampler: wgpu::Sampler,
}

/// Initial instance buffer capacity — roughly a 200x50 grid of glyphs plus
/// backgrounds, so a normal window never reallocates on the first frame.
const INITIAL_INSTANCES: usize = 16384;

impl Renderer {
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat, atlas: &Atlas) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("tuz-cell-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/cell.wgsl").into()),
        });

        let atlas_texture = create_atlas_texture(device, atlas.width(), atlas.height());

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("tuz-atlas-sampler"),
            // Nearest filtering: glyphs are rasterized at exactly the size they
            // are drawn, so linear filtering would only blur them.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tuz-uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tuz-bind-group-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let bind_group = create_bind_group(
            device,
            &bind_group_layout,
            &uniform_buffer,
            &atlas_texture,
            &sampler,
        );

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tuz-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("tuz-cell-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(Instance::layout())],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    // Straight (non-premultiplied) alpha, matching how glyph
                    // coverage is stored in the atlas.
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // No culling: quad winding is generated in the shader and a
                // mistaken cull mode would silently draw nothing.
                cull_mode: None,
                ..Default::default()
            },
            // No depth buffer: draw order is the depth order.
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let instance_buffer = create_instance_buffer(device, INITIAL_INSTANCES);

        Self {
            pipeline,
            bind_group,
            bind_group_layout,
            uniform_buffer,
            instance_buffer,
            instance_capacity: INITIAL_INSTANCES,
            atlas_texture,
            atlas_size: (atlas.width(), atlas.height()),
            sampler,
        }
    }

    /// Tell the shader the viewport size.
    pub fn set_viewport(&self, queue: &wgpu::Queue, width: u32, height: u32) {
        let uniforms = Uniforms {
            screen: [width.max(1) as f32, height.max(1) as f32],
            _padding: [0.0; 2],
        };
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
    }

    /// Upload whatever changed in the atlas since the last frame.
    ///
    /// Uploads whole rows covering the dirty band rather than a tight rect: a
    /// sub-rect copy needs a per-row stride, while full rows are one contiguous
    /// slice, and the band is usually only a few rows tall anyway.
    pub fn upload_atlas(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, atlas: &mut Atlas) {
        // The atlas was reset to a different size; rebuild the texture.
        if self.atlas_size != (atlas.width(), atlas.height()) {
            self.atlas_texture = create_atlas_texture(device, atlas.width(), atlas.height());
            self.atlas_size = (atlas.width(), atlas.height());
            self.bind_group = create_bind_group(
                device,
                &self.bind_group_layout,
                &self.uniform_buffer,
                &self.atlas_texture,
                &self.sampler,
            );
            atlas.reset();
        }

        let Some(dirty) = atlas.take_dirty() else {
            return;
        };

        let y = dirty.y;
        let height = dirty.height.min(atlas.height().saturating_sub(y));
        if height == 0 {
            return;
        }

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.atlas_texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            atlas.rows(y, height),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(atlas.width() * BYTES_PER_PIXEL as u32),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width: atlas.width(),
                height,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Upload instances, growing the buffer if needed.
    pub fn upload_instances(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        instances: &[Instance],
    ) {
        if instances.is_empty() {
            return;
        }
        if instances.len() > self.instance_capacity {
            // Double rather than fit exactly, so a steadily growing window does
            // not reallocate every frame.
            let capacity = instances.len().next_power_of_two();
            log::debug!(
                "growing instance buffer {} -> {capacity}",
                self.instance_capacity
            );
            self.instance_buffer = create_instance_buffer(device, capacity);
            self.instance_capacity = capacity;
        }
        queue.write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(instances));
    }

    /// Record the draw call for `count` instances.
    ///
    /// The caller owns the render pass, which lets it set scissor rects per pane
    /// and reuse one pass for the whole window.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, range: std::ops::Range<u32>) {
        if range.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        // Six vertices per quad, generated in the shader from the vertex index.
        pass.draw(0..6, range);
    }
}

fn create_atlas_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("tuz-glyph-atlas"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // Unorm, not Srgb: glyph coverage is a mask, and applying a transfer
        // function to alpha would distort antialiasing.
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn create_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniforms: &wgpu::Buffer,
    texture: &wgpu::Texture,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("tuz-bind-group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

fn create_instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tuz-instances"),
        size: (capacity * std::mem::size_of::<Instance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniforms_match_the_shaders_expected_size() {
        // WGSL aligns a struct containing vec2<f32> to 8 bytes but the buffer must
        // still be 16-byte aligned for a uniform binding.
        assert_eq!(std::mem::size_of::<Uniforms>(), 16);
    }

    #[test]
    fn the_shader_source_is_embedded() {
        let src = include_str!("shaders/cell.wgsl");
        assert!(src.contains("fn vs_main"));
        assert!(src.contains("fn fs_main"));
        // The flag constants must agree between shader and Rust.
        assert!(src.contains("const FLAG_TEXTURED: u32 = 1u;"));
        assert!(src.contains("const FLAG_COLOR_GLYPH: u32 = 2u;"));
        assert!(src.contains("const FLAG_ROUND_TOP: u32 = 4u;"));
        assert!(src.contains("const FLAG_ROUND_BOTTOM: u32 = 8u;"));
        assert_eq!(FLAG_TEXTURED, 1);
        assert_eq!(FLAG_COLOR_GLYPH, 2);
        assert_eq!(instance::FLAG_ROUND_TOP, 4);
        assert_eq!(instance::FLAG_ROUND_BOTTOM, 8);
    }

    #[test]
    fn the_vertex_layout_stride_matches_the_instance_size() {
        let layout = Instance::layout();
        assert_eq!(layout.array_stride, std::mem::size_of::<Instance>() as u64);
        assert_eq!(layout.step_mode, wgpu::VertexStepMode::Instance);
        assert_eq!(layout.attributes.len(), 6);
    }

    #[test]
    fn vertex_attribute_offsets_are_contiguous_and_correct() {
        // A wrong offset here corrupts every attribute after it, and the symptom
        // is garbled geometry rather than an error.
        let layout = Instance::layout();
        let offsets: Vec<u64> = layout.attributes.iter().map(|a| a.offset).collect();
        assert_eq!(offsets, vec![0, 8, 16, 32, 48, 52]);
    }
}
