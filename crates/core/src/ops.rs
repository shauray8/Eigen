//! Operators: the only way a document changes. The UI and the agent both
//! produce these; there is no second, privileged path.

use anyhow::{Result, bail};

use crate::anim::{Ease, Key};
use crate::data::{Document, Kind, Object};
use crate::paths::{self, Path, Value};

/// A frame as written: absolute, or a beat of the scene's music (`@b16`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Frame {
    At(f32),
    Beat(f32),
}

impl Frame {
    pub fn parse(s: &str) -> Option<Frame> {
        match s.strip_prefix('b') {
            Some(b) => b.parse().ok().map(Frame::Beat),
            None => s.parse().ok().map(Frame::At),
        }
    }

    pub fn resolve(self, doc: &Document) -> Result<f32> {
        match self {
            Frame::At(f) => Ok(f),
            Frame::Beat(n) => doc.beat_frame(n).map_err(|e| anyhow::anyhow!(e)),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Add { kind: Kind, name: String, props: Vec<(String, Value)> },
    Set { path: Path, value: Value },
    Key { path: Path, value: Value, frame: Frame, ease: Ease },
    Unkey { path: Path, frame: Option<Frame> },
    Del { name: String },
    /// Copy an object (with its animation) under a new name, then set props.
    Dup { src: String, name: String, props: Vec<(String, Value)> },
    Ren { from: String, to: String },
    /// Set (Some) or remove (None) the expression on a property or component.
    Expr { path: Path, src: Option<String> },
}

fn check_name(doc: &Document, name: &str) -> Result<()> {
    if name == "scene" || name.is_empty() {
        bail!("`{name}` is reserved");
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        bail!("name `{name}` may only use letters, digits, _ and -");
    }
    if doc.objects.contains_key(name) {
        bail!("`{name}` already exists");
    }
    Ok(())
}

fn check_parent(doc: &Document, child: &str, parent: &Option<String>) -> Result<()> {
    let mut cur = parent.clone();
    while let Some(p) = cur {
        if p == child {
            bail!("parenting `{child}` would create a cycle");
        }
        cur = paths::object(doc, &p)?.parent.clone();
    }
    Ok(())
}

pub fn apply(doc: &mut Document, op: &Op) -> Result<()> {
    match op {
        Op::Add { kind, name, props } => {
            check_name(doc, name)?;
            let mut o = Object::new(*kind);
            // Canvas layers start centered on their canvas (the film's or
            // their comp's); views and pictures start full-frame.
            if kind.is_2d() {
                let comp = props.iter().find_map(|(p, v)| match (p.as_str(), v) {
                    ("comp", Value::Str(c)) => Some(c.as_str()),
                    _ => None,
                });
                let [w, h] = doc.comp_canvas(comp).0.map(|v| v as f32);
                o.loc = glam::Vec3::new(w / 2.0, h / 2.0, 0.0);
                if matches!(kind, Kind::View | Kind::Image | Kind::Video | Kind::Clip | Kind::Shader) {
                    o.scale = glam::Vec3::new(w, h, 1.0);
                }
            }
            doc.objects.insert(name.clone(), o);
            for (prop, value) in props {
                let path = Path { owner: name.clone(), prop: prop.clone(), index: None };
                set(doc, &path, value)?;
            }
        }
        Op::Set { path, value } => set(doc, path, value)?,
        Op::Key { path, value, frame, ease } => {
            let frame = &frame.resolve(doc)?;
            if path.is_scene() {
                bail!("scene properties are not animatable yet");
            }
            let o = doc.objects.get_mut(&path.owner).ok_or_else(|| paths::no_object(&path.owner))?;
            let targets: Vec<(String, f32)> = match path.index {
                Some(_) => vec![(path.channel(), single(value)?)],
                None => {
                    let chans = paths::channels(o, &path.prop)?;
                    let mut vals = value.floats()?;
                    if vals.len() == 1 {
                        vals = vec![vals[0]; chans.len()];
                    } else if vals.len() < chans.len() {
                        // Missing trailing components keep their current value.
                        let cur = paths::get_object(o, &path.prop)?.floats()?;
                        vals.extend_from_slice(&cur[vals.len()..]);
                    }
                    if vals.len() != chans.len() {
                        bail!("`{}` needs {} values", path.prop, chans.len());
                    }
                    chans.into_iter().zip(vals).collect()
                }
            };
            for (chan, v) in targets {
                o.anim.entry(chan).or_default().insert(Key { frame: *frame, value: v, ease: *ease });
            }
        }
        Op::Unkey { path, frame } => {
            let frame = match frame {
                Some(f) => Some(f.resolve(doc)?),
                None => None,
            };
            let o = doc.objects.get_mut(&path.owner).ok_or_else(|| paths::no_object(&path.owner))?;
            let prefix = path.channel();
            let hit = |c: &str| c == prefix || c.strip_prefix(&prefix).is_some_and(|r| r.starts_with('.'));
            o.anim.retain(|chan, curve| {
                if !hit(chan) {
                    return true;
                }
                match frame {
                    Some(f) => {
                        curve.remove(f);
                        !curve.keys.is_empty()
                    }
                    None => false,
                }
            });
        }
        Op::Dup { src, name, props } => {
            check_name(doc, name)?;
            let o = paths::object(doc, src)?.clone();
            doc.objects.insert(name.clone(), o);
            for (prop, value) in props {
                set(doc, &Path { owner: name.clone(), prop: prop.clone(), index: None }, value)?;
            }
        }
        Op::Del { name } => {
            let Some(gone) = doc.objects.shift_remove(name) else { return Err(paths::no_object(name)) };
            // A composition takes its contents with it; clips of it go blank.
            if gone.kind == Kind::Comp {
                doc.objects.retain(|_, o| o.comp.as_deref() != Some(name));
                for o in doc.objects.values_mut().filter(|o| o.kind == Kind::Clip && o.media.src == *name) {
                    o.media.src.clear();
                }
            }
            for o in doc.objects.values_mut() {
                if o.parent.as_deref() == Some(name) {
                    o.parent = None;
                }
                if o.camera.as_deref() == Some(name) {
                    o.camera = None;
                }
            }
            if doc.scene.camera.as_deref() == Some(name) {
                doc.scene.camera = None;
            }
            if doc.scene.music.as_deref() == Some(name) {
                doc.scene.music = None;
            }
        }
        Op::Ren { from, to } => {
            check_name(doc, to)?;
            let Some(i) = doc.objects.get_index_of(from) else { return Err(paths::no_object(from)) };
            let (_, o) = doc.objects.shift_remove_index(i).unwrap();
            doc.objects.shift_insert(i, to.clone(), o);
            for o in doc.objects.values_mut() {
                for r in [&mut o.parent, &mut o.camera, &mut o.comp] {
                    if r.as_deref() == Some(from) {
                        *r = Some(to.clone());
                    }
                }
                if o.kind == Kind::Clip && o.media.src == *from {
                    o.media.src = to.clone();
                }
            }
            if doc.scene.camera.as_deref() == Some(from) {
                doc.scene.camera = Some(to.clone());
            }
            if doc.scene.music.as_deref() == Some(from) {
                doc.scene.music = Some(to.clone());
            }
        }
        Op::Expr { path, src } => {
            if path.is_scene() {
                bail!("scene properties take no expressions");
            }
            let fps = doc.scene.fps;
            let o = doc.objects.get_mut(&path.owner).ok_or_else(|| paths::no_object(&path.owner))?;
            let key = path.channel();
            match src {
                None => {
                    if o.expr.remove(&key).is_none() {
                        bail!("`{}` has no expression on {key}", path.owner);
                    }
                }
                Some(src) => {
                    // Run it once now so a mistake is reported here, not as a silent no-op later.
                    let mut probe = crate::eval::animated(o, &path.owner, 0.0, fps);
                    crate::eval::apply_expr(&mut probe, &path.owner, &key, src, 0.0, fps).map_err(|e| anyhow::anyhow!("expression on {key}: {e}"))?;
                    o.expr.insert(key, src.clone());
                }
            }
        }
    }
    Ok(())
}

fn single(v: &Value) -> Result<f32> {
    match v.floats()?.as_slice() {
        [x] => Ok(*x),
        _ => bail!("a component key takes one number"),
    }
}

fn set(doc: &mut Document, path: &Path, value: &Value) -> Result<()> {
    // `in=b16`: frames given as beats of the music.
    if matches!(path.prop.as_str(), "in" | "out") {
        if let Value::Str(s) = value {
            let f = Frame::parse(s).ok_or_else(|| anyhow::anyhow!("bad frame `{s}`"))?.resolve(doc)?;
            return paths::set(doc, path, &Value::Num(f));
        }
    }
    if path.is_scene() && path.prop == "music" {
        if let Value::Str(m) = value {
            if paths::object(doc, m)?.kind != Kind::Audio {
                bail!("`{m}` is not an audio object");
            }
        }
    }
    if !path.is_scene() && path.prop == "parent" {
        let parent = match value {
            Value::Str(p) => {
                paths::object(doc, p)?;
                Some(p.clone())
            }
            _ => None,
        };
        check_parent(doc, &path.owner, &parent)?;
    }
    if path.prop == "camera" {
        if let Value::Str(c) = value {
            if paths::object(doc, c)?.kind != Kind::Camera {
                bail!("`{c}` is not a camera");
            }
        }
    }
    if !path.is_scene() && matches!(path.prop.as_str(), "comp" | "src") {
        check_comp_link(doc, &path.owner, &path.prop, value)?;
    }
    paths::set(doc, path, value)
}

/// Membership (`comp=`) must name a composition, and a clip (`src=`) must
/// show one without ever showing itself.
fn check_comp_link(doc: &Document, owner: &str, prop: &str, value: &Value) -> Result<()> {
    let o = paths::object(doc, owner)?;
    let (home, shows) = match prop {
        "comp" => {
            let Value::Str(c) = value else { return Ok(()) };
            if o.kind == Kind::Comp {
                bail!("a composition cannot live inside another (place a clip of it instead)");
            }
            if paths::object(doc, c)?.kind != Kind::Comp {
                bail!("`{c}` is not a comp");
            }
            (Some(c.as_str()), (o.kind == Kind::Clip).then_some(o.media.src.as_str()))
        }
        _ if o.kind == Kind::Clip => {
            let Value::Str(s) = value else { bail!("a clip's src is a comp name") };
            if paths::object(doc, s)?.kind != Kind::Comp {
                bail!("`{s}` is not a comp");
            }
            (o.comp.as_deref(), Some(s.as_str()))
        }
        _ => return Ok(()),
    };
    if let (Some(home), Some(shows)) = (home, shows.filter(|s| !s.is_empty()))
        && doc.comp_shows(shows, home)
    {
        bail!("a clip of `{shows}` inside `{home}` would show `{home}` inside itself");
    }
    Ok(())
}
