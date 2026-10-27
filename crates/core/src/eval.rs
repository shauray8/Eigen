//! Evaluation: turns the document into an evaluated scene at a frame.
//! Deterministic: the same document and frame always give the same result,
//! which is what lets the agent render frame N and trust what it sees.

use std::collections::HashMap;

use glam::{EulerRot, Mat4, Quat, Vec3};

use crate::data::{Align, Document, Fit, FxKind, Kind, Object, Order, Repeat};
use crate::paths;

#[derive(Clone, Debug)]
pub struct EvalMesh {
    pub kind: Kind,
    pub world: Mat4,
    pub color: [f32; 4],
    /// metal, rough, glass, fuzz
    pub material: [f32; 4],
    /// Texture set name ("" = none) and tiling.
    pub tex: String,
    pub uvscale: f32,
    /// Unlit mix and soft (blended) flag.
    pub emit: f32,
    pub soft: bool,
    /// Wind amplitude, fade-in heights (object space), and seconds.
    pub sway: [f32; 4],
    /// Seconds into `tex` when it is a video file.
    pub tex_time: f32,
    /// OBJ / glTF / GLB file for `Kind::Model` (project-relative); empty for primitives.
    /// `####` in it is replaced by the frame number (a mesh sequence).
    pub src: String,
}

/// An image or video frame placed on the canvas.
#[derive(Clone, Debug)]
pub struct EvalMedia {
    pub src: String,
    /// Seconds into a video file; None for still images.
    pub time: Option<f32>,
    pub center: [f32; 2],
    pub size: [f32; 2],
    pub rot_deg: f32,
    pub opacity: f32,
    pub fit: Fit,
}

#[derive(Clone, Debug)]
pub struct EvalFx {
    pub kind: FxKind,
    /// Shader plugin used instead of `kind` (empty: built-in).
    pub plugin: String,
    pub amount: f32,
    pub cell: f32,
    pub color: [f32; 4],
    pub params: [f32; 4],
    /// Seconds since the layer's in point.
    pub seconds: f32,
}

/// A layer drawn by a shader plugin inside its box.
#[derive(Clone, Debug)]
pub struct EvalShader {
    pub plugin: String,
    pub center: [f32; 2],
    pub size: [f32; 2],
    pub rot_deg: f32,
    pub opacity: f32,
    pub color: [f32; 4],
    pub params: [f32; 4],
    /// Seconds since the layer's in point, and the frame.
    pub time: f32,
    pub frame: f32,
}

/// A point cloud: `src` holds little-endian f32 `x y z keep` per point.
#[derive(Clone, Debug)]
pub struct EvalPoints {
    pub src: String,
    pub world: Mat4,
    pub color: [f32; 4],
    /// Dot diameter in scene pixels.
    pub size: f32,
    /// How much scene lights shade the dots.
    pub lit: f32,
}

#[derive(Clone, Debug)]
pub struct EvalLight {
    pub pos: Vec3,
    pub color: [f32; 3],
    pub energy: f32,
    /// Shadow softness (0 = casts no shadow).
    pub shadow: f32,
}

#[derive(Clone, Debug)]
pub struct EvalShape {
    pub kind: Kind,
    /// Center and size in scene pixels, y down.
    pub center: [f32; 2],
    pub size: [f32; 2],
    pub rot_deg: f32,
    pub color: [f32; 4],
    pub radius: f32,
    pub z: f32,
}

/// Per-letter animator result, one per non-whitespace character.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LetterXform {
    pub offset: [f32; 2],
    pub rot_deg: f32,
    pub scale: [f32; 2],
    pub alpha: f32,
}

#[derive(Clone, Debug)]
pub struct EvalText {
    pub body: String,
    pub font: String,
    pub weight: f32,
    pub size: f32,
    pub align: Align,
    pub tracking: f32,
    pub leading: f32,
    /// Anchor in scene pixels: horizontal per `align`, vertically centered.
    pub center: [f32; 2],
    pub rot_deg: f32,
    pub scale: [f32; 2],
    pub color: [f32; 4],
    pub letters: Vec<LetterXform>,
}

