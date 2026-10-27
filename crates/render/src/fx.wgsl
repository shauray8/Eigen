// Adjustment-layer effects: read the canvas composited so far, write the
// effected image. All sizes are in target pixels.

struct Fx {
    res: vec2f,
    cell: f32,
    amount: f32,
    kind: f32,
    time: f32,
    _pad: vec2f,
    color: vec4f,
}

@group(0) @binding(0) var<uniform> fx: Fx;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct Out {
    @builtin(position) clip: vec4f,
    @location(0) uv: vec2f,
}

@vertex
fn vs(@builtin(vertex_index) i: u32) -> Out {
    let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    var o: Out;
    o.clip = vec4f(p * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2f(p.x, 1.0 - p.y);
    return o;
}

// PCG-based hash: no visible patterns, unlike sin/fract hashes.
fn hash(p: vec2f) -> f32 {
    var v = u32(i32(p.x)) * 1664525u + u32(i32(p.y)) * 1013904223u;
    v = v * 747796405u + 2891336453u;
    v = ((v >> ((v >> 28u) + 4u)) ^ v) * 277803737u;
    v = (v >> 22u) ^ v;
    return f32(v) / 4294967295.0;
}

fn at(px: vec2f) -> vec4f {
    return textureSampleLevel(tex, samp, px / fx.res, 0.0);
}

fn luma(c: vec3f) -> f32 {
    return dot(c, vec3f(0.2126, 0.7152, 0.0722));
}

@fragment
fn fs(i: Out) -> @location(0) vec4f {
    let px = i.uv * fx.res;
    let src = at(px);
    let k = u32(fx.kind + 0.5);
    var c = src;
    switch k {
        case 0u: { // blur: golden-angle spiral
            var acc = vec4f(0.0);
            let n = 48;
            for (var s = 0; s < n; s++) {
                let r = sqrt((f32(s) + 0.5) / f32(n)) * fx.cell;
                let a = f32(s) * 2.39996;
                acc += at(px + vec2f(cos(a), sin(a)) * r);
            }
            c = acc / f32(n);
        }
        case 1u: { // pixelate
            c = at((floor(px / fx.cell) + 0.5) * fx.cell);
        }
        case 2u: { // halftone: rotated dot grid, dot size from darkness
            let ang = 0.785398;
            let rot = mat2x2f(cos(ang), -sin(ang), sin(ang), cos(ang));
            let q = rot * px;
            let cell_q = (floor(q / fx.cell) + 0.5) * fx.cell;
            let center = transpose(rot) * cell_q;
            let s = at(center);
            let radius = fx.cell * 0.5 * sqrt(clamp(1.0 - luma(s.rgb), 0.0, 1.0)) * 1.3;
            let cov = clamp(radius - length(q - cell_q) + 0.5, 0.0, 1.0);
            let ink = select(s.rgb, fx.color.rgb, fx.color.a > 0.0);
            c = vec4f(mix(vec3f(0.97), ink, cov), 1.0);
        }
        case 3u: { // chromatic split
            let d = vec2f(fx.cell, 0.0);
            c = vec4f(at(px + d).r, src.g, at(px - d).b, src.a);
        }
        case 4u: { // film grain
            let n = hash(floor(px) + vec2f(fx.time * 7919.0, fx.time * 104729.0)) - 0.5;
            c = vec4f(src.rgb + n * 0.3 * (0.15 + luma(src.rgb)), src.a); // grain lives in the mids
        }
        case 5u: { // glitch: displaced row bands + split
            let row = floor(px.y / fx.cell);
            let t = floor(fx.time / 2.0);
            let r = hash(vec2f(row, t));
            var shift = 0.0;
            if r > 0.55 {
                shift = (hash(vec2f(t, row)) - 0.5) * fx.res.x * 0.18;
            }
            let q = px + vec2f(shift, 0.0);
            c = vec4f(at(q + vec2f(fx.cell * 0.4, 0.0)).r, at(q).g, at(q - vec2f(fx.cell * 0.4, 0.0)).b, 1.0);
        }
        case 7u: { // grade: filmic contrast, split-tone (color = shadow tint, alpha = strength), vignette (cell 0-100)
            let s = src.rgb * src.rgb * (3.0 - 2.0 * src.rgb);
            var g = mix(src.rgb, s, 0.55);
            let l = luma(g);
            let tint = fx.color.rgb - 0.5;
            g += tint * fx.color.a * (1.0 - l) * 0.5 - tint * fx.color.a * l * 0.3;
            g = mix(vec3f(luma(g)), g, 0.8 + 0.2 * l);
            let d = length((i.uv - 0.5) * vec2f(fx.res.x / fx.res.y, 1.0));
            g *= 1.0 - fx.cell / 100.0 * smoothstep(0.3, 1.05, d);
            c = vec4f(clamp(g, vec3f(0.0), vec3f(1.0)), src.a);
        }
        case 8u: { // stipple: random ink dots on warm paper, density from darkness (whites go clean), reseeded every 2 frames
            let cs = max(fx.cell, 1.0);
            let cellp = floor(px / cs);
            let s = at((cellp + 0.5) * cs);
            let d = smoothstep(0.55, 0.05, pow(luma(s.rgb), 1.0 / 2.2)); // perceptual lightness
            let seed = floor(fx.time / 2.0);
            let n = hash(cellp + vec2f(seed * 7919.0, seed * 3571.0));
            let ink = select(s.rgb * 0.35, fx.color.rgb, fx.color.a > 0.0);
            let paper = vec3f(0.761, 0.730, 0.651); // #e2ded3
            c = vec4f(mix(paper, ink, select(0.0, 1.0, n < d)), 1.0);
        }
        default: { // invert
            c = vec4f(1.0 - src.rgb, src.a);
        }
    }
    return mix(src, c, clamp(fx.amount, 0.0, 1.0));
}
