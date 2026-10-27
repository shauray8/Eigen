// ---- Provided by the editor to every fx plugin (an adjustment layer). ----
// A plugin defines:  fn effect(px: vec2f, src: vec4f) -> vec4f
// px: this pixel in target pixels; src: the canvas beneath at px (linear,
// premultiplied). Return the new pixel; the editor mixes it in by `amount`.

struct Fx {
    res: vec2f,      // target size in pixels
    cell: f32,       // the layer's `cell`, in target pixels
    amount: f32,     // 0..1 mix (already applied by the editor)
    kind: f32,
    frame: f32,      // scene frame
    seconds: f32,    // seconds since the layer's in point
    _pad: f32,
    color: vec4f,    // the layer's `color` (linear)
    params: vec4f,   // the layer's `params`
}

@group(0) @binding(0) var<uniform> fx: Fx;
@group(1) @binding(0) var canvas: texture_2d<f32>;
@group(1) @binding(1) var canvas_sampler: sampler;

struct EditorVsOut {
    @builtin(position) clip: vec4f,
    @location(0) uv: vec2f,
}

@vertex
fn editor_vs(@builtin(vertex_index) i: u32) -> EditorVsOut {
    let q = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    var o: EditorVsOut;
    o.clip = vec4f(q * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2f(q.x, 1.0 - q.y);
    return o;
}

// The canvas beneath at any pixel (bilinear).
fn at(px: vec2f) -> vec4f {
    return textureSampleLevel(canvas, canvas_sampler, px / fx.res, 0.0);
}

// Stable per-pixel random in 0..1.
fn hash(p: vec2f) -> f32 {
    var v = u32(i32(p.x)) * 1664525u + u32(i32(p.y)) * 1013904223u;
    v = v * 747796405u + 2891336453u;
    v = ((v >> ((v >> 28u) + 4u)) ^ v) * 277803737u;
    v = (v >> 22u) ^ v;
    return f32(v) / 4294967295.0;
}

@fragment
fn editor_fs(i: EditorVsOut) -> @location(0) vec4f {
    let px = i.uv * fx.res;
    let src = at(px);
    return mix(src, effect(px, src), clamp(fx.amount, 0.0, 1.0));
}