/// The 3D world seen through a camera, placed on the canvas.
#[derive(Clone, Debug)]
pub struct EvalView {
    pub center: [f32; 2],
    pub size: [f32; 2],
    pub rot_deg: f32,
    pub opacity: f32,
    pub camera: Option<EvalCamera>,
}

/// A composition, evaluated at its own frame, placed on the canvas like a picture.
#[derive(Clone, Debug)]
pub struct EvalComp {
    pub scene: Box<EvalScene>,
    pub center: [f32; 2],
    pub size: [f32; 2],
    pub rot_deg: f32,
    pub opacity: f32,
    pub fit: Fit,
}

#[derive(Clone, Debug)]
pub enum Layer2D {
    Shape(EvalShape),
    Text(EvalText),
    View(EvalView),
    /// Applies to everything composited beneath it.
    Fx(EvalFx),
    Media(EvalMedia),
    Comp(EvalComp),
    Shader(EvalShader),
}

#[derive(Clone, Debug)]
pub struct EvalCamera {
    pub world: Mat4,
    pub fov_deg: f32,
    pub focus: f32,
    pub aperture: f32,
    pub haze: f32,
    pub haze_color: [f32; 3],
}

#[derive(Clone, Debug)]
pub struct EvalScene {
    pub frame: f32,
    pub res: [u32; 2],
    pub bg: [f32; 4],
    pub ambient: f32,
    pub env: String,
    pub envrot: f32,
    pub bloom: f32,
    pub grain: f32,
    pub vignette: f32,
    /// The 3D world, shared by all views.
    pub meshes: Vec<EvalMesh>,
    pub points: Vec<EvalPoints>,
    pub lights: Vec<EvalLight>,
    /// The scene camera, for views without their own.
    pub camera: Option<EvalCamera>,
    /// Canvas layers, sorted back to front.
    pub layers: Vec<Layer2D>,
}

fn local(o: &Object) -> Mat4 {
    let r = o.rot * (std::f32::consts::PI / 180.0);
    Mat4::from_scale_rotation_translation(o.scale, Quat::from_euler(EulerRot::XYZ, r.x, r.y, r.z), o.loc)
}

/// Object `name` at `frame` (scene at `fps`): keys, then expressions, applied.
/// An expression that fails at some frame leaves that value as keyed.
pub fn animated(o: &Object, name: &str, frame: f32, fps: f32) -> Object {
    let mut e = o.clone();
    for (chan, curve) in &o.anim {
        if let Some(v) = curve.eval(frame) {
            paths::set_channel(&mut e, chan, v);
        }
    }
    for (path, src) in &o.expr {
        let _ = apply_expr(&mut e, name, path, src, frame, fps);
    }
    e
}

