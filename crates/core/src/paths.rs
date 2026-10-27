//! RNA: reflection over DNA. Every property has a data path, e.g. `cube1.loc.z`,
//! `scene.fps`. Operators, animation and queries all go through here, so a new
//! property only has to be added in one place.

use anyhow::{Result, anyhow, bail};
use glam::Vec3;

use crate::data::{Align, Document, Fit, FxKind, Object, Order, Scene};

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Num(f32),
    Vec(Vec<f32>),
    Str(String),
    Bool(bool),
    None,
}

impl Value {
    pub fn floats(&self) -> Result<Vec<f32>> {
        match self {
            Value::Num(n) => Ok(vec![*n]),
            Value::Vec(v) => Ok(v.clone()),
            Value::Bool(b) => Ok(vec![if *b { 1.0 } else { 0.0 }]),
            _ => bail!("expected number(s), got {self}"),
        }
    }

    fn num(&self) -> Result<f32> {
        match self.floats()?.as_slice() {
            [n] => Ok(*n),
            _ => bail!("expected one number, got {self}"),
        }
    }

    fn vec3(&self) -> Result<Vec3> {
        match self.floats()?.as_slice() {
            [n] => Ok(Vec3::splat(*n)),
            [x, y] => Ok(Vec3::new(*x, *y, 0.0)),
            [x, y, z] => Ok(Vec3::new(*x, *y, *z)),
            _ => bail!("expected 1-3 numbers, got {self}"),
        }
    }

    fn color(&self) -> Result<[f32; 4]> {
        match self.floats()?.as_slice() {
            [v] => Ok([*v, *v, *v, 1.0]),
            [r, g, b] => Ok([*r, *g, *b, 1.0]),
            [r, g, b, a] => Ok([*r, *g, *b, *a]),
            _ => bail!("expected color, got {self}"),
        }
    }

    fn bool(&self) -> Result<bool> {
        match self {
            Value::Bool(b) => Ok(*b),
            Value::Num(n) => Ok(*n != 0.0),
            _ => bail!("expected bool, got {self}"),
        }
    }

    fn opt_name(&self) -> Result<Option<String>> {
        match self {
            Value::None => Ok(None),
            Value::Str(s) => Ok(Some(s.clone())),
            _ => bail!("expected a name or none, got {self}"),
        }
    }
}

pub fn fmt_num(n: f32) -> String {
    let s = format!("{n:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// `#rgb`, `#rrggbb`, `#rrggbbaa` in sRGB -> linear RGBA.
pub fn parse_hex(s: &str) -> Option<[f32; 4]> {
    let h = s.strip_prefix('#')?;
    let expand: String = match h.len() {
        3 => h.chars().flat_map(|c| [c, c]).collect(),
        6 | 8 => h.into(),
        _ => return None,
    };
    let byte = |i: usize| u8::from_str_radix(&expand[i..i + 2], 16).ok();
    let a = if expand.len() == 8 { byte(6)? as f32 / 255.0 } else { 1.0 };
    Some([
        srgb_to_linear(byte(0)? as f32 / 255.0),
        srgb_to_linear(byte(2)? as f32 / 255.0),
        srgb_to_linear(byte(4)? as f32 / 255.0),
        a,
    ])
}

/// Hex form of a linear color, only if it parses back to the same values.
fn to_hex(c: [f32; 4]) -> Option<String> {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let s = format!(
        "#{:02x}{:02x}{:02x}",
        q(linear_to_srgb(c[0])),
        q(linear_to_srgb(c[1])),
        q(linear_to_srgb(c[2]))
    );
    let s = if c[3] < 1.0 { format!("{s}{:02x}", q(c[3])) } else { s };
    let back = parse_hex(&s)?;
    back.iter().zip(c).all(|(a, b)| (a - b).abs() < 1e-5).then_some(s)
}

/// Whether a string must be quoted to parse back as the same string.
fn needs_quotes(s: &str) -> bool {
    s.is_empty()
        || matches!(s, "none" | "true" | "false")
        || s.starts_with(|c: char| c.is_ascii_digit() || matches!(c, '-' | '.' | '#' | '@'))
        || s.contains(|c: char| c.is_whitespace() || matches!(c, '"' | ';' | '=' | ',' | '\\'))
        || s.contains("//")
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Num(n) => f.write_str(&fmt_num(*n)),
            Value::Vec(v) => {
                if let [r, g, b, a] = v.as_slice() {
                    if let Some(hex) = to_hex([*r, *g, *b, *a]) {
                        return f.write_str(&hex);
                    }
                }
                // Uniform vec3s read back as one number (`scale=2`), as they are written.
                if let [x, y, z] = v.as_slice() {
                    if x == y && y == z {
                        return f.write_str(&fmt_num(*x));
                    }
                }
                let parts: Vec<String> = v.iter().map(|n| fmt_num(*n)).collect();
                f.write_str(&parts.join(","))
            }
            // Not `{s:?}`: Debug escapes combining marks (Devanagari matras) as \u{..}.
            Value::Str(s) if needs_quotes(s) => {
                let esc = s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\t', "\\t");
                write!(f, "\"{esc}\"")
            }
            Value::Str(s) => f.write_str(s),
            Value::Bool(b) => write!(f, "{b}"),
            Value::None => f.write_str("none"),
        }
    }
}

