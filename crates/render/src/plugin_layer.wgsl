// ---- Provided by the editor to every shader-layer plugin. ----
// A plugin defines:  fn shade(uv: vec2f, px: vec2f) -> vec4f
// uv: 0..1 across the layer's box (y down); px: the same in box pixels.
// Return straight (not premultiplied) linear RGBA; the editor composites it.

struct Layer {
    size: vec2f,     // the layer's box in pixels (scene px x render scale)
    seconds: f32,    // seconds since the layer's in point
    frame: f32,      // scene frame
    color: vec4f,    // the layer's `color` (linear)
    params: vec4f,   // the layer's `params`
}

@group(0) @binding(0) var<uniform> layer: Layer;

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
    let c = shade(i.uv, i.uv * layer.size);
    let a = clamp(c.a, 0.0, 1.0);
    return vec4f(c.rgb * a, a);
}
