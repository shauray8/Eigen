// Bloom chain: each mip is a 4-tap filtered half of the one above.

struct Out {
    @builtin(position) clip: vec4f,
    @location(0) uv: vec2f,
}

@vertex
fn vs(@builtin(vertex_index) i: u32) -> Out {
    let q = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    var o: Out;
    o.clip = vec4f(q * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2f(q.x, 1.0 - q.y);
    return o;
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_samp: sampler;

@fragment
fn down(in: Out) -> @location(0) vec4f {
    let t = 1.0 / vec2f(textureDimensions(src));
    return 0.25 * (textureSampleLevel(src, src_samp, in.uv + vec2f(-t.x, -t.y), 0.0)
        + textureSampleLevel(src, src_samp, in.uv + vec2f(t.x, -t.y), 0.0)
        + textureSampleLevel(src, src_samp, in.uv + vec2f(-t.x, t.y), 0.0)
        + textureSampleLevel(src, src_samp, in.uv + vec2f(t.x, t.y), 0.0));
}
