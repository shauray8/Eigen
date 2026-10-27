//! Music library: search open catalogs (Openverse: Jamendo/Wikimedia/Freesound
//! music, ccMixter, Internet Archive netlabels) and local folders, then fetch a
//! track into the project.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct Track {
    /// `ov:<id>`, `cc:<id>`, `ia:<identifier>` or `local:<path>`.
    pub id: String,
    pub title: String,
    pub artist: String,
    pub seconds: Option<f32>,
    pub tags: String,
    pub source: String,
}

impl Track {
    pub fn line(&self) -> String {
        let dur = self.seconds.map_or("?".into(), |s| format!("{}:{:02}", (s / 60.0) as u32, (s % 60.0) as u32));
        format!("{} | {} — {} | {dur} | {} | {}", self.id, self.title, self.artist, self.tags, self.source)
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .user_agent("editor/0.1 (personal film tool)")
        .build()
        .into()
}

fn get_json(url: &str) -> Result<Value> {
    Ok(agent().get(url).call()?.body_mut().read_json::<Value>()?)
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

fn openverse(q: &str, n: usize) -> Result<Vec<Track>> {
    let d = get_json(&format!("https://api.openverse.org/v1/audio/?q={}&category=music&page_size={n}", enc(q)))?;
    Ok(d["results"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let mut tags: Vec<String> = r["genres"].as_array().into_iter().flatten().map(s).collect();
            tags.extend(r["tags"].as_array().into_iter().flatten().take(6).map(|t| s(&t["name"])));
            Track {
                id: format!("ov:{}", s(&r["id"])),
                title: s(&r["title"]),
                artist: s(&r["creator"]),
                seconds: r["duration"].as_f64().map(|ms| ms as f32 / 1000.0),
                tags: tags.join(","),
                source: format!("openverse/{}", s(&r["source"])),
            }
        })
        .collect())
}

fn mmss(t: &str) -> Option<f32> {
    let (m, s) = t.split_once(':')?;
    Some(m.parse::<f32>().ok()? * 60.0 + s.parse::<f32>().ok()?)
}

fn ccmixter(q: &str, n: usize) -> Result<Vec<Track>> {
    let tags = q.split_whitespace().collect::<Vec<_>>().join("+");
    let d = get_json(&format!("http://ccmixter.org/api/query?f=json&tags={}&type=any&limit={n}", enc(&tags).replace("%2B", "+")))?;
    Ok(d.as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let tags: Vec<&str> = r["upload_tags"]
                .as_str()
                .unwrap_or_default()
                .split(',')
                .filter(|t| !t.is_empty() && !matches!(*t, "media" | "audio" | "mp3" | "stereo" | "44k" | "48k" | "CBR" | "VBR"))
                .take(8)
                .collect();
            Track {
                id: format!("cc:{}", r["upload_id"].as_i64().unwrap_or_default()),
                title: s(&r["upload_name"]),
                artist: s(&r["user_name"]),
                seconds: r["files"][0]["file_format_info"]["ps"].as_str().and_then(mmss),
                tags: tags.join(","),
                source: "ccmixter".into(),
            }
        })
        .collect())
}

fn archive(q: &str, n: usize) -> Result<Vec<Track>> {
    let query = format!("({q}) AND mediatype:audio AND collection:(netlabels OR opensource_audio)");
    let d = get_json(&format!(
        "https://archive.org/advancedsearch.php?q={}&fl[]=identifier&fl[]=title&fl[]=creator&fl[]=subject&rows={n}&output=json",
        enc(&query)
    ))?;
    Ok(d["response"]["docs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let subject = match &r["subject"] {
                Value::Array(a) => a.iter().map(s).collect::<Vec<_>>().join(","),
                v => s(v),
            };
            Track {
                id: format!("ia:{}", s(&r["identifier"])),
                title: s(&r["title"]),
                artist: match &r["creator"] {
                    Value::Array(a) => a.first().map(s).unwrap_or_default(),
                    v => s(v),
                },
                seconds: None,
                tags: subject,
                source: "archive.org".into(),
            }
        })
        .collect())
}

const AUDIO_EXT: [&str; 7] = ["mp3", "wav", "flac", "m4a", "aac", "ogg", "aiff"];

