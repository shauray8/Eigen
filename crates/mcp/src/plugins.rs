//! Plugins the agent writes while the editor runs, so a missing capability
//! never needs an editor rebuild. Each is a folder `plugins/<name>/` in the
//! project (or the shared library `~/.editor/plugins/<name>/`) with a
//! `plugin.json` manifest:
//!
//! - `fx`: `shader.wgsl` defining `effect(px, src)`; used by `add fx x fx=<name>`.
//! - `layer`: `shader.wgsl` defining `shade(uv, px)`; used by `add shader x src=<name>`.
//! - `tool`: a program (`run`, e.g. `python3 run.py`) that gets JSON on stdin and
//!   prints `{"ops": "...", "text": "..."}`; its ops are applied as the agent's.
//!   Tools run sandboxed: they can write only inside the project, their own
//!   folder and temp/cache folders.

use std::collections::BTreeMap;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ed_render::plugins::{Use, check, library_dir};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    Fx,
    Layer,
    Tool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub kind: PluginKind,
    /// One line: what it does.
    #[serde(default)]
    pub about: String,
    /// What `params.x/y/z/w` mean (shaders) or which `args` it takes (tools).
    #[serde(default)]
    pub params: Vec<String>,
    /// Tools: the command to run in the plugin folder, e.g. `python3 run.py`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
}

pub const HELP: &str = r#"Plugins add what the editor lacks, live (no rebuild). One `plugin` call writes and checks one; then use it.
fx (adjustment layer): files {"shader.wgsl": WGSL defining `fn effect(px: vec2f, src: vec4f) -> vec4f`}; use: add fx x fx=<name> params=a,b,c,d amount cell color
  px = pixel (target px), src = canvas beneath at px (linear, premultiplied). Given: at(px) samples the canvas anywhere,
  hash(p) random 0..1, fx.res, fx.cell (px), fx.color, fx.params, fx.seconds (since the layer's in), fx.frame. Output is mixed by amount.
layer (generator): files {"shader.wgsl": WGSL defining `fn shade(uv: vec2f, px: vec2f) -> vec4f`}; use: add shader x src=<name> params=... (full frame; loc scale rot opacity z in out)
  uv 0..1 over the layer box (y down), px in box pixels; given: layer.size, layer.seconds, layer.frame, layer.color, layer.params, hash(p). Return straight RGBA.
tool (anything a program can do: generators, rigs, point clouds, analysis, local AI): files incl. the program, run="python3 run.py".
  stdin: {"args":{...}, "project": dir, "frame", "comp", "selected", "res":[w,h], "fps"}; stdout: {"ops": "op script", "text": "note"} (or plain text).
  Write outputs into the project (media/, audio/, scene/...). Sandboxed: writes only in the project, the plugin folder, temp and caches.
  Long jobs: print `progress: 2/5 <what>` lines to stderr as each piece lands (the human sees them live; media/ and audio/ files show in the pool at once; write each file under a temp name, then rename).
params: up to 4 one-line descriptions (what params.x..w mean, or the tool's args). scope=library shares it with every project."#;

/// Where plugins are written: the project's, or the shared library.
fn base(project: &Path, library: bool) -> Result<PathBuf> {
    if library { library_dir().context("no home folder for the plugin library") } else { Ok(project.join("plugins")) }
}

pub struct Listed {
    pub name: String,
    pub manifest: Manifest,
    pub dir: PathBuf,
    pub shared: bool,
}

/// Every plugin, project ones first (a project plugin hides a library one of the same name).
pub fn list(project: &Path) -> Vec<Listed> {
    let mut out: Vec<Listed> = Vec::new();
    for (root, shared) in [(Some(project.join("plugins")), false), (library_dir(), true)] {
        let Some(entries) = root.and_then(|r| std::fs::read_dir(r).ok()) else { continue };
        let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.join("plugin.json").is_file()).collect();
        dirs.sort();
        for dir in dirs {
            let name = dir.file_name().unwrap().to_string_lossy().into_owned();
            if out.iter().any(|l| l.name == name) {
                continue;
            }
            if let Ok(manifest) = std::fs::read(dir.join("plugin.json")).map_err(anyhow::Error::from).and_then(|b| Ok(serde_json::from_slice(&b)?)) {
                out.push(Listed { name, manifest, dir, shared });
            }
        }
    }
    out
}

/// Writes plugin `name` and checks it; shader errors name the shader's own lines.
pub fn write(project: &Path, name: &str, manifest: &Manifest, files: &BTreeMap<String, String>, library: bool) -> Result<PathBuf> {
    if !ed_core::paths::is_plugin_name(name) {
        bail!("plugin names use letters, digits, _ and -");
    }
    for f in files.keys() {
        if f.is_empty() || f.contains("..") || f.starts_with('/') {
            bail!("file `{f}` must be a relative path inside the plugin");
        }
    }
    let usage = match manifest.kind {
        PluginKind::Fx => Some(Use::Fx),
        PluginKind::Layer => Some(Use::Layer),
        PluginKind::Tool => None,
    };
    let dir = base(project, library)?.join(name);
    if let Some(usage) = usage {
        let src = match files.get("shader.wgsl") {
            Some(s) => s.clone(),
            None => std::fs::read_to_string(dir.join("shader.wgsl")).context("a shader plugin needs files[\"shader.wgsl\"]")?,
        };
        check(usage, &src).map_err(|e| anyhow::anyhow!("shader.wgsl does not compile (nothing written):\n{e}"))?;
    } else if manifest.run.as_deref().is_none_or(|r| r.trim().is_empty()) {
        bail!("a tool plugin needs run, e.g. \"python3 run.py\"");
    }
    std::fs::create_dir_all(&dir)?;
    for (f, content) in files {
        let path = dir.join(f);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, content)?;
    }
    std::fs::write(dir.join("plugin.json"), serde_json::to_string_pretty(manifest)?)?;
    Ok(dir)
}

