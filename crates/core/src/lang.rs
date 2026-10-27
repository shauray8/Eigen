//! The op language: a line-oriented text form of operators. It is the agent's
//! whole interface, the UI's undo/log text, and the project file format, so
//! reading state costs the same few tokens as writing it.
//!
//! ```text
//! add cube box loc=0,1,0 color=#ff5533     // create with properties
//! set box.loc.z 2                          // one property (or component)
//! set box rot=0,45,0 scale=2               // several properties
//! key box.loc 0,3,0 @48 outBack            // keyframe a property (all channels)
//! key box loc=0,0,0 rot=0,0,0 @0           // keyframe several properties
//! unkey box.loc @48                        // drop one key (or all, without @)
//! dup box box2 loc=2,1,0                   // copy (with keys), then set props
//! ren box crate ; del crate                // `;` separates statements
//! ```

use anyhow::{Context, Result, anyhow, bail};

use crate::anim::Ease;
use crate::data::{Document, Kind, Object, Scene};
use crate::ops::{Frame, Op};
use crate::paths::{self, Path, Value, fmt_num};

/// Splits a line into statements of tokens. `;` and `//` are only special
/// outside quotes; quotes and escapes are kept for [`parse_value`].
fn tokenize(line: &str) -> Result<Vec<Vec<String>>> {
    let mut stmts = vec![Vec::new()];
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    let flush = |cur: &mut String, stmts: &mut Vec<Vec<String>>| {
        if !cur.is_empty() {
            stmts.last_mut().unwrap().push(std::mem::take(cur));
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            '\\' if quoted => {
                cur.push(c);
                cur.extend(chars.next());
            }
            '/' if !quoted && chars.peek() == Some(&'/') => break,
            ';' if !quoted => {
                flush(&mut cur, &mut stmts);
                stmts.push(Vec::new());
            }
            c if c.is_whitespace() && !quoted => flush(&mut cur, &mut stmts),
            c => cur.push(c),
        }
    }
    if quoted {
        bail!("unclosed quote");
    }
    flush(&mut cur, &mut stmts);
    Ok(stmts.into_iter().filter(|s| !s.is_empty()).collect())
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                // \u{947}: written by older saves
                Some('u') if chars.as_str().starts_with('{') => {
                    let rest = chars.as_str();
                    let end = rest.find('}').unwrap_or(0);
                    match u32::from_str_radix(&rest[1..end.max(1)], 16).ok().and_then(char::from_u32) {
                        Some(ch) if end > 1 => {
                            out.push(ch);
                            chars = rest[end + 1..].chars();
                        }
                        _ => out.push('u'),
                    }
                }
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn parse_value(s: &str) -> Result<Value> {
    if let Some(q) = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return Ok(Value::Str(unescape(q)));
    }
    match s {
        "none" => return Ok(Value::None),
        "true" => return Ok(Value::Bool(true)),
        "false" => return Ok(Value::Bool(false)),
        _ => {}
    }
    if s.starts_with('#') {
        return paths::parse_hex(s).map(|c| Value::Vec(c.to_vec())).ok_or_else(|| anyhow!("bad color `{s}`"));
    }
    let starts_numeric = s.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '.');
    if starts_numeric {
        let nums: Vec<f32> = s
            .split(',')
            .map(|p| p.parse::<f32>().map_err(|_| anyhow!("bad number `{p}`")))
            .collect::<Result<_>>()?;
        return Ok(if nums.len() == 1 { Value::Num(nums[0]) } else { Value::Vec(nums) });
    }
    Ok(Value::Str(s.into()))
}

fn parse_props(tokens: &[String]) -> Result<Vec<(String, Value)>> {
    tokens
        .iter()
        .map(|t| {
            let (k, v) = t.split_once('=').ok_or_else(|| anyhow!("expected prop=value, got `{t}`"))?;
            Ok((k.to_string(), parse_value(v)?))
        })
        .collect()
}