fn local(q: &str, roots: &[PathBuf], n: usize) -> Vec<Track> {
    let words: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = roots.to_vec();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()).is_some_and(|x| AUDIO_EXT.contains(&x.to_lowercase().as_str())) {
                let hay = p.to_string_lossy().to_lowercase();
                if words.iter().all(|w| w == "*" || hay.contains(w)) {
                    out.push(Track {
                        id: format!("local:{}", p.display()),
                        title: p.file_stem().unwrap_or_default().to_string_lossy().into(),
                        artist: String::new(),
                        seconds: None,
                        tags: String::new(),
                        source: "local".into(),
                    });
                    if out.len() >= n {
                        return out;
                    }
                }
            }
        }
    }
    out
}

/// Searches every source; failures of one source don't hide the others.
pub fn search(q: &str, min_s: Option<f32>, max_s: Option<f32>, local_roots: &[PathBuf]) -> (Vec<Track>, Vec<String>) {
    let mut tracks = local(q, local_roots, 10);
    let mut errors = Vec::new();
    let sources: [(&str, fn(&str, usize) -> Result<Vec<Track>>); 3] =
        [("openverse", openverse), ("ccmixter", ccmixter), ("archive.org", archive)];
    let found: Vec<(&str, Result<Vec<Track>>)> = std::thread::scope(|sc| {
        let handles: Vec<_> = sources.iter().map(|(name, f)| (*name, sc.spawn(move || f(q, 12)))).collect();
        handles.into_iter().map(|(n, h)| (n, h.join().unwrap_or_else(|_| Err(anyhow!("panicked"))))).collect()
    });
    for (name, r) in found {
        match r {
            Ok(t) => tracks.extend(t),
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    tracks.retain(|t| match t.seconds {
        Some(sec) => min_s.is_none_or(|m| sec >= m) && max_s.is_none_or(|m| sec <= m),
        None => true,
    });
    (tracks, errors)
}

fn slug(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    out.chars().take(48).collect()
}

fn download(url: &str, dest: &Path) -> Result<()> {
    // Some hosts (ccMixter) only serve files to requests from their own pages.
    let origin = url.splitn(4, '/').take(3).collect::<Vec<_>>().join("/") + "/";
    let mut resp = agent().get(url).header("Referer", &origin).call().with_context(|| format!("downloading {url}"))?;
    let mut reader = resp.body_mut().with_config().limit(400 * 1024 * 1024).reader();
    let mut file = std::fs::File::create(dest)?;
    std::io::copy(&mut reader, &mut file)?;
    Ok(())
}

/// Fetches a track into `<project>/audio/` and returns its project-relative path.
pub fn fetch(id: &str, project: &Path) -> Result<String> {
    let dir = project.join("audio");
    std::fs::create_dir_all(&dir)?;
    let (kind, key) = id.split_once(':').ok_or_else(|| anyhow!("bad id `{id}`"))?;
    let (url, title, ext) = match kind {
        "ov" => {
            let r = get_json(&format!("https://api.openverse.org/v1/audio/{key}/"))?;
            let ft = r["filetype"].as_str().unwrap_or("mp3").to_lowercase();
            let ext = AUDIO_EXT.iter().find(|e| ft.starts_with(*e)).unwrap_or(&"mp3").to_string();
            (s(&r["url"]), s(&r["title"]), ext)
        }
        "cc" => {
            let d = get_json(&format!("http://ccmixter.org/api/query?f=json&ids={key}"))?;
            let r = &d[0];
            let f = r["files"].as_array().into_iter().flatten().find(|f| s(&f["download_url"]).ends_with(".mp3"));
            let f = f.ok_or_else(|| anyhow!("no mp3 for {id}"))?;
            (s(&f["download_url"]), s(&r["upload_name"]), "mp3".into())
        }
        "ia" => {
            let d = get_json(&format!("https://archive.org/metadata/{key}"))?;
            let f = d["files"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|f| s(&f["name"]).to_lowercase().ends_with(".mp3"))
                .ok_or_else(|| anyhow!("no mp3 in {id}"))?;
            let name = s(&f["name"]);
            let url = format!("https://archive.org/download/{key}/{}", enc(&name).replace('+', "%20").replace("%2F", "/"));
            (url, name.trim_end_matches(".mp3").to_string(), "mp3".into())
        }
        "local" => {
            let src = PathBuf::from(key);
            let name = format!("{}.{}", slug(&src.file_stem().unwrap_or_default().to_string_lossy()), src.extension().unwrap_or_default().to_string_lossy());
            std::fs::copy(&src, dir.join(&name))?;
            return Ok(format!("audio/{name}"));
        }
        _ => bail!("unknown source `{kind}`"),
    };
    let name = format!("{}.{}", slug(if title.is_empty() { key } else { &title }), ext);
    let dest = dir.join(&name);
    if !dest.exists() {
        download(&url, &dest)?;
    }
    Ok(format!("audio/{name}"))
}
