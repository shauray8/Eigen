//! Queries return op-language text, so state reads cost the same few tokens
//! as writes and can be pasted back as ops.
//!
//! ```text
//! *            overview: one line per object, keys summarised
//! dump         full document as a script
//! scene        scene settings
//! box          one object with all of its keys
//! box.loc.z    one value
//! kind=light   objects of a kind
//! comp=shot1   one composition and its contents (`comp=main`: the film's own)
//! box @48      any of the above evaluated at frame 48
//! ```

use anyhow::{Result, anyhow};

use crate::eval::animated;
use crate::data::{Document, Kind, Object};
use crate::lang::{changed_props, dump, fmt_keys, fmt_props};
use crate::paths::{self, Path, SCENE_PROPS, fmt_num};

fn key_summary(o: &Object) -> String {
    let mut props: Vec<(String, f32, f32)> = Vec::new();
    for (chan, curve) in &o.anim {
        let prop = chan.split('.').next().unwrap().to_string();
        let (a, b) = (curve.keys.first().unwrap().frame, curve.keys.last().unwrap().frame);
        match props.iter_mut().find(|p| p.0 == prop) {
            Some(p) => {
                p.1 = p.1.min(a);
                p.2 = p.2.max(b);
            }
            None => props.push((prop, a, b)),
        }
    }
    let mut out = String::new();
    if !props.is_empty() {
        let parts: Vec<String> = props.iter().map(|(p, a, b)| format!("{p}@{}-{}", fmt_num(*a), fmt_num(*b))).collect();
        out = format!("  // keys {}", parts.join(" "));
    }
    if !o.expr.is_empty() {
        let paths: Vec<&str> = o.expr.keys().map(String::as_str).collect();
        out += &format!("{} expr {}", if out.is_empty() { "  //" } else { ";" }, paths.join(" "));
    }
    out
}

fn object_line(name: &str, o: &Object, frame: Option<f32>, fps: f32) -> String {
    let shown = frame.map_or_else(|| o.clone(), |f| animated(o, name, f, fps));
    format!("add {} {name}{}{}", o.kind.name(), fmt_props(&changed_props(&shown)), key_summary(o))
}

pub fn query(doc: &Document, selector: &str, now: f32) -> Result<String> {
    let mut sel = selector.trim();
    let mut frame = None;
    if let Some((s, f)) = sel.rsplit_once('@') {
        frame = Some(f.trim().parse::<f32>().map_err(|_| anyhow!("bad frame `{f}`"))?);
        sel = s.trim();
    }
    let fps = doc.scene.fps;
    let lines: Vec<String> = match sel {
        "" | "*" => {
            let mut v = vec![format!(
                "// {} objects, frames {}-{} @{}fps, camera {}, now @{}",
                doc.objects.len(),
                doc.scene.start,
                doc.scene.end,
                fmt_num(doc.scene.fps),
                doc.scene.camera.as_deref().unwrap_or("none"),
                fmt_num(now)
            )];
            v.extend(doc.objects.iter().map(|(n, o)| object_line(n, o, frame, fps)));
            v
        }
        "dump" => vec![dump(doc)],
        "scene" => {
            let props: Vec<String> = SCENE_PROPS
                .iter()
                .map(|p| format!(" {p}={}", paths::get_scene(&doc.scene, p).unwrap()))
                .collect();
            vec![format!("set scene{}", props.concat())]
        }
        s if s.starts_with("kind=") => {
            let kind = Kind::parse(&s[5..]).ok_or_else(|| anyhow!("unknown kind `{}`", &s[5..]))?;
            doc.objects.iter().filter(|(_, o)| o.kind == kind).map(|(n, o)| object_line(n, o, frame, fps)).collect()
        }
        s if s.starts_with("comp=") => {
            let comp = Some(&s[5..]).filter(|c| *c != "main");
            if let Some(c) = comp {
                paths::object(doc, c)?;
            }
            doc.objects
                .iter()
                .filter(|(n, o)| o.comp.as_deref() == comp || comp == Some(n.as_str()))
                .map(|(n, o)| object_line(n, o, frame, fps))
                .collect()
        }
        s if s.contains('.') => {
            let path = Path::parse(s)?;
            let value = match (frame, path.is_scene()) {
                (Some(f), false) => {
                    let o = animated(paths::object(doc, &path.owner)?, &path.owner, f, fps);
                    let v = paths::get_object(&o, &path.prop)?;
                    match (path.index, v) {
                        (Some(i), paths::Value::Vec(xs)) => paths::Value::Num(xs[i]),
                        (_, v) => v,
                    }
                }
                _ => paths::get(doc, &path)?,
            };
            vec![format!("{s} {value}")]
        }
        name => {
            let o = paths::object(doc, name)?;
            let shown = frame.map_or_else(|| o.clone(), |f| animated(o, name, f, fps));
            let mut v = vec![format!("add {} {name}{}", o.kind.name(), fmt_props(&changed_props(&shown)))];
            v.extend(fmt_keys(name, o));
            v
        }
    };
    Ok(lines.join("\n"))
}