/// Splits a trailing `@frame` and optional ease (`outBack` or `ease=outBack`).
fn split_frame(tokens: &[String]) -> Result<(&[String], Frame, Ease)> {
    let at = tokens.iter().position(|t| t.starts_with('@')).ok_or_else(|| anyhow!("key needs @frame"))?;
    let frame = Frame::parse(&tokens[at][1..]).ok_or_else(|| anyhow!("bad frame `{}`", tokens[at]))?;
    let ease = match &tokens[at + 1..] {
        [] => Ease::default(),
        [e] => {
            let e = e.strip_prefix("ease=").unwrap_or(e);
            Ease::parse(e).ok_or_else(|| anyhow!("unknown ease `{e}`"))?
        }
        _ => bail!("unexpected tokens after @frame"),
    };
    Ok((&tokens[..at], frame, ease))
}

fn statement(tokens: &[String]) -> Result<Vec<Op>> {
    let (verb, rest) = tokens.split_first().ok_or_else(|| anyhow!("empty statement"))?;
    let obj_path = |name: &str, prop: &str| Path { owner: name.into(), prop: prop.into(), index: None };
    Ok(match (verb.as_str(), rest) {
        ("add", [kind, name, props @ ..]) => {
            let kind = Kind::parse(kind).ok_or_else(|| anyhow!("unknown kind `{kind}`"))?;
            vec![Op::Add { kind, name: name.clone(), props: parse_props(props)? }]
        }
        ("set", [path, value]) if !value.contains('=') => {
            vec![Op::Set { path: Path::parse(path)?, value: parse_value(value)? }]
        }
        ("set", [owner, props @ ..]) if !props.is_empty() => parse_props(props)?
            .into_iter()
            .map(|(p, value)| Op::Set { path: obj_path(owner, &p), value })
            .collect(),
        ("key", rest) => {
            let (head, frame, ease) = split_frame(rest)?;
            match head {
                [path, value] if !value.contains('=') => {
                    vec![Op::Key { path: Path::parse(path)?, value: parse_value(value)?, frame, ease }]
                }
                [owner, props @ ..] if !props.is_empty() => parse_props(props)?
                    .into_iter()
                    .map(|(p, value)| Op::Key { path: obj_path(owner, &p), value, frame, ease })
                    .collect(),
                _ => bail!("usage: key <path> <value> @frame [ease]"),
            }
        }
        ("unkey", [path]) => vec![Op::Unkey { path: Path::parse(path)?, frame: None }],
        ("unkey", [path, at]) => {
            let f = at.strip_prefix('@').and_then(Frame::parse).ok_or_else(|| anyhow!("bad frame `{at}`"))?;
            vec![Op::Unkey { path: Path::parse(path)?, frame: Some(f) }]
        }
        ("del", names) if !names.is_empty() => names.iter().map(|n| Op::Del { name: n.clone() }).collect(),
        ("dup", [src, name, props @ ..]) => {
            vec![Op::Dup { src: src.clone(), name: name.clone(), props: parse_props(props)? }]
        }
        ("ren", [from, to]) => vec![Op::Ren { from: from.clone(), to: to.clone() }],
        ("expr", [path, src]) => match parse_value(src)? {
            Value::Str(s) => vec![Op::Expr { path: Path::parse(path)?, src: Some(s) }],
            Value::Num(n) => vec![Op::Expr { path: Path::parse(path)?, src: Some(fmt_num(n)) }],
            _ => bail!("usage: expr <path> \"<script>\""),
        },
        ("unexpr", [path]) => vec![Op::Expr { path: Path::parse(path)?, src: None }],
        (v, _) => bail!("bad statement `{v} ...` (verbs: add set key unkey expr unexpr dup del ren in)"),
    })
}