/// Text files of a plugin, for reading back and editing.
pub fn show(project: &Path, name: &str) -> Result<String> {
    let p = list(project).into_iter().find(|l| l.name == name).with_context(|| format!("no plugin `{name}`"))?;
    let mut out = format!("// {} ({})\n", p.dir.display(), if p.shared { "library" } else { "project" });
    let mut files: Vec<PathBuf> = walk(&p.dir);
    files.sort();
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        if text.len() > 64 * 1024 {
            continue;
        }
        out += &format!("\n=== {} ===\n{text}\n", f.strip_prefix(&p.dir).unwrap().display());
    }
    Ok(out)
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        let hidden = p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.') || n == "__pycache__");
        if hidden {
            continue;
        }
        if p.is_dir() { out.extend(walk(&p)) } else { out.push(p) }
    }
    out
}

/// What a tool plugin printed.
#[derive(Default, Deserialize)]
pub struct ToolOutput {
    #[serde(default)]
    pub ops: String,
    #[serde(default)]
    pub text: String,
}

/// macOS sandbox for tool plugins: everything allowed but writing outside
/// the project, the plugin's folder, temp and caches, or reading secrets.
fn sandbox_profile(project: &Path, plugin: &Path) -> String {
    let mut write = vec![project.to_path_buf(), plugin.to_path_buf()];
    write.extend(crate::sandbox::scratch());
    crate::sandbox::profile(&write, &[], &crate::sandbox::secrets())
}

