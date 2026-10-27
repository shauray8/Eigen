// Real-time principled surface: GGX lights (one may cast a soft cube-map shadow),
// an HDR environment (or a procedural studio) for reflections and diffuse
// light, glass (reflection + tinted transmission), and fur shells.
// Output is linear, premultiplied HDR; the post pass tonemaps.

struct Light {
    pos: vec4f,
    color: vec4f, // rgb * energy
}

struct Globals {
    view_proj: mat4x4f,
    cam_pos: vec4f,
    ambient: vec4f, // rgb ambient, w = light count
    lights: array<Light, 8>,
    shadow: vec4f, // x: shadow light index (-1 none), y: softness (world units), z: 1 / map size
    env: vec4f,    // x: has env map, y: yaw (radians), z: max mip level
    sh: array<vec4f, 9>, // env irradiance / pi, SH9 (rgb)
    shadow_vp: array<mat4x4f, 6>,
}

@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var shadow_map: texture_depth_2d_array;
@group(0) @binding(2) var shadow_samp: sampler_comparison;
@group(0) @binding(3) var env_map: texture_2d<f32>;
@group(0) @binding(4) var env_samp: sampler;
// Texture set (1x1 neutral textures when the object has none).
@group(1) @binding(0) var t_color: texture_2d<f32>;
@group(1) @binding(1) var t_normal: texture_2d<f32>;
@group(1) @binding(2) var t_rough: texture_2d<f32>;
@group(1) @binding(3) var t_samp: sampler;

struct VsIn {
    @location(0) pos: vec3f,
    @location(1) normal: vec3f,
    @location(2) m0: vec4f,
    @location(3) m1: vec4f,
    @location(4) m2: vec4f,
    @location(5) m3: vec4f,
    @location(6) n0: vec4f, // w: fur shell 0..1
    @location(7) n1: vec4f,
    @location(8) n2: vec4f,
    @location(9) color: vec4f,
    @location(10) mat: vec4f, // metal, rough, glass, fuzz
    @location(11) uv: vec2f,
    @location(12) tex: vec4f, // x: textured, y: uv scale, z: emit, w: soft
    @location(13) sway: vec4f, // x: amplitude, y..z: fade-in heights, w: seconds
}

// Wind bend in object space: slow gusts travelling across x, a sway per
// position, and a quick small flutter for leaves. Weighted by height.
fn wind(p: vec3f, s: vec4f) -> vec3f {
    if s.x == 0.0 {
        return vec3f(0.0);
    }
    let w = smoothstep(s.y, s.z, p.y);
    let t = s.w;
    let gust = 0.55 + 0.45 * sin(t * 0.7 - p.x * 0.09);
    let bend = sin(t * 1.25 + p.x * 0.31 + p.z * 0.17) * 0.65 + sin(t * 2.3 + p.x * 0.83 - p.y * 0.21) * 0.25;
    let flutter = sin(t * 7.0 + p.x * 4.1 + p.y * 3.3) * sin(t * 5.3 + p.y * 2.7);
    let dx = (bend * gust + flutter * 0.09) * w;
    let dy = (flutter * 0.05 - abs(bend) * 0.06) * w;
    let dz = sin(t * 1.6 + p.x * 0.5 + 1.3) * 0.3 * w * gust;
    return vec3f(dx, dy, dz) * s.x;
}

struct VsOut {
    @builtin(position) clip: vec4f,
    @location(0) world: vec3f,
    @location(1) normal: vec3f,
    @location(2) color: vec4f,
    @location(3) mat: vec4f,
    @location(4) local: vec3f,
    @location(5) shell: f32,
    @location(6) uv: vec2f,
    @location(7) tex: vec4f,
}

@vertex
fn vs(v: VsIn) -> VsOut {
    let model = mat4x4f(v.m0, v.m1, v.m2, v.m3);
    let normal_m = mat3x3f(v.n0.xyz, v.n1.xyz, v.n2.xyz);
    let shell = v.n0.w;
    // Fur shells grow along the normal; length relative to the unit primitive.
    let p = v.pos + v.normal * shell * v.mat.w * 0.18 + wind(v.pos, v.sway);
    let world = model * vec4f(p, 1.0);
    var o: VsOut;
    o.clip = g.view_proj * world;
    o.world = world.xyz;
    o.normal = normal_m * v.normal;
    o.color = v.color;
    o.mat = v.mat;
    o.local = v.pos;
    o.shell = shell;
    o.uv = v.uv * v.tex.y;
    o.tex = v.tex;
    return o;
}