/// Properties of objects, in display order. `kind` is read-only.
pub const OBJECT_PROPS: [&str; 60] = [
    "text", "font", "weight", "size", "align", "tracking", "leading", "camera", "fx", "src", "fit", "comp", "parent", "in", "out",
    "loc", "rot", "scale", "color", "opacity", "visible", "fov", "focus", "aperture", "haze", "energy", "shadow", "radius", "z", "stagger", "order", "gloc",
    "grot", "gscale", "galpha", "gspread", "gspin", "metal", "rough", "glass", "fuzz", "emit", "soft", "sway", "tex", "uvscale", "rep", "spacing", "rrot",
    "rjitter", "rshade", "rdelay", "amount", "cell", "params", "lit", "trim", "volume", "bpm", "beat0"
];

/// The properties that mean something for a kind, in display order.
pub fn props_for(kind: crate::data::Kind) -> Vec<&'static str> {
    use crate::data::Kind::*;
    match kind {
        Audio => return vec!["src", "comp", "in", "out", "trim", "volume", "bpm", "beat0"],
        Fx => return vec!["fx", "amount", "cell", "params", "color", "comp", "in", "out", "visible", "z"],
        // `scale` is the canvas size, `out` the length, `color` the background.
        Comp => return vec!["scale", "out", "color", "camera"],
        _ => {}
    }
    let mut v: Vec<&str> = match kind {
        Text => vec!["text", "font", "weight", "size", "align", "tracking", "leading"],
        View => vec!["camera"],
        Image => vec!["src", "fit"],
        Points => vec!["src", "cell", "lit"],
        Model => vec!["src"],
        Video => vec!["src", "fit", "trim", "volume"],
        Clip => vec!["src", "fit", "trim"],
        Shader => vec!["src", "params"],
        _ => vec![],
    };
    v.extend(["comp", "loc", "rot", "scale", "color", "opacity", "in", "out", "visible", "parent"]);
    v.extend(match kind {
        Rect | Ellipse => &["radius", "z", "rep", "spacing", "rrot", "rjitter", "rshade", "rdelay"][..],
        Text => &["z", "stagger", "order", "gloc", "grot", "gscale", "galpha", "gspread", "gspin"][..],
        View | Image | Video | Clip | Shader => &["z"][..],
        Camera => &["fov", "focus", "aperture", "haze"][..],
        Light => &["energy", "shadow"][..],
        Cube | Sphere | Plane | Cylinder | Model => {
            &["metal", "rough", "glass", "fuzz", "emit", "soft", "sway", "tex", "uvscale", "rep", "spacing", "rrot", "rjitter", "rshade", "rdelay"][..]
        }
        _ => &[][..],
    });
    v
}

