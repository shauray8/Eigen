//! Web sources: any URL, openly licensed images (Openverse), and CC0 PBR
//! textures, HDRIs and models (Poly Haven).

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

use crate::{MediaKind, folder, kind_of, unique};

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .user_agent("editor/0.1 (personal film tool)")
        .build()
        .into()
}

fn get_json(url: &str) -> Result<Value> {
    Ok(agent().get(url).call()?.body_mut().read_json::<Value>()?)
}

/// Downloads `url` to the file `dest`.
pub fn download(url: &str, dest: &Path) -> Result<()> {
    let origin = url.splitn(4, '/').take(3).collect::<Vec<_>>().join("/") + "/";
    let mut resp = agent().get(url).header("Referer", &origin).call().with_context(|| format!("downloading {url}"))?;
    let mut reader = resp.body_mut().with_config().limit(2 * 1024 * 1024 * 1024).reader();
    let mut file = std::fs::File::create(dest)?;
    std::io::copy(&mut reader, &mut file)?;
    Ok(())
}

fn enc(q: &str) -> String {
    q.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().trim().to_string()
}

/// Openverse image search: `ovi:<id> | title | WxH | source` lines.
pub fn image_search(q: &str) -> Result<Vec<String>> {
    let d = get_json(&format!("https://api.openverse.org/v1/images/?q={}&page_size=20", enc(q)))?;
    Ok(d["results"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            format!(
                "ovi:{} | {} | {}x{} | {}",
                s(&r["id"]),
                s(&r["title"]),
                r["width"].as_u64().unwrap_or(0),
                r["height"].as_u64().unwrap_or(0),
                s(&r["source"])
            )
        })
        .collect())
}

/// Downloads a URL or an `ovi:` image into the library; returns its `src`.
pub fn fetch(project: &Path, what: &str) -> Result<String> {
    let url = match what.strip_prefix("ovi:") {
        Some(id) => s(&get_json(&format!("https://api.openverse.org/v1/images/{id}/"))?["url"]),
        None if what.starts_with("http://") || what.starts_with("https://") => what.to_string(),
        None => bail!("expected a URL or an ovi: id, got `{what}`"),
    };
    let path_part = url.split(['?', '#']).next().unwrap_or(&url);
    let name = path_part.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or("download");
    let mut name: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    let kind = kind_of(Path::new(&name)).unwrap_or_else(|| {
        name.push_str(".jpg");
        MediaKind::Image
    });
    let dir = project.join(folder(kind));
    std::fs::create_dir_all(&dir)?;
    let dest = unique(&dir, &name);
    download(&url, &dest)?;
    Ok(format!("{}/{}", folder(kind), dest.file_name().unwrap().to_string_lossy()))
}

/// Poly Haven asset types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolyHaven {
    Texture,
    Hdri,
    Model,
}

impl PolyHaven {
    /// `texture`, `hdri` or `model` (plurals too).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().trim_end_matches('s') {
            "texture" => Some(Self::Texture),
            "hdri" => Some(Self::Hdri),
            "model" => Some(Self::Model),
            _ => None,
        }
    }
}

fn polyhaven_catalog(kind: PolyHaven) -> Result<&'static Value> {
    static CATALOGS: [OnceLock<Value>; 3] = [const { OnceLock::new() }; 3];
    let cell = &CATALOGS[kind as usize];
    if let Some(v) = cell.get() {
        return Ok(v);
    }
    let t = ["textures", "hdris", "models"][kind as usize];
    let v = get_json(&format!("https://api.polyhaven.com/assets?t={t}"))?;
    Ok(cell.get_or_init(|| v))
}

/// Poly Haven search over ids, names, tags and categories:
/// `<id> | name | categories | tags` lines.
pub fn polyhaven_search(kind: PolyHaven, q: &str) -> Result<Vec<String>> {
    let cat = polyhaven_catalog(kind)?;
    let words: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
    let mut hits: Vec<(usize, String)> = Vec::new();
    for (id, a) in cat.as_object().into_iter().flatten() {
        let list = |k: &str| a[k].as_array().into_iter().flatten().map(s).collect::<Vec<_>>().join(",");
        let (cats, tags) = (list("categories"), list("tags"));
        let hay = format!("{id} {} {cats} {tags}", s(&a["name"])).to_lowercase();
        let score = words.iter().filter(|w| hay.contains(w.as_str())).count();
        if score > 0 {
            hits.push((score, format!("{id} | {} | {cats} | {tags}", s(&a["name"]))));
        }
    }
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    Ok(hits.into_iter().take(25).map(|(_, l)| l).collect())
}

/// Downloads a Poly Haven asset at `res` (1k, 2k, ...; falls back to 1k) and returns
/// how to use it: a texture set `textures/<id>/{color,normal,rough}.jpg` returns `<id>`
/// (for `tex=`); an HDRI `hdri/<id>.hdr` and a model `models/<id>/<id>_<res>.gltf`
/// (with its bin and texture files at the relative paths it names) return that path.
/// Files already present are kept.
pub fn polyhaven_fetch(project: &Path, kind: PolyHaven, id: &str, res: &str) -> Result<String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        bail!("not a Poly Haven id: `{id}`");
    }
    let files = get_json(&format!("https://api.polyhaven.com/files/{id}"))?;
    let at_res = |v: &Value| -> Value { if v[res].is_object() { v[res].clone() } else { v["1k"].clone() } };
    let fetch = |url: &str, dest: &Path| -> Result<()> {
        if dest.exists() {
            return Ok(());
        }
        std::fs::create_dir_all(dest.parent().unwrap())?;
        // Partial downloads must not pass for finished files.
        let part = dest.with_extension("part");
        download(url, &part)?;
        Ok(std::fs::rename(&part, dest)?)
    };
    match kind {
        PolyHaven::Texture => {
            let dir = project.join("textures").join(id);
            for (map, file) in [("Diffuse", "color.jpg"), ("nor_gl", "normal.jpg"), ("Rough", "rough.jpg")] {
                let url = s(&at_res(&files[map])["jpg"]["url"]);
                ensure!(!url.is_empty(), "{id} has no {map} map (is it a texture?)");
                fetch(&url, &dir.join(file))?;
            }
            Ok(id.to_string())
        }
        PolyHaven::Hdri => {
            let url = s(&at_res(&files["hdri"])["hdr"]["url"]);
            ensure!(!url.is_empty(), "{id} has no .hdr (is it an HDRI?)");
            let src = format!("hdri/{id}.hdr");
            fetch(&url, &project.join(&src))?;
            Ok(src)
        }
        PolyHaven::Model => {
            let gltf = at_res(&files["gltf"])["gltf"].clone();
            let url = s(&gltf["url"]);
            ensure!(!url.is_empty(), "{id} has no glTF (is it a model?)");
            let dir = project.join("models").join(id);
            let name = url.rsplit('/').next().unwrap_or_default();
            ensure!(name.ends_with(".gltf"), "unexpected glTF url {url}");
            for (rel, f) in gltf["include"].as_object().into_iter().flatten() {
                let safe = Path::new(rel).components().all(|c| matches!(c, std::path::Component::Normal(_)));
                ensure!(safe, "{id}: unsafe include path {rel}");
                fetch(&s(&f["url"]), &dir.join(rel))?;
            }
            fetch(&url, &dir.join(name))?;
            Ok(format!("models/{id}/{name}"))
        }
    }
}