const PI: f32 = 3.14159265;

fn hash3(p: vec3f) -> f32 {
    let q = fract(p * vec3f(0.1031, 0.1030, 0.0973));
    let r = q + dot(q, q.yxz + 33.33);
    return fract((r.x + r.y) * r.z);
}

// Studio environment: bright soft sky, warm grey floor, two softboxes.
fn env(d: vec3f, rough: f32) -> vec3f {
    let horizon = vec3f(0.92, 0.93, 0.95);
    let zenith = vec3f(0.62, 0.72, 0.92);
    let floor = vec3f(0.30, 0.29, 0.28);
    var c = mix(horizon, zenith, smoothstep(0.0, 0.7, d.y));
    c = mix(c, floor, smoothstep(0.0, -0.35, d.y));
    // Studio flags: a dark band just above the horizon and a dark ring below,
    // which give mirror finishes their contrast.
    let sharp = 0.02 + rough * 0.3;
    c *= mix(0.08, 1.0, smoothstep(0.0, sharp, abs(d.y - 0.12) - 0.06));
    c *= mix(0.15, 1.0, smoothstep(0.0, sharp, abs(d.y + 0.45) - 0.08));
    let w = 0.06 + rough * 0.5;
    let box1 = smoothstep(1.0 - w, 1.0 - w * 0.3, dot(d, normalize(vec3f(0.55, 0.55, 0.63))));
    let box2 = smoothstep(1.0 - w, 1.0 - w * 0.3, dot(d, normalize(vec3f(-0.7, 0.25, 0.3))));
    c += vec3f(5.0) * box1 * (1.0 - rough * 0.8) + vec3f(2.0, 2.2, 2.5) * box2 * (1.0 - rough * 0.8);
    // Rough surfaces see a blurrier, flatter environment.
    let avg = mix(horizon, zenith, 0.4) * 0.9;
    return mix(c, avg, rough * rough * 0.7);
}

// Environment space: the scene direction turned by the env yaw.
fn env_dir(d: vec3f) -> vec3f {
    let c = cos(g.env.y);
    let s = sin(g.env.y);
    return vec3f(c * d.x - s * d.z, d.y, s * d.x + c * d.z);
}

// Reflections: the HDR map (blurrier mips for rougher surfaces) or the studio.
fn env_spec(d: vec3f, rough: f32) -> vec3f {
    if g.env.x < 0.5 {
        return env(d, rough);
    }
    let r = env_dir(d);
    let uv = vec2f(atan2(r.x, -r.z) / (2.0 * PI) + 0.5, acos(clamp(r.y, -1.0, 1.0)) / PI);
    return textureSampleLevel(env_map, env_samp, uv, sqrt(rough) * g.env.z).rgb * g.ambient.x;
}

// Diffuse light from the HDR map's spherical harmonics.
fn env_irr(n: vec3f) -> vec3f {
    let d = env_dir(n);
    let c = g.sh;
    let r = c[0].rgb * 0.282095
        + (c[1].rgb * d.y + c[2].rgb * d.z + c[3].rgb * d.x) * 0.488603
        + (c[4].rgb * d.x * d.y + c[5].rgb * d.y * d.z + c[7].rgb * d.x * d.z) * 1.092548
        + c[6].rgb * 0.315392 * (3.0 * d.z * d.z - 1.0)
        + c[8].rgb * 0.546274 * (d.x * d.x - d.y * d.y);
    return max(r, vec3f(0.0)) * g.ambient.x;
}

