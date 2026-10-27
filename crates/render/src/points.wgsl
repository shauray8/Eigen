// Point clouds: each point is a round screen-space dot. A point shows when a
// hash reseeded every 2 frames is under its `keep`, so the cloud boils.
// Dot edges are anti-aliased over one pixel so slowly moving dots don't twinkle.
// With `lit`, scene lights shade each dot as if its normal pointed out from the
// object's pivot (a bust lit from one side stays lit from that side as it turns).

struct Light {
    pos: vec4f,
    color: vec4f, // rgb * energy
}

// Leading fields of the mesh globals (same bind group).
struct Globals {
    view_proj: mat4x4f,
    cam_pos: vec4f,
    ambient: vec4f, // rgb ambient, w = light count
    lights: array<Light, 8>,
}

struct Obj {
    model: mat4x4f,
    color: vec4f,
    params: vec4f, // x: dot diameter in target px, y: frame, zw: target size
    shade: vec4f,  // x: lit, yzw: world pivot
}

@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var<uniform> o: Obj;

struct VsOut {
    @builtin(position) clip: vec4f,
    @location(0) local: vec2f,     // 1.0 at the dot's rim
    @location(1) radius_px: f32,
    @location(2) tint: vec3f,
}

fn hash(p: vec2f) -> f32 {
    var q = fract(p * vec2f(0.1031, 0.1030));
    q += dot(q, q.yx + 33.33);
    return fract((q.x + q.y) * q.x);
}

@vertex
fn vs(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32, @location(0) p: vec4f) -> VsOut {
    var corners = array<vec2f, 6>(vec2f(-1, -1), vec2f(1, -1), vec2f(1, 1), vec2f(-1, -1), vec2f(1, 1), vec2f(-1, 1));
    // pad the quad by one pixel so the soft rim isn't clipped
    let r = max(o.params.x * 0.5, 0.5);
    let pad = (r + 1.0) / r;
    let c = corners[vi] * pad;
    var out: VsOut;
    out.local = c;
    out.radius_px = r;
    out.tint = vec3f(1.0);
    let seed = floor(o.params.y / 2.0);
    if (hash(vec2f(f32(ii) * 0.7071, seed * 13.37 + 1.0)) >= p.w) {
        out.clip = vec4f(2.0, 2.0, 2.0, 1.0); // culled
        return out;
    }
    let wp = (o.model * vec4f(p.xyz, 1.0)).xyz;
    if (o.shade.x > 0.0) {
        let n = normalize(wp - o.shade.yzw + vec3f(0.0, 1e-5, 0.0));
        var irr = g.ambient.rgb;
        for (var k = 0u; k < u32(g.ambient.w); k++) {
            let d = g.lights[k].pos.xyz - wp;
            // wrapped Lambert: dots are soft, so light creeps past the terminator
            let ndl = max((dot(n, normalize(d)) + 0.4) / 1.4, 0.0);
            irr += g.lights[k].color.rgb * ndl / max(dot(d, d), 0.01);
        }
        out.tint = mix(vec3f(1.0), irr, o.shade.x);
    }
    var clip = g.view_proj * vec4f(wp, 1.0);
    clip = vec4f(clip.xy + c * o.params.x / o.params.zw * clip.w, clip.zw);
    out.clip = clip;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4f {
    // coverage ramps across the rim over one pixel
    let a = clamp((1.0 - length(in.local)) * in.radius_px + 0.5, 0.0, 1.0);
    if (a <= 0.0) {
        discard;
    }
    return vec4f(o.color.rgb * in.tint * o.color.a * a, o.color.a * a);
}
