// Depth-only pass into one face of the shadow cube. Textured cards are
// alpha-tested so cut-outs cast their silhouette, not their rectangle.

@group(0) @binding(0) var<uniform> view_proj: mat4x4f;
@group(1) @binding(0) var t_color: texture_2d<f32>;
@group(1) @binding(3) var t_samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4f,
    @location(0) uv: vec2f,
    @location(1) tex: vec4f,
    @location(2) alpha: f32,
}

@vertex
fn vs(
    @location(0) pos: vec3f,
    @location(2) m0: vec4f,
    @location(3) m1: vec4f,
    @location(4) m2: vec4f,
    @location(5) m3: vec4f,
    @location(9) color: vec4f,
    @location(11) uv: vec2f,
    @location(12) tex: vec4f,
) -> VsOut {
    var o: VsOut;
    o.clip = view_proj * mat4x4f(m0, m1, m2, m3) * vec4f(pos, 1.0);
    o.uv = uv * tex.y;
    o.tex = tex;
    o.alpha = color.a;
    return o;
}

@fragment
fn fs(i: VsOut) {
    let a = textureSample(t_color, t_samp, i.uv).a * i.alpha;
    if i.tex.x > 0.5 && a < 0.5 {
        discard;
    }
}