fn str_val(v: &Value) -> Result<String> {
    match v {
        Value::Str(s) => Ok(s.clone()),
        Value::Num(n) => Ok(fmt_num(*n)),
        _ => bail!("expected text, got {v}"),
    }
}

/// Plugin names: what a folder under `plugins/` may be called.
pub fn is_plugin_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 48 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn enum_val<T: Copy>(v: &Value, all: &[T], name: fn(T) -> &'static str) -> Result<T> {
    let s = str_val(v)?;
    all.iter().copied().find(|x| name(*x) == s).ok_or_else(|| {
        let opts: Vec<&str> = all.iter().map(|x| name(*x)).collect();
        anyhow!("`{s}` is not one of {}", opts.join("|"))
    })
}
pub const SCENE_PROPS: [&str; 14] = [
    "fps", "start", "end", "res", "camera", "music", "bg", "ambient", "env", "envrot", "bloom", "shutter", "grain", "vignette",
];

pub fn component_index(c: &str) -> Option<usize> {
    match c {
        "x" | "r" | "0" => Some(0),
        "y" | "g" | "1" => Some(1),
        "z" | "b" | "2" => Some(2),
        "w" | "a" | "3" => Some(3),
        _ => None,
    }
}

/// A parsed data path.
#[derive(Clone, Debug, PartialEq)]
pub struct Path {
    /// Object name, or `scene`.
    pub owner: String,
    pub prop: String,
    pub index: Option<usize>,
}

impl Path {
    pub fn parse(s: &str) -> Result<Path> {
        let mut parts = s.split('.');
        let owner = parts.next().filter(|p| !p.is_empty()).ok_or_else(|| anyhow!("empty path"))?;
        let prop = parts.next().ok_or_else(|| anyhow!("path `{s}` needs a property"))?;
        let index = match parts.next() {
            None => None,
            Some(c) => Some(component_index(c).ok_or_else(|| anyhow!("bad component `{c}` in `{s}`"))?),
        };
        if parts.next().is_some() {
            bail!("path `{s}` is too deep");
        }
        Ok(Path { owner: owner.into(), prop: prop.into(), index })
    }

    pub fn is_scene(&self) -> bool {
        self.owner == "scene"
    }

    /// The channel name inside the owner, e.g. `loc.z`.
    pub fn channel(&self) -> String {
        match self.index {
            Some(i) => format!("{}.{}", self.prop, ["x", "y", "z", "w"][i]),
            None => self.prop.clone(),
        }
    }
}