/// Parses a script. Each op carries its 1-based line number for error reporting.
/// `in <comp>` makes later `add`/`dup` in the script land in that composition
/// (as if they said `comp=<comp>`) until `in main`.
pub fn parse(script: &str) -> Result<Vec<(usize, Op)>> {
    let mut out = Vec::new();
    let mut inside: Option<String> = None;
    for (i, line) in script.lines().enumerate() {
        for tokens in tokenize(line).with_context(|| format!("line {}", i + 1))? {
            if let [verb, comp] = tokens.as_slice()
                && verb == "in"
            {
                inside = (comp != "main").then(|| comp.clone());
                continue;
            }
            for mut op in statement(&tokens).with_context(|| format!("line {}: `{}`", i + 1, tokens.join(" ")))? {
                if let (Some(c), Op::Add { props, .. } | Op::Dup { props, .. }) = (&inside, &mut op)
                    && !props.iter().any(|(p, _)| p == "comp")
                {
                    props.insert(0, ("comp".into(), Value::Str(c.clone())));
                }
                out.push((i + 1, op));
            }
        }
    }
    Ok(out)
}

/// Properties of `o` that differ from a fresh object of its kind.
pub fn changed_props(o: &Object) -> Vec<(&'static str, Value)> {
    let fresh = Object::new(o.kind);
    paths::OBJECT_PROPS
        .into_iter()
        .filter_map(|p| {
            let v = paths::get_object(o, p).ok()?;
            (paths::get_object(&fresh, p).ok()? != v).then_some((p, v))
        })
        .collect()
}

pub fn fmt_props(props: &[(&str, Value)]) -> String {
    props.iter().map(|(k, v)| format!(" {k}={v}")).collect()
}

/// Key lines for one object, folding channels of a property into one line
/// when they share frames and easing.
pub fn fmt_keys(name: &str, o: &Object) -> Vec<String> {
    let mut lines = Vec::new();
    let mut done = std::collections::HashSet::new();
    for chan in o.anim.keys() {
        if done.contains(chan) {
            continue;
        }
        let prop = chan.split('.').next().unwrap();
        let group = paths::channels(o, prop).unwrap_or_default();
        let curves: Option<Vec<_>> = group.iter().map(|c| o.anim.get(c)).collect();
        let foldable = group.len() > 1
            && curves.as_ref().is_some_and(|cs| {
                cs.iter().all(|c| {
                    c.keys.len() == cs[0].keys.len()
                        && c.keys.iter().zip(&cs[0].keys).all(|(a, b)| a.frame == b.frame && a.ease == b.ease)
                })
            });
        if foldable {
            let cs = curves.unwrap();
            for (i, k) in cs[0].keys.iter().enumerate() {
                let v = Value::Vec(cs.iter().map(|c| c.keys[i].value).collect());
                lines.push(format!("key {name}.{prop} {v} @{} {}", fmt_num(k.frame), k.ease.name()));
            }
            done.extend(group);
        } else {
            for k in &o.anim[chan].keys {
                lines.push(format!("key {name}.{chan} {} @{} {}", fmt_num(k.value), fmt_num(k.frame), k.ease.name()));
            }
            done.insert(chan.clone());
        }
    }
    for (path, src) in &o.expr {
        lines.push(format!("expr {name}.{path} {}", Value::Str(src.clone())));
    }
    lines
}

pub fn fmt_scene(s: &Scene) -> Option<String> {
    let fresh = Scene::default();
    let props: Vec<(&str, Value)> = paths::SCENE_PROPS
        .into_iter()
        .filter(|p| !matches!(*p, "camera" | "music"))
        .filter_map(|p| {
            let v = paths::get_scene(s, p).ok()?;
            (paths::get_scene(&fresh, p).ok()? != v).then_some((p, v))
        })
        .collect();
    (!props.is_empty()).then(|| format!("set scene{}", fmt_props(&props)))
}

