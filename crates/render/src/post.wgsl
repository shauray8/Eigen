// Lens and film for a rendered 3D view: atmospheric haze, depth of field
// (scatter-as-gather over a lens disc), bloom from the HDR mip chain, and
// ACES tonemapping. Input is linear premultiplied HDR plus the MSAA depth.

struct Post {
    res: vec2f,
    near: f32,
    far: f32,
    coc_k: f32,     // blur diameter in px = coc_k * |1/focus - 1/depth|
    inv_focus: f32,
    max_coc: f32,   // largest blur radius in px
    haze: f32,      // density per scene unit
    haze_color: vec4f,
    bloom: f32,
    lods: f32,      // mips available for bloom
    grain: f32,
    vignette: f32,
    frame: vec4f,   // x: frame (grain seed)
}

@group(0) @binding(0) var<uniform> p: Post;
@group(1) @binding(0) var hdr: texture_2d<f32>;
@group(1) @binding(1) var depth: texture_depth_multisampled_2d;
@group(1) @binding(2) var samp: sampler;

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

fn texel(q: vec2f) -> vec2i {
    return clamp(vec2i(q), vec2i(0), vec2i(p.res) - 1);
}

fn lin_depth(px: vec2i) -> f32 {
    let z = textureLoad(depth, px, 0);
    if z >= 1.0 {
        return 1e6;
    }
    return p.far * p.near / (p.far - z * (p.far - p.near));
}

// Blur radius in px at a depth.
fn coc(d: f32) -> f32 {
    return min(0.5 * p.coc_k * abs(p.inv_focus - 1.0 / d), p.max_coc);
}

// The scene color at a pixel, seen through haze.
fn hazed(px: vec2i, d: f32) -> vec4f {
    let c = textureLoad(hdr, px, 0);
    if p.haze <= 0.0 {
        return c;
    }
    let f = 1.0 - exp(-p.haze * min(d, 1e5));
    return vec4f(c.rgb * (1.0 - f) + p.haze_color.rgb * f, c.a * (1.0 - f) + f);
}

fn hash(q: vec2f) -> f32 {
    var r = fract(q * vec2f(0.1031, 0.1030));
    r += dot(r, r.yx + 33.33);
    return fract((r.x + r.y) * r.x);
}

fn karis(c: vec4f) -> f32 {
    return 1.0 / (1.0 + max(max(c.r, c.g), c.b) / max(c.a, 1e-3));
}

fn aces(x: vec3f) -> vec3f {
    return clamp((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14), vec3f(0.0), vec3f(1.0));
}

@fragment
fn fs(in: Out) -> @location(0) vec4f {
    let pc = in.clip.xy;
    let px = texel(pc);
    let d0 = lin_depth(px);
    var c = hazed(px, d0);

    if p.coc_k > 0.0 {
        // Blur radius: this pixel's own, or a nearer blurred object's that reaches over it.
        let c0 = coc(d0);
        var r_max = c0;
        let spin = fract(52.982919 * fract(dot(floor(pc), vec2f(0.06711056, 0.00583715)))) * 6.2831853;
        for (var i = 0u; i < 12u; i++) {
            let a = spin + f32(i) * 2.3999632;
            let r = p.max_coc * (0.25 + 0.75 * f32(i % 3u) / 2.0);
            let dq = lin_depth(texel(pc + vec2f(cos(a), sin(a)) * r));
            if dq < d0 {
                r_max = max(r_max, min(coc(dq), r + coc(dq)));
            }
        }
        if r_max > 0.5 {
            // Samples are weighted down by brightness (Karis) so a lone hot
            // highlight can't sparkle through the blur.
            var sum = c * karis(c);
            var wsum = karis(c);
            let n = 96u;
            for (var i = 0u; i < n; i++) {
                let r = r_max * sqrt((f32(i) + 0.5) / f32(n));
                let a = spin + f32(i) * 2.3999632;
                let q = texel(pc + vec2f(cos(a), sin(a)) * r);
                let dq = lin_depth(q);
                // Things behind this pixel may not blur over it more than it is blurred itself.
                let cq = select(min(coc(dq), c0), coc(dq), dq < d0);
                let s = hazed(q, dq);
                let w = clamp(cq - r + 1.0, 0.0, 1.0) * karis(s);
                sum += s * w;
                wsum += w;
            }
            c = sum / wsum;
        }
    }

    if p.bloom > 0.0 {
        var b = vec4f(0.0);
        var bw = 0.0;
        for (var l = 1.0; l <= p.lods; l += 1.0) {
            let t = exp2(l) / p.res;
            let s = textureSampleLevel(hdr, samp, in.uv + vec2f(-t.x, -t.y) * 0.5, l)
                + textureSampleLevel(hdr, samp, in.uv + vec2f(t.x, -t.y) * 0.5, l)
                + textureSampleLevel(hdr, samp, in.uv + vec2f(-t.x, t.y) * 0.5, l)
                + textureSampleLevel(hdr, samp, in.uv + vec2f(t.x, t.y) * 0.5, l);
            b += s * 0.25;
            bw += 1.0;
        }
        c = mix(c, b / bw, clamp(p.bloom, 0.0, 1.0));
    }

    let a = c.a;
    var rgb = aces(c.rgb / max(a, 1e-4));
    if p.vignette > 0.0 {
        let d = length((in.uv - 0.5) * vec2f(p.res.x / p.res.y, 1.0));
        rgb *= 1.0 - p.vignette * smoothstep(0.35, 1.1, d);
    }
    if p.grain > 0.0 {
        // Soft grain (about 2 px clumps at 1080p), strongest in the mids, new every frame.
        let gp = in.uv * vec2f(p.res.y * 16.0 / 9.0, p.res.y) * 0.5 + vec2f(p.frame.x * 37.0, p.frame.x * 91.0);
        let i = floor(gp);
        let f = fract(gp);
        let u = f * f * (3.0 - 2.0 * f);
        let n = mix(mix(hash(i), hash(i + vec2f(1.0, 0.0)), u.x), mix(hash(i + vec2f(0.0, 1.0)), hash(i + vec2f(1.0, 1.0)), u.x), u.y) - 0.5;
        let l = dot(rgb, vec3f(0.2126, 0.7152, 0.0722));
        rgb = max(rgb + n * p.grain * 0.2 * (0.04 + l * (1.0 - l) * 2.0), vec3f(0.0));
    }
    return vec4f(rgb * a, a);
}
