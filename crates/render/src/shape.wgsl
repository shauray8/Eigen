struct Globals {
    res: vec2f,      // scene resolution in pixels
    px: f32,         // scene pixels per target pixel
    _pad: f32,
}

@group(0) @binding(0) var<uniform> g: Globals;

struct VsIn {
    @builtin(vertex_index) vi: u32,
    @location(0) center: vec2f,
    @location(1) size: vec2f,
    @location(2) color: vec4f,
    @location(3) params: vec4f, // rot (rad), radius, kind (0 rect, 1 ellipse), _
}

struct VsOut {
    @builtin(position) clip: vec4f,
    @location(0) local: vec2f,
    @location(1) half: vec2f,
    @location(2) color: vec4f,
    @location(3) params: vec4f,
}

@vertex
fn vs(v: VsIn) -> VsOut {
    var corners = array<vec2f, 6>(
        vec2f(-1.0, -1.0), vec2f(1.0, -1.0), vec2f(1.0, 1.0),
        vec2f(-1.0, -1.0), vec2f(1.0, 1.0), vec2f(-1.0, 1.0),
    );
    let half = v.size * 0.5;
    let local = corners[v.vi] * (half + vec2f(2.0 * g.px));
    let c = cos(v.params.x);
    let s = sin(v.params.x);
    let p = v.center + vec2f(local.x * c - local.y * s, local.x * s + local.y * c);
    var o: VsOut;
    o.clip = vec4f(p.x / g.res.x * 2.0 - 1.0, 1.0 - p.y / g.res.y * 2.0, 0.0, 1.0);
    o.local = local;
    o.half = half;
    o.color = v.color;
    o.params = v.params;
    return o;
}

@fragment
fn fs(i: VsOut) -> @location(0) vec4f {
    var d: f32;
    if i.params.z > 0.5 {
        let k = length(i.local / i.half);
        d = (k - 1.0) * min(i.half.x, i.half.y);
    } else {
        let r = min(i.params.y, min(i.half.x, i.half.y));
        let q = abs(i.local) - i.half + vec2f(r);
        d = length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - r;
    }
    let a = clamp(0.5 - d / g.px, 0.0, 1.0);
    return vec4f(i.color.rgb, i.color.a * a);
}