pub fn get_object(o: &Object, prop: &str) -> Result<Value> {
    Ok(match prop {
        "kind" => Value::Str(o.kind.name().into()),
        "parent" => o.parent.clone().map_or(Value::None, Value::Str),
        "comp" => o.comp.clone().map_or(Value::None, Value::Str),
        "loc" => Value::Vec(o.loc.to_array().to_vec()),
        "rot" => Value::Vec(o.rot.to_array().to_vec()),
        "scale" => Value::Vec(o.scale.to_array().to_vec()),
        "color" => Value::Vec(o.color.to_vec()),
        "visible" => Value::Bool(o.visible),
        "fov" => Value::Num(o.fov),
        "energy" => Value::Num(o.energy),
        "focus" => Value::Num(o.focus),
        "aperture" => Value::Num(o.aperture),
        "haze" => Value::Num(o.haze),
        "shadow" => Value::Num(o.shadow),
        "lit" => Value::Num(o.lit),
        "radius" => Value::Num(o.radius),
        "z" => Value::Num(o.z),
        "opacity" => Value::Num(o.opacity),
        "in" => o.t_in.map_or(Value::None, Value::Num),
        "out" => o.t_out.map_or(Value::None, Value::Num),
        "camera" => o.camera.clone().map_or(Value::None, Value::Str),
        "text" => Value::Str(o.text.body.clone()),
        "font" => Value::Str(o.text.font.clone()),
        "weight" => Value::Num(o.text.weight),
        "size" => Value::Num(o.text.size),
        "align" => Value::Str(o.text.align.name().into()),
        "tracking" => Value::Num(o.text.tracking),
        "leading" => Value::Num(o.text.leading),
        "stagger" => Value::Num(o.text.stagger),
        "order" => Value::Str(o.text.order.name().into()),
        "gloc" => Value::Vec(o.text.gloc.to_array().to_vec()),
        "grot" => Value::Num(o.text.grot),
        "gscale" => Value::Vec(o.text.gscale.to_array().to_vec()),
        "galpha" => Value::Num(o.text.galpha),
        "gspread" => Value::Vec(o.text.gspread.to_array().to_vec()),
        "gspin" => Value::Num(o.text.gspin),
        "metal" => Value::Num(o.material.metal),
        "rough" => Value::Num(o.material.rough),
        "glass" => Value::Num(o.material.glass),
        "fuzz" => Value::Num(o.material.fuzz),
        "emit" => Value::Num(o.material.emit),
        "soft" => Value::Num(o.material.soft),
        "sway" => Value::Vec(o.material.sway.to_array().to_vec()),
        "rep" => Value::Vec(o.repeat.count.to_array().to_vec()),
        "spacing" => Value::Vec(o.repeat.spacing.to_array().to_vec()),
        "rrot" => Value::Vec(o.repeat.rot.to_array().to_vec()),
        "rjitter" => Value::Vec(o.repeat.jitter.to_array().to_vec()),
        "rshade" => Value::Num(o.repeat.shade),
        "rdelay" => Value::Num(o.repeat.delay),
        "fx" => Value::Str(if o.fx.plugin.is_empty() { o.fx.kind.name().into() } else { o.fx.plugin.clone() }),
        "amount" => Value::Num(o.fx.amount),
        "cell" => Value::Num(o.fx.cell),
        "params" => Value::Vec(o.fx.params.to_vec()),
        "src" => Value::Str(o.media.src.clone()),
        "fit" => Value::Str(o.media.fit.name().into()),
        "tex" => Value::Str(o.material.tex.clone()),
        "uvscale" => Value::Num(o.material.uvscale),
        "trim" => Value::Num(o.media.trim),
        "volume" => Value::Num(o.media.volume),
        "bpm" => Value::Num(o.media.bpm),
        "beat0" => Value::Num(o.media.beat0),
        _ => bail!("unknown object property `{prop}`"),
    })
}

