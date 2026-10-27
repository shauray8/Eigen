// Export motion blur on the GPU: each sub-frame is sampled (linear, through
// the sRGB view) and added to a float target at the blend constant's weight;
// the sum is then copied back into the sRGB target, which encodes it.

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
fn fs(in: Out) -> @location(0) vec4f {
    return textureSampleLevel(src, src_samp, in.uv, 0.0);
}
