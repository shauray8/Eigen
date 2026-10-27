// Canvas pipelines that draw in scene pixels: filled paths (text) and
// textured quads (views now, footage and images later).

struct Globals {
    res: vec2f,
    px: f32,
    _pad: f32,
}

@group(0) @binding(0) var<uniform> g: Globals;

fn to_clip(p: vec2f) -> vec4f {
    return vec4f(p.x / g.res.x * 2.0 - 1.0, 1.0 - p.y / g.res.y * 2.0, 0.0, 1.0);
}

struct PathOut {
    @builtin(position) clip: vec4f,
    @location(0) color: vec4f,
}

@vertex
fn path_vs(@location(0) pos: vec2f, @location(1) color: vec4f) -> PathOut {
    var o: PathOut;
    o.clip = to_clip(pos);
    o.color = color;
    return o;
}

@fragment
fn path_fs(i: PathOut) -> @location(0) vec4f {
    return i.color;
}

@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct ImageOut {
    @builtin(position) clip: vec4f,
    @location(0) uv: vec2f,
    @location(1) alpha: f32,
}

@vertex
fn image_vs(@location(0) pos: vec2f, @location(1) uv: vec2f, @location(2) alpha: f32) -> ImageOut {
    var o: ImageOut;
    o.clip = to_clip(pos);
    o.uv = uv;
    o.alpha = alpha;
    return o;
}

// Textures are premultiplied; output stays premultiplied.
@fragment
fn image_fs(i: ImageOut) -> @location(0) vec4f {
    return textureSample(tex, samp, i.uv) * i.alpha;
}