/// Serializes a whole document as a script that rebuilds it.
/// Dependencies (comp, parent, camera, a clip's comp) are emitted first so
/// every reference resolves; a comp's camera lives inside it, so it is set last.
pub fn dump(doc: &Document) -> String {
    let mut out: Vec<String> = fmt_scene(&doc.scene).into_iter().collect();
    let mut emitted = std::collections::HashSet::new();
    let mut late = Vec::new();
    fn visit(doc: &Document, name: &str, emitted: &mut std::collections::HashSet<String>, out: &mut Vec<String>, late: &mut Vec<String>) {
        if emitted.contains(name) {
            return;
        }
        let o = &doc.objects[name];
        let is_comp = o.kind == Kind::Comp;
        let shows = (o.kind == Kind::Clip).then_some(&o.media.src);
        let camera = o.camera.as_ref().filter(|_| !is_comp);
        for dep in [o.comp.as_ref(), o.parent.as_ref(), camera, shows].into_iter().flatten() {
            if doc.objects.contains_key(dep) {
                visit(doc, dep, emitted, out, late);
            }
        }
        emitted.insert(name.to_string());
        let mut props = changed_props(o);
        if is_comp && let Some(c) = &o.camera {
            props.retain(|(p, _)| *p != "camera");
            late.push(format!("set {name}.camera {c}"));
        }
        out.push(format!("add {} {name}{}", o.kind.name(), fmt_props(&props)));
        out.extend(fmt_keys(name, o));
    }
    for name in doc.objects.keys() {
        visit(doc, name, &mut emitted, &mut out, &mut late);
    }
    out.extend(late);
    if let Some(c) = &doc.scene.camera {
        out.push(format!("set scene.camera {c}"));
    }
    if let Some(m) = &doc.scene.music {
        out.push(format!("set scene.music {m}"));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::apply;

    fn run(doc: &mut Document, s: &str) {
        for (_, op) in parse(s).unwrap() {
            apply(doc, &op).unwrap();
        }
    }

    #[test]
    fn dump_round_trips() {
        let mut doc = Document::default();
        run(
            &mut doc,
            "add empty rig\nadd cube box loc=0,1,0 color=#ff5533 parent=rig\n\
             key box.loc 0,0,0 @0 ; key box.loc 0,3,0 @48 outBack\nkey box.rot.y 90 @24 linear\n\
             add camera cam loc=0,2,8 ; set scene.camera cam ; set scene fps=30 bg=#101010",
        );
        let text = dump(&doc);
        let mut again = Document::default();
        run(&mut again, &text);
        assert_eq!(text, dump(&again));
        assert!(text.contains("key box.loc 0,3,0 @48 outBack"), "{text}");
        assert!(text.contains("color=#ff5533"), "{text}");
    }

    #[test]
    fn text_round_trips_with_awkward_strings() {
        let mut doc = Document::default();
        run(
            &mut doc,
            r#"add text t text="A; B // C=\"q\"\nline 2" size=96 align=left order=center stagger=2 // comment
               key t gloc=0,80 galpha=0 @0 outBack ; key t gloc=0 galpha=1 @12"#,
        );
        assert_eq!(doc.objects["t"].text.body, "A; B // C=\"q\"\nline 2");
        let text = dump(&doc);
        let mut again = Document::default();
        run(&mut again, &text);
        assert_eq!(again.objects["t"].text, doc.objects["t"].text, "{text}");
        assert_eq!(text, dump(&again));
    }

    #[test]
    fn beat_references_resolve_through_music() {
        let mut doc = Document::default();
        run(&mut doc, "set scene fps=30 ; add audio m src=a.mp3 bpm=120 beat0=0.5 in=10 ; set scene.music m");
        run(&mut doc, "add rect r in=b4 out=b8 ; key r.opacity 0 @b2 ; key r.opacity 1 @b2.5");
        let r = &doc.objects["r"];
        // beat n at 10 + (0.5 + n * 0.5s) * 30fps
        assert_eq!((r.t_in, r.t_out), (Some(85.0), Some(145.0)));
        let frames: Vec<f32> = r.anim["opacity"].keys.iter().map(|k| k.frame).collect();
        assert_eq!(frames, vec![55.0, 62.5_f32.round()]);
        let text = dump(&doc);
        let mut again = Document::default();
        run(&mut again, &text);
        assert_eq!(text, dump(&again));
        // Trimmed into the song: b0 is the first downbeat after the trim point.
        run(&mut doc, "set m trim=10.4");
        assert_eq!(doc.beat_frame(0.0), Ok(13.0)); // downbeat at 10.5s, 0.1s into the clip
    }
}