pub fn set_object(o: &mut Object, prop: &str, v: &Value) -> Result<()> {
    match prop {
        "parent" => o.parent = v.opt_name()?,
        "comp" => o.comp = v.opt_name()?,
        "loc" => o.loc = v.vec3()?,
        "rot" => o.rot = v.vec3()?,
        "scale" => o.scale = v.vec3()?,
        "color" => o.color = v.color()?,
        "visible" => o.visible = v.bool()?,
        "fov" => o.fov = v.num()?,
        "energy" => o.energy = v.num()?,
        "focus" => o.focus = v.num()?.max(0.01),
        "aperture" => o.aperture = v.num()?.max(0.0),
        "haze" => o.haze = v.num()?.max(0.0),
        "shadow" => o.shadow = v.num()?.max(0.0),
        "lit" => o.lit = v.num()?.clamp(0.0, 1.0),
        "radius" => o.radius = v.num()?,
        "z" => o.z = v.num()?,
        "opacity" => o.opacity = v.num()?,
        "in" => o.t_in = if *v == Value::None { None } else { Some(v.num()?) },
        "out" => o.t_out = if *v == Value::None { None } else { Some(v.num()?) },
        "camera" => o.camera = v.opt_name()?,
        "text" => o.text.body = str_val(v)?,
        "font" => o.text.font = str_val(v)?,
        "weight" => o.text.weight = v.num()?.clamp(1.0, 1000.0),
        "size" => o.text.size = v.num()?,
        "align" => o.text.align = enum_val(v, &Align::ALL, Align::name)?,
        "tracking" => o.text.tracking = v.num()?,
        "leading" => o.text.leading = v.num()?,
        "stagger" => o.text.stagger = v.num()?,
        "order" => o.text.order = enum_val(v, &Order::ALL, Order::name)?,
        "gloc" => o.text.gloc = v.vec3()?,
        "grot" => o.text.grot = v.num()?,
        "gscale" => o.text.gscale = v.vec3()?,
        "galpha" => o.text.galpha = v.num()?,
        "gspread" => o.text.gspread = v.vec3()?,
        "gspin" => o.text.gspin = v.num()?,
        "metal" => o.material.metal = v.num()?,
        "rough" => o.material.rough = v.num()?,
        "glass" => o.material.glass = v.num()?,
        "fuzz" => o.material.fuzz = v.num()?,
        "emit" => o.material.emit = v.num()?.clamp(0.0, 1.0),
        "soft" => o.material.soft = v.num()?,
        "sway" => o.material.sway = v.vec3()?,
        "rep" => o.repeat.count = v.vec3()?.round().max(glam::Vec3::ONE),
        "spacing" => o.repeat.spacing = v.vec3()?,
        "rrot" => o.repeat.rot = v.vec3()?,
        "rjitter" => o.repeat.jitter = v.vec3()?,
        "rshade" => o.repeat.shade = v.num()?,
        "rdelay" => o.repeat.delay = v.num()?,
        "fx" => {
            // A built-in effect by name, or else a shader plugin of that name.
            let name = str_val(v)?;
            match FxKind::ALL.into_iter().find(|k| k.name() == name) {
                Some(k) => (o.fx.kind, o.fx.plugin) = (k, String::new()),
                None if is_plugin_name(&name) => o.fx.plugin = name,
                None => bail!("`{name}` is not a built-in effect or a plugin name (letters, digits, _ and -)"),
            }
        }
        "amount" => o.fx.amount = v.num()?,
        "cell" => o.fx.cell = v.num()?.max(1.0),
        "params" => {
            let xs = v.floats()?;
            if xs.is_empty() || xs.len() > 4 {
                bail!("params takes 1-4 numbers, got {v}");
            }
            o.fx.params[..xs.len()].copy_from_slice(&xs);
        }
        "src" => o.media.src = str_val(v)?,
        "fit" => o.media.fit = enum_val(v, &Fit::ALL, Fit::name)?,
        "tex" => o.material.tex = if *v == Value::None { String::new() } else { str_val(v)? },
        "uvscale" => o.material.uvscale = v.num()?,
        "trim" => o.media.trim = v.num()?,
        "volume" => o.media.volume = v.num()?,
        "bpm" => o.media.bpm = v.num()?,
        "beat0" => o.media.beat0 = v.num()?,
        "kind" => bail!("`kind` is read-only"),
        _ => bail!("unknown object property `{prop}`"),
    }
    Ok(())
}

pub fn get_scene(s: &Scene, prop: &str) -> Result<Value> {
    Ok(match prop {
        "fps" => Value::Num(s.fps),
        "start" => Value::Num(s.start as f32),
        "end" => Value::Num(s.end as f32),
        "res" => Value::Vec(vec![s.res[0] as f32, s.res[1] as f32]),
        "camera" => s.camera.clone().map_or(Value::None, Value::Str),
        "music" => s.music.clone().map_or(Value::None, Value::Str),
        "bg" => Value::Vec(s.bg.to_vec()),
        "ambient" => Value::Num(s.ambient),
        "env" => Value::Str(s.env.clone()),
        "envrot" => Value::Num(s.envrot),
        "bloom" => Value::Num(s.bloom),
        "shutter" => Value::Num(s.shutter),
        "grain" => Value::Num(s.grain),
        "vignette" => Value::Num(s.vignette),
        _ => bail!("unknown scene property `{prop}`"),
    })
}