/// Runs expression `src` on `path` (`prop` or `prop.c`) of an already keyed object.
pub fn apply_expr(o: &mut Object, name: &str, path: &str, src: &str, frame: f32, fps: f32) -> Result<(), String> {
    let (prop, comp) = path.split_once('.').map_or((path, None), |(p, c)| (p, Some(c)));
    let current = paths::get_object(o, prop).and_then(|v| v.floats()).map_err(|e| e.to_string())?;
    let index = match comp {
        Some(c) => Some(paths::component_index(c).ok_or(format!("bad component `{c}`"))?),
        None => None,
    };
    let value = match index {
        Some(i) => vec![*current.get(i).ok_or("component out of range")?],
        None => current,
    };
    let seed = hash(name, path.len()) ^ hash(path, 7);
    let out = crate::expr::eval(src, frame / fps, frame, &value, seed)?;
    match index {
        Some(_) => paths::set_channel(o, path, out[0]),
        None => {
            let v = if out.len() == 1 { paths::Value::Num(out[0]) } else { paths::Value::Vec(out) };
            paths::set_object(o, prop, &v).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// On-canvas box of a 2D layer (already animated): (center, half size, rotation deg), in scene px.
pub fn layer_box(o: &Object) -> Option<([f32; 2], [f32; 2], f32)> {
    let s = o.scale;
    match o.kind {
        Kind::Rect | Kind::Ellipse | Kind::Image | Kind::Video | Kind::View | Kind::Clip | Kind::Shader => {
            Some(([o.loc.x, o.loc.y], [s.x.abs() / 2.0, s.y.abs() / 2.0], o.rot.z))
        }
        Kind::Text => {
            // Estimate from the text metrics (exact glyph bounds live in the renderer).
            let t = &o.text;
            let lines: Vec<&str> = t.body.split('\n').collect();
            let chars = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as f32;
            let w = (chars * (t.size * 0.56 + t.tracking)).max(t.size * 0.5) * s.x.abs();
            let h = ((lines.len() as f32 - 1.0) * t.size * t.leading + t.size * 0.75) * s.y.abs();
            let cx = match t.align {
                Align::Left => o.loc.x + w / 2.0,
                Align::Center => o.loc.x,
                Align::Right => o.loc.x - w / 2.0,
            };
            Some(([cx, o.loc.y], [w / 2.0, h / 2.0], o.rot.z))
        }
        _ => None,
    }
}

/// Corners of a layer box, clockwise from top-left, in scene px.
pub fn box_corners((c, h, rot): ([f32; 2], [f32; 2], f32)) -> [[f32; 2]; 4] {
    let (sn, cs) = rot.to_radians().sin_cos();
    [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].map(|(x, y)| {
        let (dx, dy) = (x * h[0], y * h[1]);
        [c[0] + dx * cs - dy * sn, c[1] + dx * sn + dy * cs]
    })
}

/// How deep clips of compositions may nest.
const MAX_NESTING: u32 = 8;

/// The film at `frame`.
pub fn evaluate(doc: &Document, frame: f32) -> EvalScene {
    evaluate_comp(doc, None, frame)
}

/// Composition `comp` (None: the film) at its own `frame`.
pub fn evaluate_comp(doc: &Document, comp: Option<&str>, frame: f32) -> EvalScene {
    eval_in(doc, comp, frame, 0)
}

fn eval_in(doc: &Document, comp: Option<&str>, frame: f32, depth: u32) -> EvalScene {
    // Only this composition's objects are animated: others cost nothing.
    let fps = doc.scene.fps;
    let objs: HashMap<&str, Object> =
        doc.objects.iter().filter(|(_, o)| o.comp.as_deref() == comp).map(|(n, o)| (n.as_str(), animated(o, n, frame, fps))).collect();
    let def = comp.and_then(|c| doc.objects.get(c)).filter(|o| o.kind == Kind::Comp);
    let scene_camera = match def {
        Some(d) => d.camera.as_deref(),
        None => doc.scene.camera.as_deref(),
    };

    // World matrices; 3D transforms only (2D layers are drawn in screen space).
    let mut world: HashMap<&str, Mat4> = HashMap::new();
    fn world_of<'a>(name: &'a str, objs: &HashMap<&'a str, Object>, cache: &mut HashMap<&'a str, Mat4>) -> Mat4 {
        if let Some(m) = cache.get(name) {
            return *m;
        }
        let o = &objs[name];
        let parent = o
            .parent
            .as_deref()
            .and_then(|p| objs.get_key_value(p))
            .map_or(Mat4::IDENTITY, |(p, _)| world_of(p, objs, cache));
        let m = parent * local(o);
        cache.insert(name, m);
        m
    }

    let mut scene = EvalScene {
        frame,
        res: doc.comp_canvas(comp).0,
        bg: def.map_or(doc.scene.bg, |d| d.color),
        ambient: doc.scene.ambient,
        env: doc.scene.env.clone(),
        envrot: doc.scene.envrot,
        bloom: doc.scene.bloom,
        grain: doc.scene.grain,
        vignette: doc.scene.vignette,
        camera: None,
        meshes: Vec::new(),
        points: Vec::new(),
        lights: Vec::new(),
        layers: Vec::new(),
    };
    let mut layers: Vec<(f32, Layer2D)> = Vec::new();

    for name in doc.objects.keys() {
        let Some(o) = objs.get(name.as_str()) else { continue };
        if !shown(name, &objs, frame) || o.opacity <= 0.0 {
            continue;
        }
        match o.kind {
            Kind::Cube | Kind::Sphere | Kind::Plane | Kind::Cylinder | Kind::Model => {
                let parent = o
                    .parent
                    .as_deref()
                    .filter(|p| objs.contains_key(p))
                    .map_or(Mat4::IDENTITY, |p| world_of(p, &objs, &mut world));
                let m = &o.material;
                for (i, off, extra, shade) in copies(name, &o.repeat) {
                    let c = delayed(doc, name, o, i, frame);
                    let w = parent
                        * Mat4::from_rotation_translation(euler(c.rot), c.loc)
                        * Mat4::from_translation(off)
                        * Mat4::from_quat(euler(extra))
                        * Mat4::from_scale(c.scale);
                    scene.meshes.push(EvalMesh {
                        kind: o.kind,
                        world: w,
                        color: shaded(c.color, shade),
                        material: [m.metal, m.rough, m.glass, m.fuzz],
                        tex: m.tex.clone(),
                        uvscale: m.uvscale,
                        emit: m.emit,
                        soft: m.soft > 0.0,
                        sway: [m.sway.x, m.sway.y, m.sway.z, frame / doc.scene.fps],
                        tex_time: (frame - o.t_in.unwrap_or(0.0)) / doc.scene.fps + o.media.trim,
                        src: o.media.src.replace("####", &format!("{:04}", frame.floor().max(0.0) as i64)),
                    });
                }
            }
            Kind::Points if !o.media.src.is_empty() => scene.points.push(EvalPoints {
                src: o.media.src.clone(),
                world: world_of(name, &objs, &mut world),
                color: with_opacity(o.color, o.opacity),
                size: o.fx.cell,
                lit: o.lit,
            }),
            Kind::Light => scene.lights.push(EvalLight {
                pos: world_of(name, &objs, &mut world).transform_point3(Vec3::ZERO),
                color: [o.color[0], o.color[1], o.color[2]],
                energy: o.energy,
                shadow: o.shadow,
            }),
            Kind::Rect | Kind::Ellipse => {
                for (i, off, extra, shade) in copies(name, &o.repeat) {
                    let c = delayed(doc, name, o, i, frame);
                    let (sn, cs) = (c.rot.z * std::f32::consts::PI / 180.0).sin_cos();
                    let center = [c.loc.x + off.x * cs - off.y * sn, c.loc.y + off.x * sn + off.y * cs];
                    layers.push((
                        c.z,
                        Layer2D::Shape(EvalShape {
                            kind: o.kind,
                            center,
                            size: [c.scale.x, c.scale.y],
                            rot_deg: c.rot.z + extra.z,
                            color: with_opacity(shaded(c.color, shade), c.opacity),
                            radius: c.radius,
                            z: c.z,
                        }),
                    ));
                }
            }
            Kind::Fx => layers.push((
                o.z,
                Layer2D::Fx(EvalFx {
                    kind: o.fx.kind,
                    plugin: o.fx.plugin.clone(),
                    amount: o.fx.amount,
                    cell: o.fx.cell,
                    color: o.color,
                    params: o.fx.params,
                    seconds: (frame - o.t_in.unwrap_or(0.0)) / doc.scene.fps,
                }),
            )),
            Kind::View => {
                let cam = o.camera.as_deref().or(scene_camera).filter(|c| objs.contains_key(c));
                let camera = cam.map(|c| camera_of(&objs[c], world_of(c, &objs, &mut world)));
                layers.push((
                    o.z,
                    Layer2D::View(EvalView {
                        center: [o.loc.x, o.loc.y],
                        size: [o.scale.x, o.scale.y],
                        rot_deg: o.rot.z,
                        opacity: o.opacity * o.color[3],
                        camera,
                    }),
                ))
            }
            Kind::Text => layers.push((o.z, Layer2D::Text(eval_text(name, &doc.objects[name], o, frame)))),
            Kind::Image | Kind::Video if !o.media.src.is_empty() => {
                let time = (o.kind == Kind::Video)
                    .then(|| (frame - o.t_in.unwrap_or(0.0)) / doc.scene.fps + o.media.trim);
                layers.push((
                    o.z,
                    Layer2D::Media(EvalMedia {
                        src: o.media.src.clone(),
                        time,
                        center: [o.loc.x, o.loc.y],
                        size: [o.scale.x, o.scale.y],
                        rot_deg: o.rot.z,
                        opacity: o.opacity * o.color[3],
                        fit: o.media.fit,
                    }),
                ))
            }
            Kind::Clip if depth < MAX_NESTING && !o.media.src.is_empty() => {
                let src = o.media.src.as_str();
                let len = doc.comp_canvas(Some(src)).1;
                let local = frame - o.t_in.unwrap_or(0.0) + o.media.trim * doc.scene.fps;
                if doc.objects.get(src).is_some_and(|c| c.kind == Kind::Comp) && (0.0..len).contains(&local) {
                    layers.push((
                        o.z,
                        Layer2D::Comp(EvalComp {
                            scene: Box::new(eval_in(doc, Some(src), local, depth + 1)),
                            center: [o.loc.x, o.loc.y],
                            size: [o.scale.x, o.scale.y],
                            rot_deg: o.rot.z,
                            opacity: o.opacity * o.color[3],
                            fit: o.media.fit,
                        }),
                    ))
                }
            }
            Kind::Shader if !o.media.src.is_empty() => layers.push((
                o.z,
                Layer2D::Shader(EvalShader {
                    plugin: o.media.src.clone(),
                    center: [o.loc.x, o.loc.y],
                    size: [o.scale.x, o.scale.y],
                    rot_deg: o.rot.z,
                    opacity: o.opacity,
                    color: o.color,
                    params: o.fx.params,
                    time: (frame - o.t_in.unwrap_or(0.0)) / doc.scene.fps,
                    frame,
                }),
            )),
            Kind::Camera | Kind::Empty | Kind::Points | Kind::Audio | Kind::Image | Kind::Video | Kind::Comp | Kind::Clip | Kind::Shader => {}
        }
    }
    if let Some(cam) = scene_camera.filter(|c| objs.contains_key(c)) {
        scene.camera = Some(camera_of(&objs[cam], world_of(cam, &objs, &mut world)));
    }
    layers.sort_by(|a, b| a.0.total_cmp(&b.0));
    scene.layers = layers.into_iter().map(|(_, l)| l).collect();
    scene
}

fn camera_of(c: &Object, world: Mat4) -> EvalCamera {
    EvalCamera {
        world,
        fov_deg: c.fov,
        focus: c.focus,
        aperture: c.aperture,
        haze: c.haze,
        haze_color: [c.color[0], c.color[1], c.color[2]],
    }
}

fn euler(deg: Vec3) -> Quat {
    let r = deg * (std::f32::consts::PI / 180.0);
    Quat::from_euler(EulerRot::XYZ, r.x, r.y, r.z)
}

/// Copies of a live array: (index, offset, extra rotation deg, brightness shift).
/// The grid is centered on the object; copy order is x, then y, then z.
fn copies(name: &str, r: &Repeat) -> Vec<(usize, Vec3, Vec3, f32)> {
    let n = r.count.max(Vec3::ONE);
    let (nx, ny, nz) = (n.x as usize, n.y as usize, n.z as usize);
    let mut out = Vec::with_capacity(nx * ny * nz);
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                let idx = out.len();
                let grid = Vec3::new(i as f32, j as f32, k as f32) - (n - Vec3::ONE) / 2.0;
                let rand = Vec3::new(unit_random(name, idx * 4 + 100), unit_random(name, idx * 4 + 101), unit_random(name, idx * 4 + 102));
                let extra = r.rot * idx as f32 + r.jitter * rand;
                out.push((idx, grid * r.spacing, extra, r.shade * unit_random(name, idx * 4 + 103)));
            }
        }
    }
    out
}