/// Runs tool plugin `name` with `input` on stdin, sandboxed, for at most
/// `timeout`. Each stderr line `progress: <text>` goes to `progress` as the
/// tool prints it.
pub fn run(project: &Path, name: &str, input: &serde_json::Value, timeout: Duration, progress: &mut dyn FnMut(&str)) -> Result<ToolOutput> {
    let p = list(project).into_iter().find(|l| l.name == name).with_context(|| format!("no plugin `{name}`"))?;
    if p.manifest.kind != PluginKind::Tool {
        bail!("`{name}` is a shader plugin; use it in ops (fx=/src=), not run");
    }
    let command = p.manifest.run.clone().context("tool has no run command")?;
    let project = project.canonicalize()?;
    let dir = p.dir.canonicalize()?;
    let mut cmd = if crate::sandbox::available() {
        let mut c = std::process::Command::new(crate::sandbox::SANDBOX_EXEC);
        c.arg("-p").arg(sandbox_profile(&project, &dir)).args(["/bin/zsh", "-lc", &command]);
        c
    } else {
        let mut c = std::process::Command::new("/bin/sh");
        c.args(["-lc", &command]);
        c
    };
    let mut child = cmd
        .current_dir(&dir)
        .env("EDITOR_PROJECT", &project)
        .env("EDITOR_PLUGIN", &dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("starting `{command}`"))?;
    child.stdin.take().unwrap().write_all(serde_json::to_string(input)?.as_bytes())?;
    // Read both pipes on threads so a chatty tool never blocks on a full pipe.
    let mut out_pipe = child.stdout.take().unwrap();
    let err_pipe = child.stderr.take().unwrap();
    let out_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out_pipe.read_to_string(&mut s);
        s
    });
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let err_t = std::thread::spawn(move || {
        let mut s = String::new();
        for line in std::io::BufReader::new(err_pipe).lines().map_while(Result::ok) {
            if let Some(p) = line.strip_prefix("progress:") {
                let _ = tx.send(p.trim().to_string());
            } else {
                s.push_str(&line);
                s.push('\n');
            }
        }
        s
    });
    let start = Instant::now();
    let status = loop {
        while let Ok(p) = rx.try_recv() {
            progress(&p);
        }
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            bail!("`{name}` ran past {}s and was stopped", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();
    for p in rx.try_iter() {
        progress(&p);
    }
    if !status.success() {
        let tail: String = stderr.lines().rev().take(20).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
        bail!("`{name}` failed ({status}):\n{tail}");
    }
    let trimmed = stdout.trim();
    Ok(match serde_json::from_str::<ToolOutput>(trimmed) {
        Ok(o) if trimmed.starts_with('{') => o,
        _ => ToolOutput { ops: String::new(), text: trimmed.to_string() },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_run_sandboxed_and_return_ops() {
        let project = std::env::temp_dir().join(format!("ed_tool_{}", std::process::id()));
        std::fs::create_dir_all(&project).unwrap();
        let tool = Manifest { kind: PluginKind::Tool, about: "dots".into(), params: vec![], run: Some("sh run.sh".into()) };
        let script = "read input\necho 'progress: 1/2 half' >&2\necho 'chatter' >&2\necho made > \"$EDITOR_PROJECT/made.txt\"\necho 'progress: 2/2 all' >&2\necho '{\"ops\": \"add rect r\", \"text\": \"ok\"}'\n";
        write(&project, "dots", &tool, &BTreeMap::from([("run.sh".into(), script.into())]), false).unwrap();
        let mut seen = Vec::new();
        let out = run(&project, "dots", &serde_json::json!({"args": {}}), Duration::from_secs(20), &mut |p| seen.push(p.to_string())).unwrap();
        assert_eq!(seen, ["1/2 half", "2/2 all"], "progress lines arrive in order");
        assert_eq!((out.ops.as_str(), out.text.as_str()), ("add rect r", "ok"));
        assert!(project.join("made.txt").exists(), "tools may write in the project");

        if cfg!(target_os = "macos") {
            let escape = PathBuf::from(std::env::var("HOME").unwrap()).join(format!("ed_escape_{}", std::process::id()));
            let bad = format!("touch {} && echo '{{}}'\n", escape.display());
            write(&project, "bad", &Manifest { run: Some("sh run.sh".into()), ..tool.clone() }, &BTreeMap::from([("run.sh".into(), bad)]), false).unwrap();
            assert!(run(&project, "bad", &serde_json::json!({}), Duration::from_secs(20), &mut |_| {}).is_err(), "writes outside are denied");
            assert!(!escape.exists());
        }

        let broken = Manifest { kind: PluginKind::Fx, about: String::new(), params: vec![], run: None };
        let err = write(&project, "glow", &broken, &BTreeMap::from([("shader.wgsl".into(), "fn effect(px: vec2f, src: vec4f) -> vec4f { return x; }".into())]), false)
            .unwrap_err();
        assert!(err.to_string().contains("does not compile") && !project.join("plugins/glow").exists());
        std::fs::remove_dir_all(&project).unwrap();
    }
}
