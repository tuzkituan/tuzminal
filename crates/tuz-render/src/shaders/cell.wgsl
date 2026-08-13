// One pipeline draws everything a terminal needs: cell backgrounds, glyphs,
// underlines, strikethroughs and the cursor. Each is an instanced quad, and the
// only difference is whether it samples the glyph atlas.
//
// Drawing them all through one pipeline means one draw call per frame and no
// pipeline switches. Ordering is the caller's job: instances are drawn in buffer
// order with no depth test, so backgrounds must be appended before glyphs.

struct Uniforms {
    // Viewport size in physical pixels, for the pixel -> clip space transform.
    screen: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var atlas_texture: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

// Flag bits, mirrored in instance.rs.
const FLAG_TEXTURED: u32 = 1u;
const FLAG_COLOR_GLYPH: u32 = 2u;

struct Instance {
    @location(0) position: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv: vec4<f32>,
    @location(3) color: vec4<f32>,
    @location(4) flags: u32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) flags: u32,
};

// Unit-quad corner for a vertex index, as two triangles.
fn corner(index: u32) -> vec2<f32> {
    switch index {
        case 0u: { return vec2<f32>(0.0, 0.0); }
        case 1u: { return vec2<f32>(1.0, 0.0); }
        case 2u: { return vec2<f32>(0.0, 1.0); }
        case 3u: { return vec2<f32>(0.0, 1.0); }
        case 4u: { return vec2<f32>(1.0, 0.0); }
        default: { return vec2<f32>(1.0, 1.0); }
    }
}

@vertex
fn vs_main(
    @builtin(vertex_index) vertex_index: u32,
    instance: Instance,
) -> VertexOutput {
    let unit = corner(vertex_index);
    let pixel = instance.position + unit * instance.size;

    // Pixels to clip space. Y is flipped because pixel space grows downward
    // while clip space grows upward.
    let ndc = vec2<f32>(
        pixel.x / uniforms.screen.x * 2.0 - 1.0,
        1.0 - pixel.y / uniforms.screen.y * 2.0,
    );

    var out: VertexOutput;
    out.clip_position = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = mix(instance.uv.xy, instance.uv.zw, unit);
    out.color = instance.color;
    out.flags = instance.flags;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    if (in.flags & FLAG_TEXTURED) == 0u {
        // A solid rect: cell background, underline, strikethrough or cursor.
        return in.color;
    }

    let texel = textureSample(atlas_texture, atlas_sampler, in.uv);

    if (in.flags & FLAG_COLOR_GLYPH) != 0u {
        // Color emoji carry their own color; only the instance's alpha applies,
        // so a faded pane fades the emoji too.
        return vec4<f32>(texel.rgb, texel.a) * in.color.a;
    }

    // A monochrome glyph is stored as white with coverage in alpha, so tinting is
    // a multiply. Keeping coverage in alpha (rather than pre-tinting on the CPU)
    // is what lets one cached bitmap serve every color the glyph appears in.
    return vec4<f32>(in.color.rgb, in.color.a * texel.a);
}