/// Copy `i` of an array, with its animation delayed by `rdelay * i` frames.
fn delayed(doc: &Document, name: &str, o: &Object, i: usize, frame: f32) -> Object {
    if i == 0 || o.repeat.delay == 0.0 {
        o.clone()
    } else {
        animated(&doc.objects[name], name, frame - o.repeat.delay * i as f32, doc.scene.fps)
    }
}

fn shaded(c: [f32; 4], shade: f32) -> [f32; 4] {
    let k = 1.0 + shade;
    [c[0] * k, c[1] * k, c[2] * k, c[3]]
}

/// Visible and alive, along with every ancestor (a hidden group hides its children).
fn shown(name: &str, objs: &HashMap<&str, Object>, frame: f32) -> bool {
    let mut cur = Some(name);
    let mut depth = 0;
    while let Some(n) = cur {
        let Some(o) = objs.get(n) else { return true };
        if !o.visible || !o.alive(frame) || depth > 64 {
            return false;
        }
        cur = o.parent.as_deref();
        depth += 1;
    }
    true
}

fn with_opacity(c: [f32; 4], opacity: f32) -> [f32; 4] {
    [c[0], c[1], c[2], c[3] * opacity]
}

fn hash(seed: &str, i: usize) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in seed.bytes().chain((i as u64).to_le_bytes()) {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    h ^ (h >> 29)
}