// Soft shadow from the cube map (six 100-degree faces, so filter taps near a
// face edge stay inside it). 1 = lit.
fn shadow_at(p: vec3f, n: vec3f, light: vec3f, frag: vec2f) -> f32 {
    let d = p - light;
    let a = abs(d);
    var face = 0u;
    var major = a.x;
    if a.x >= a.y && a.x >= a.z {
        face = select(1u, 0u, d.x > 0.0);
    } else if a.y >= a.z {
        face = select(3u, 2u, d.y > 0.0);
        major = a.y;
    } else {
        face = select(5u, 4u, d.z > 0.0);
        major = a.z;
    }
    let span = 2.0 * major * 1.19175; // world size the face covers at this distance (tan 50deg)
    // Push the lookup off the surface: a texel or two, plus enough that the soft
    // kernel's taps on a surface tilted from the light don't shadow it (acne).
    let ndl = clamp(dot(n, normalize(light - p)), 0.0, 1.0);
    let q = p + n * (span * g.shadow.z * 2.0 + g.shadow.y * (1.0 - ndl) * 0.8);
    let c = g.shadow_vp[face] * vec4f(q, 1.0);
    let ndc = c.xyz / c.w;
    let uv = vec2f(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    let radius = clamp(g.shadow.y / span, g.shadow.z * 1.5, 0.07);
    var taps = array<vec2f, 12>(
        vec2f(-0.326, -0.406), vec2f(-0.840, -0.074), vec2f(-0.696, 0.457), vec2f(-0.203, 0.621),
        vec2f(0.962, -0.195), vec2f(0.473, -0.480), vec2f(0.519, 0.767), vec2f(0.185, -0.893),
        vec2f(0.507, 0.064), vec2f(0.896, 0.412), vec2f(-0.322, -0.933), vec2f(-0.792, -0.598));
    let ang = hash3(vec3f(frag, 7.0)) * 6.2831853;
    let rot = mat2x2f(cos(ang), sin(ang), -sin(ang), cos(ang));
    var lit = 0.0;
    for (var i = 0u; i < 12u; i++) {
        lit += textureSampleCompareLevel(shadow_map, shadow_samp, uv + rot * taps[i] * radius, face, ndc.z);
    }
    return lit / 12.0;
}

// The scene value ACES maps to `y`: unlit paint shows its own colors after tonemapping.
fn aces_inv(y: vec3f) -> vec3f {
    let t = clamp(y, vec3f(0.0), vec3f(0.985));
    let a = 2.43 * t - 2.51;
    let b = 0.59 * t - 0.03;
    let c = 0.14 * t;
    return (-b - sqrt(max(b * b - 4.0 * a * c, vec3f(0.0)))) / (2.0 * a);
}

struct FsOut {
    @location(0) color: vec4f,
    @builtin(sample_mask) mask: u32,
}

fn ggx(n_h: f32, a: f32) -> f32 {
    let a2 = a * a;
    let d = n_h * n_h * (a2 - 1.0) + 1.0;
    return a2 / (PI * d * d);
}

fn smith(n_v: f32, n_l: f32, a: f32) -> f32 {
    let k = (a + 1.0) * (a + 1.0) / 8.0;
    return (n_v / (n_v * (1.0 - k) + k)) * (n_l / (n_l * (1.0 - k) + k));
}

fn fresnel(cos_t: f32, f0: vec3f, rough: f32) -> vec3f {
    return f0 + (max(vec3f(1.0 - rough), f0) - f0) * pow(clamp(1.0 - cos_t, 0.0, 1.0), 5.0);
}

// Normal mapping without stored tangents (cotangent frame from derivatives).
fn perturb(n: vec3f, p: vec3f, uv: vec2f, tn: vec3f) -> vec3f {
    let dp1 = dpdx(p);
    let dp2 = dpdy(p);
    let duv1 = dpdx(uv);
    let duv2 = dpdy(uv);
    let dp2perp = cross(dp2, n);
    let dp1perp = cross(n, dp1);
    let t = dp2perp * duv1.x + dp1perp * duv2.x;
    let b = dp2perp * duv1.y + dp1perp * duv2.y;
    let inv = inverseSqrt(max(max(dot(t, t), dot(b, b)), 1e-12));
    return normalize(mat3x3f(t * inv, -b * inv, n) * tn);
}

@fragment
fn fs(i: VsOut) -> FsOut {
    // Sample textures first (they need uniform control flow).
    let tc = textureSample(t_color, t_samp, i.uv);
    let albedo = tc.rgb;
    let alpha = i.color.a * tc.a;
    let tn = textureSample(t_normal, t_samp, i.uv).xyz * 2.0 - 1.0;
    let tr = textureSample(t_rough, t_samp, i.uv).g;
    let mapped = perturb(normalize(i.normal), i.world, i.uv, tn);

    let metal = i.mat.x;
    let rough = clamp(i.mat.y * tr, 0.04, 1.0);
    let glass = i.mat.z;
    let fuzz = i.mat.w;
    var base = i.color.rgb * albedo;

    // Fur: each shell keeps only strands taller than it; strands thin toward the tip.
    if i.shell > 0.0 {
        let cell = floor(i.local * 140.0);
        let h = hash3(cell);
        if h < i.shell * 1.05 || fract(dot(i.local * 140.0, vec3f(1.0))) > 1.0 - i.shell * 0.6 {
            discard;
        }
    }
    let fur_ao = select(1.0, mix(0.45, 1.05, i.shell), fuzz > 0.0);

    let n = select(normalize(i.normal), mapped, i.tex.x > 0.5);
    let v = normalize(g.cam_pos.xyz - i.world);
    let n_v = max(dot(n, v), 1e-3);
    let a = rough * rough;
    let f0 = mix(vec3f(0.04), base, metal);
    let f = fresnel(n_v, f0, rough);

    // Image-based part.
    var irr = mix(vec3f(0.35, 0.33, 0.31), vec3f(0.95, 0.97, 1.0), n.y * 0.5 + 0.5) * g.ambient.rgb * 2.0;
    if g.env.x > 0.5 {
        irr = env_irr(n);
    }
    let diffuse = base * (1.0 - metal) * (1.0 - glass);
    var c = diffuse * irr * (1.0 - f) + env_spec(reflect(-v, n), rough) * f;

    // Direct lights.
    let count = u32(g.ambient.w);
    for (var k = 0u; k < count; k++) {
        let d = g.lights[k].pos.xyz - i.world;
        let l = normalize(d);
        let h = normalize(l + v);
        let raw = dot(n, l);
        // Fuzzy surfaces wrap light around (soft, velvety terminator).
        let n_l = max(mix(raw, (raw + 0.6) / 1.6, fuzz), 0.0);
        var radiance = g.lights[k].color.rgb / max(dot(d, d), 0.01);
        if i32(k) == i32(g.shadow.x) && raw > -0.2 {
            radiance *= shadow_at(i.world, normalize(i.normal), g.lights[k].pos.xyz, i.clip.xy);
        }
        let spec = ggx(max(dot(n, h), 0.0), a) * smith(n_v, max(raw, 1e-3), a) / (4.0 * n_v * max(raw, 1e-3) + 1e-4);
        let fl = fresnel(max(dot(h, v), 0.0), f0, 0.0);
        c += radiance * n_l * (diffuse / PI * 3.0 * (1.0 - fl) + fl * spec * select(1.0, 0.0, raw <= 0.0));
    }

    // Sheen: fibers glow at grazing angles.
    c += base * pow(1.0 - n_v, 3.0) * fuzz * 1.2;
    c *= fur_ao;

    if glass > 0.0 {
        // Thin glass: reflection on top of a tinted view of the refracted environment.
        let t = env_spec(refract(-v, n, 1.0 / 1.45), 0.02) * base;
        let fg = fresnel(n_v, vec3f(0.06), 0.0).x;
        let rgb = t * 0.35 * (1.0 - fg) + env_spec(reflect(-v, n), 0.02) * fg + (c - diffuse * irr) * 0.6;
        let ga = clamp(0.28 + fg + (1.0 - glass), 0.0, 1.0);
        return FsOut(vec4f(rgb * alpha, ga * alpha), 0xFFFFFFFFu); // premultiplied
    }
    c = mix(c, aces_inv(base), i.tex.z);
    if alpha < 0.004 {
        discard;
    }
    if i.tex.w > 0.5 {
        return FsOut(vec4f(c * alpha, alpha), 0xFFFFFFFFu); // premultiplied, blended
    }
    // Cut-outs: alpha becomes MSAA coverage (dithered), so edges stay soft
    // without sorting.
    let samples = u32(clamp(floor(alpha * 4.0 + hash3(vec3f(i.clip.xy, 3.0))), 0.0, 4.0));
    let bits = (1u << samples) - 1u;
    let r = u32(hash3(vec3f(i.clip.yx, 11.0)) * 4.0) & 3u;
    let mask = ((bits << r) | (bits >> (4u - r))) & 15u;
    return FsOut(vec4f(c, 1.0), mask);
}