pub fn set_scene(s: &mut Scene, prop: &str, v: &Value) -> Result<()> {
    match prop {
        "fps" => s.fps = v.num()?.max(1.0),
        "start" => s.start = v.num()? as i32,
        "end" => s.end = v.num()? as i32,
        "res" => match v.floats()?.as_slice() {
            [w, h] if *w >= 1.0 && *h >= 1.0 => s.res = [*w as u32, *h as u32],
            _ => bail!("res needs w,h"),
        },
        "camera" => s.camera = v.opt_name()?,
        "music" => s.music = v.opt_name()?,
        "bg" => s.bg = v.color()?,
        "ambient" => s.ambient = v.num()?,
        "env" => s.env = if *v == Value::None { String::new() } else { str_val(v)? },
        "envrot" => s.envrot = v.num()?,
        "bloom" => s.bloom = v.num()?.max(0.0),
        "shutter" => s.shutter = v.num()?.clamp(0.0, 1.0),
        "grain" => s.grain = v.num()?.clamp(0.0, 1.0),
        "vignette" => s.vignette = v.num()?.clamp(0.0, 1.0),
        _ => bail!("unknown scene property `{prop}`"),
    }
    Ok(())
}

fn with_index(v: Value, index: Option<usize>) -> Result<Value> {
    match (index, v) {
        (None, v) => Ok(v),
        (Some(i), Value::Vec(xs)) => {
            xs.get(i).copied().map(Value::Num).ok_or_else(|| anyhow!("component out of range"))
        }
        (Some(_), v) => bail!("{v} has no components"),
    }
}

pub fn get(doc: &Document, path: &Path) -> Result<Value> {
    let v = if path.is_scene() {
        get_scene(&doc.scene, &path.prop)?
    } else {
        get_object(object(doc, &path.owner)?, &path.prop)?
    };
    with_index(v, path.index)
}

/// Set a property; a component path writes into the existing vector.
pub fn set(doc: &mut Document, path: &Path, v: &Value) -> Result<()> {
    let value = match path.index {
        None => v.clone(),
        Some(i) => {
            let Value::Vec(mut xs) = get(doc, &Path { index: None, ..path.clone() })? else {
                bail!("`{}` has no components", path.prop);
            };
            *xs.get_mut(i).ok_or_else(|| anyhow!("component out of range"))? = v.num()?;
            Value::Vec(xs)
        }
    };
    if path.is_scene() {
        set_scene(&mut doc.scene, &path.prop, &value)
    } else {
        let o = doc.objects.get_mut(&path.owner).ok_or_else(|| no_object(&path.owner))?;
        set_object(o, &path.prop, &value)
    }
}

/// Write one animated scalar channel (`loc.z`, `energy`) into an object.
pub fn set_channel(o: &mut Object, channel: &str, v: f32) {
    let (prop, comp) = channel.split_once('.').map_or((channel, None), |(p, c)| (p, Some(c)));
    let _ = match comp.and_then(component_index) {
        None => set_object(o, prop, &Value::Num(v)),
        Some(i) => match get_object(o, prop) {
            Ok(Value::Vec(mut xs)) if i < xs.len() => {
                xs[i] = v;
                set_object(o, prop, &Value::Vec(xs))
            }
            _ => Ok(()),
        },
    };
}

/// Scalar channels a property animates through (`loc` -> `loc.x`, `loc.y`, `loc.z`).
pub fn channels(o: &Object, prop: &str) -> Result<Vec<String>> {
    Ok(match get_object(o, prop)? {
        Value::Num(_) => vec![prop.into()],
        Value::Vec(xs) => (0..xs.len()).map(|i| format!("{prop}.{}", ["x", "y", "z", "w"][i])).collect(),
        _ => bail!("`{prop}` cannot be animated"),
    })
}

pub fn object<'a>(doc: &'a Document, name: &str) -> Result<&'a Object> {
    doc.objects.get(name).ok_or_else(|| no_object(name))
}

pub fn no_object(name: &str) -> anyhow::Error {
    anyhow!("no object `{name}`")
}