/// Deterministic value in -1..1 per (object, index).
fn unit_random(seed: &str, i: usize) -> f32 {
    (hash(seed, i) % 20001) as f32 / 10000.0 - 1.0
}

/// Start rank of each letter for a stagger order.
fn letter_ranks(order: Order, n: usize, seed: &str) -> Vec<f32> {
    let mid = (n as f32 - 1.0) / 2.0;
    match order {
        Order::Forward => (0..n).map(|i| i as f32).collect(),
        Order::Reverse => (0..n).map(|i| (n - 1 - i) as f32).collect(),
        Order::Center => (0..n).map(|i| (i as f32 - mid).abs()).collect(),
        Order::Edges => (0..n).map(|i| mid - (i as f32 - mid).abs()).collect(),
        Order::Random => {
            // Deterministic shuffle seeded by the object name.
            let mut idx: Vec<usize> = (0..n).collect();
            idx.sort_by_key(|i| hash(seed, *i));
            let mut ranks = vec![0.0; n];
            for (rank, i) in idx.into_iter().enumerate() {
                ranks[i] = rank as f32;
            }
            ranks
        }
    }
}

/// `orig` holds the curves; `o` is already animated at `frame` (for the
/// layer's own transform). Letter channels are read at staggered frames.
fn eval_text(name: &str, orig: &Object, o: &Object, frame: f32) -> EvalText {
    let t = &o.text;
    let n = t.body.chars().filter(|c| !c.is_whitespace()).count();
    let read = |chan: &str, f: f32, fallback: f32| orig.anim.get(chan).and_then(|c| c.eval(f)).unwrap_or(fallback);
    let ot = &orig.text;
    let letters = letter_ranks(t.order, n, name)
        .into_iter()
        .enumerate()
        .map(|(i, rank)| {
            let f = frame - rank * t.stagger;
            let [rx, ry, rr] = [0, 1, 2].map(|k| unit_random(name, i * 3 + k));
            let spread = [read("gspread.x", f, ot.gspread.x), read("gspread.y", f, ot.gspread.y)];
            LetterXform {
                offset: [
                    read("gloc.x", f, ot.gloc.x) + rx * spread[0],
                    read("gloc.y", f, ot.gloc.y) + ry * spread[1],
                ],
                rot_deg: read("grot", f, ot.grot) + rr * read("gspin", f, ot.gspin),
                scale: [read("gscale.x", f, ot.gscale.x), read("gscale.y", f, ot.gscale.y)],
                alpha: read("galpha", f, ot.galpha),
            }
        })
        .collect();
    EvalText {
        body: t.body.clone(),
        font: t.font.clone(),
        weight: t.weight,
        size: t.size,
        align: t.align,
        tracking: t.tracking,
        leading: t.leading,
        center: [o.loc.x, o.loc.y],
        rot_deg: o.rot.z,
        scale: [o.scale.x, o.scale.y],
        color: with_opacity(o.color, o.opacity),
        letters,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Session, Source};

    #[test]
    fn stagger_delays_each_letter() {
        let mut s = Session::new();
        s.run("add text t text=\"AB C\" stagger=4\nkey t galpha=0 @0 linear ; key t galpha=1 @8", Source::Agent)
            .unwrap();
        let scene = evaluate(s.doc(), 4.0);
        let Layer2D::Text(t) = &scene.layers[0] else { panic!() };
        let alphas: Vec<f32> = t.letters.iter().map(|l| l.alpha).collect();
        assert_eq!(alphas, vec![0.5, 0.0, 0.0]);
        assert_eq!(letter_ranks(Order::Center, 5, "x"), vec![2.0, 1.0, 0.0, 1.0, 2.0]);
    }

    #[test]
    fn views_and_cuts() {
        let mut s = Session::new();
        s.run(
            "add camera cam loc=0,0,5\nadd view shot1 camera=cam out=24 z=1\nadd rect card in=24 z=0\nadd cube box",
            Source::Agent,
        )
        .unwrap();
        let a = evaluate(s.doc(), 0.0);
        assert!(matches!(&a.layers[..], [Layer2D::View(v)] if v.size == [1920.0, 1080.0] && v.camera.is_some()));
        let b = evaluate(s.doc(), 24.0);
        assert!(matches!(&b.layers[..], [Layer2D::Shape(r)] if r.center == [960.0, 540.0]));
        assert_eq!(b.meshes.len(), 1);
        s.run("add empty grp out=24 ; set box.parent grp", Source::Agent).unwrap();
        assert_eq!(evaluate(s.doc(), 24.0).meshes.len(), 0);
        s.run("add sphere dots rep=4,3 spacing=2,1 ; add fx glitch fx=pixelate cell=20 z=5", Source::Agent).unwrap();
        let c = evaluate(s.doc(), 0.0);
        assert_eq!(c.meshes.iter().filter(|m| m.kind == Kind::Sphere).count(), 12);
        let xs: Vec<f32> = c.meshes.iter().filter(|m| m.kind == Kind::Sphere).take(4).map(|m| m.world.w_axis.x).collect();
        assert_eq!(xs, vec![-3.0, -1.0, 1.0, 3.0]);
        assert!(matches!(c.layers.last(), Some(Layer2D::Fx(f)) if f.kind == FxKind::Pixelate));
        // Views depend on cameras, so the dump must add the camera first.
        let text = crate::lang::dump(s.doc());
        assert!(text.find("add camera cam").unwrap() < text.find("add view shot1").unwrap(), "{text}");
    }

    #[test]
    fn compositions_play_through_clips() {
        let mut s = Session::new();
        s.run(
            "add comp title scale=800,200 out=24\nin title\nadd rect bar in=12\nin main\nadd rect bg\nadd clip t1 src=title in=100 trim=0.25",
            Source::Agent,
        )
        .unwrap();
        let doc = s.doc();
        assert_eq!(doc.objects["bar"].comp.as_deref(), Some("title"), "`in` puts adds in the comp");
        assert_eq!(doc.objects["bg"].comp, None, "`in main` returns to the film");
        assert_eq!(doc.objects["t1"].t_out, None);

        fn comp_at(doc: &Document, f: f32) -> Option<EvalComp> {
            evaluate(doc, f).layers.into_iter().find_map(|l| match l {
                Layer2D::Comp(c) => Some(c),
                _ => None,
            })
        }
        assert!(comp_at(s.doc(), 99.0).is_none());
        let early = comp_at(s.doc(), 100.0).unwrap();
        assert_eq!((early.scene.frame, early.scene.res), (6.0, [800, 200]));
        assert!(early.scene.layers.is_empty(), "bar starts at the comp's frame 12");
        assert_eq!(comp_at(s.doc(), 106.0).unwrap().scene.layers.len(), 1);
        // trim 0.25s skips the comp's first 6 frames (24fps), leaving 18.
        assert!(comp_at(s.doc(), 117.0).is_some());
        assert!(comp_at(s.doc(), 118.0).is_none(), "past the comp's length the clip shows nothing");
        assert_eq!(evaluate(s.doc(), 106.0).layers.len(), 2, "the film draws bg and the clip, not the comp's bar");

        // Saved and loaded, the composition comes back whole, with the camera
        // that lives inside it.
        s.run("add camera cam comp=title ; set title.camera cam", Source::Agent).unwrap();
        let text = crate::lang::dump(s.doc());
        assert!(text.find("add comp title").unwrap() < text.find("add rect bar").unwrap(), "{text}");
        let mut back = Session::new();
        back.run(&text, Source::Agent).unwrap();
        assert_eq!(crate::lang::dump(back.doc()), text);
    }

    #[test]
    fn compositions_cannot_contain_themselves() {
        let mut s = Session::new();
        s.run("add comp a ; add comp b ; add clip ab src=b comp=a", Source::Agent).unwrap();
        assert!(s.run("add clip aa src=a comp=a", Source::Agent).is_err(), "a in a");
        assert!(s.run("add clip ba src=a comp=b", Source::Agent).is_err(), "a in b in a");
        assert!(s.run("add clip free src=a ; set free.comp b", Source::Agent).is_err(), "moving a clip into a loop");
        assert!(s.run("set a.comp b", Source::Agent).is_err(), "comps do not nest as members");

        s.run("add rect r comp=b ; add clip top src=b", Source::Agent).unwrap();
        s.run("del b", Source::Agent).unwrap();
        assert!(!s.doc().objects.contains_key("r"), "deleting a comp deletes its contents");
        assert_eq!(s.doc().objects["top"].media.src, "", "clips of a deleted comp go blank");
    }

    #[test]
    fn expressions_drive_properties_after_keys() {
        let mut s = Session::new();
        s.run("add rect r\nkey r.loc.x 100 @0 linear ; key r.loc.x 200 @24\nexpr r.loc.x \"value + 10\"\nexpr r.loc \"[value[0], 50 * t, 0]\"", Source::Agent)
            .unwrap();
        let at = |s: &Session, f: f32| animated(&s.doc().objects["r"], "r", f, 24.0).loc;
        assert_eq!(at(&s, 12.0), glam::Vec3::new(160.0, 25.0, 0.0), "keys, then each expression in path order");
        assert!(s.run("expr r.opacity \"value +\"", Source::Agent).is_err(), "a broken script is refused when written");
        assert!(s.run("expr r.text \"1\"", Source::Agent).is_err(), "only numeric properties take expressions");

        let text = crate::lang::dump(s.doc());
        let mut back = Session::new();
        back.run(&text, Source::Agent).unwrap();
        assert_eq!(at(&back, 12.0), at(&s, 12.0), "expressions survive save and load:\n{text}");
        s.run("unexpr r.loc ; unexpr r.loc.x", Source::Agent).unwrap();
        assert_eq!(at(&s, 12.0).x, 150.0);
    }
}
