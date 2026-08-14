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
const FLAG_ROUND_TL: u32 = 4u;
const FLAG_ROUND_TR: u32 = 8u;
const FLAG_ROUND_BL: u32 = 16u;
const FLAG_ROUND_BR: u32 = 32u;
const FLAG_ROUND_ANY: u32 = 60u;
const FLAG_RING: u32 = 64u;

struct Instance {
    @location(0) position: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv: vec4<f32>,
    @location(3) color: vec4<f32>,
    @location(4) flags: u32,
    @location(5) corner_radius: f32,
    @location(6) rotation: f32,
    @location(7) stroke_width: f32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) flags: u32,
    /// Position within the quad, in pixels, for the rounded-corner distance field.
    @location(3) local: vec2<f32>,
    @location(4) @interpolate(flat) quad_size: vec2<f32>,
    @location(5) @interpolate(flat) corner_radius: f32,
    @location(6) @interpolate(flat) stroke_width: f32,
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

    // Rotation is about the quad's own center, so a rotated icon stroke stays where
    // it was placed instead of swinging away from the origin. Zero rotation reduces
    // to the identity, which is every terminal cell.
    let half = instance.size * 0.5;
    let from_center = (unit - vec2<f32>(0.5, 0.5)) * instance.size;
    let c = cos(instance.rotation);
    let s = sin(instance.rotation);
    let turned = vec2<f32>(
        from_center.x * c - from_center.y * s,
        from_center.x * s + from_center.y * c,
    );
    let pixel = instance.position + half + turned;

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
    out.local = unit * instance.size;
    out.quad_size = instance.size;
    out.corner_radius = instance.corner_radius;
    out.stroke_width = instance.stroke_width;
    return out;
}

/// Coverage of a rounded rectangle at a point, antialiased over one pixel.
///
/// Corners are selected per-pair so a title bar can round only its top and still sit
/// flush against the content below. Returns 1.0 everywhere when no corner is
/// selected, which is the overwhelmingly common case — every terminal cell.
fn rounded_coverage(local: vec2<f32>, size: vec2<f32>, radius: f32, flags: u32) -> f32 {
    if (flags & FLAG_ROUND_ANY) == 0u || radius <= 0.0 {
        return 1.0;
    }

    // Each corner is selected independently, so a shape can round only where it meets
    // the outside of the window: the first tab rounds its top-left to sit in the
    // window's corner, while its other three stay square against its neighbours.
    let left = local.x < size.x * 0.5;
    let top = local.y < size.y * 0.5;
    var bit = FLAG_ROUND_BR;
    if (top && left) { bit = FLAG_ROUND_TL; }
    else if (top) { bit = FLAG_ROUND_TR; }
    else if (left) { bit = FLAG_ROUND_BL; }

    if (flags & bit) == 0u {
        return 1.0;
    }
    // One pixel of smoothing, so the curve is not a staircase.
    return 1.0 - smoothstep(-0.5, 0.5, rounded_distance(local, size, radius));
}

/// Signed distance to a rounded rectangle's edge: negative inside, positive outside.
fn rounded_distance(local: vec2<f32>, size: vec2<f32>, radius: f32) -> f32 {
    let half = size * 0.5;
    let p = local - half;
    let q = abs(p) - (half - vec2<f32>(radius, radius));
    return length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

/// Coverage of a band `stroke` wide lying just inside the edge.
///
/// Written separately from `rounded_coverage` rather than sharing its per-corner
/// early-out: that returns full coverage for a corner the flags leave square, which is
/// right for a fill and would punch a hole in a ring.
fn ring_coverage(local: vec2<f32>, size: vec2<f32>, radius: f32, stroke: f32) -> f32 {
    if stroke <= 0.0 {
        return 0.0;
    }
    let d = rounded_distance(local, size, radius);
    // The band is where the distance falls between -stroke and 0. Folding it about its
    // own middle turns that interval into one more distance field, so the inner and
    // outer edges are both antialiased by the same smoothstep.
    let band = abs(d + stroke * 0.5) - stroke * 0.5;
    return 1.0 - smoothstep(-0.5, 0.5, band);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    if (in.flags & FLAG_RING) != 0u {
        // An outline, hollow in the middle. Discarding rather than blending zero alpha
        // matters here: a ring is drawn last, over finished pixels, and it must leave
        // everything but its own band exactly as it found it.
        let ring = ring_coverage(in.local, in.quad_size, in.corner_radius, in.stroke_width);
        if ring <= 0.0 {
            discard;
        }
        return vec4<f32>(in.color.rgb, in.color.a * ring);
    }

    let coverage = rounded_coverage(in.local, in.quad_size, in.corner_radius, in.flags);
    if coverage <= 0.0 {
        discard;
    }

    if (in.flags & FLAG_TEXTURED) == 0u {
        // A solid rect: cell background, underline, strikethrough or cursor.
        return vec4<f32>(in.color.rgb, in.color.a * coverage);
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
