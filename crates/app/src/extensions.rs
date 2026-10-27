//! "Plugins & Skills" page: the agent's live plugins (project + library) and the
//! Claude/omp skills enabled for this project.
//!
//! Plugins are disabled by moving their folder into `<root>/.disabled/<name>`;
//! plugin listing ignores folders without `plugin.json`, so the agent and the
//! renderer no longer see them. Skills are enabled per project by symlinking
//! `<project>/.omp/skills/<name>` to the skill's folder (omp discovers project
//! skills there); unticking removes only the symlink.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::app::Panels;

const VIDEO_SKILLS: &str = "https://github.com/zhuyansen/awesome-claude-video-skills";
const SKILL_REGISTRY: &str = "https://github.com/majiayu000/claude-skill-registry";
const RESCAN: Duration = Duration::from_secs(2);

pub(crate) fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// Where `Install from git URL` clones to.
pub(crate) fn skills_src() -> PathBuf {
    home().join(".editor/skills-src")
}

struct Plugin {
    name: String,
    kind: String,
    about: String,
    params: Vec<String>,
    dir: PathBuf,
    /// In the library (`~/.editor/plugins`) rather than the project.
    shared: bool,
    enabled: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// The skill's real folder (the one containing SKILL.md).
    pub dir: PathBuf,
    /// Where it was found, for display.
    pub origin: String,
    /// A real folder inside the project's `.omp/skills` (always on; never unlinked).
    pub local: bool,
}

#[derive(Default)]
pub struct ExtensionsPage {
    plugins: Vec<Plugin>,
    skills: Vec<Skill>,
    /// Project the lists were scanned for, and when.
    scanned: Option<(PathBuf, Instant)>,
    source: Option<(String, String)>,
    git_url: String,
    cloning: Option<Receiver<Result<PathBuf, String>>>,
    /// Last clone, so its skills can be highlighted.
    installed: Option<PathBuf>,
    catalog: crate::skill_catalog::Catalog,
}

impl ExtensionsPage {
    pub fn ui(&mut self, ui: &mut egui::Ui, panels: &mut Panels) {
        let project = panels.dir();
        if let Some(rx) = &self.cloning {
            match rx.try_recv() {
                Ok(Ok(dir)) => {
                    panels.notify(format!("cloned into {}", dir.display()));
                    self.installed = Some(dir);
                    self.cloning = None;
                    self.scanned = None;
                }
                Ok(Err(e)) => {
                    panels.notify(format!("git clone failed: {e}"));
                    self.cloning = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.cloning = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => ui.ctx().request_repaint_after(Duration::from_millis(200)),
            }
        }
        if self.scanned.as_ref().is_none_or(|(p, t)| *p != project || t.elapsed() > RESCAN) {
            self.rescan(&project);
        }
        ui.ctx().request_repaint_after(RESCAN);

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.heading("Plugins");
            ui.weak("Plugins add effects, generator layers and tools live, without rebuilding the editor. The agent writes them: ask it for what's missing (it uses `plugin action=help`).");
            ui.add_space(4.0);
            self.plugins_ui(ui, panels, &project);
            ui.add_space(12.0);
            ui.separator();
            ui.heading("Video skill catalog");
            ui.weak("Video skills from the awesome-claude-video-skills list and the claude-skill-registry. Install clones the repo and enables its skills in this project.");
            ui.add_space(4.0);
            if self.catalog.ui(ui, panels, &project) {
                self.scanned = None;
            }
            ui.add_space(12.0);
            ui.separator();
            ui.heading("Skills");
            ui.weak("Ticked skills are linked into this project's .omp/skills. The agent picks up changes on its next session/restart.");
            ui.add_space(4.0);
            self.skills_ui(ui, panels, &project);
            ui.add_space(12.0);
            ui.separator();
            ui.heading("Find more skills");
            self.registry_ui(ui, panels);
        });

        if let Some((name, text)) = &mut self.source {
            let mut open = true;
            egui::Window::new(format!("{name} (read-only)")).open(&mut open).default_size([640.0, 480.0]).show(ui.ctx(), |ui| {
                egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                    ui.add(egui::TextEdit::multiline(&mut text.as_str()).code_editor().desired_width(f32::INFINITY));
                });
            });
            if !open {
                self.source = None;
            }
        }
    }

    fn rescan(&mut self, project: &Path) {
        self.plugins = scan_plugins(project);
        self.skills = scan_skills(project);
        self.scanned = Some((project.to_path_buf(), Instant::now()));
        self.catalog.rescan();
    }

    fn plugins_ui(&mut self, ui: &mut egui::Ui, panels: &mut Panels, project: &Path) {
        if self.plugins.is_empty() {
            ui.label("No plugins yet.");
            return;
        }
        let mut changed = false;
        for p in &self.plugins {
            let error = if p.enabled { panels.shared.renderer.lock().plugin_error(&p.name) } else { None };
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    let name = egui::RichText::new(&p.name).strong();
                    ui.label(if p.enabled { name } else { name.weak().strikethrough() });
                    ui.weak(format!("{} · {}", p.kind, if p.shared { "library" } else { "project" }));
                    if !p.enabled {
                        ui.weak("(disabled)");
                    }
                });
                if !p.about.is_empty() {
                    ui.label(&p.about);
                }
                for (i, param) in p.params.iter().enumerate() {
                    let slot = ["x", "y", "z", "w"].get(i).copied().unwrap_or("·");
                    ui.weak(format!("  {slot}: {param}"));
                }
                if let Some(e) = error {
                    ui.colored_label(ui.visuals().error_fg_color, format!("render error: {e}"));
                }
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(p.enabled, egui::Button::new("View source")).clicked() {
                        match ed_mcp::plugins::show(project, &p.name) {
                            Ok(text) => self.source = Some((p.name.clone(), text)),
                            Err(e) => panels.notify(format!("{e:#}")),
                        }
                    }
                    let toggle = if p.enabled { "Disable" } else { "Enable" };
                    if ui.button(toggle).clicked() {
                        let root = plugin_root(&p.dir, p.enabled);
                        match set_plugin_enabled(&root, &p.name, !p.enabled) {
                            Ok(()) => panels.notify(format!("plugin {} {}", p.name, if p.enabled { "disabled" } else { "enabled" })),
                            Err(e) => panels.notify(format!("plugin {}: {e}", p.name)),
                        }
                        changed = true;
                    }
                    let (label, dest) = if p.shared {
                        ("Copy to project", Some(project.join("plugins")))
                    } else {
                        ("Copy to library", ed_render::plugins::library_dir())
                    };
                    if ui.add_enabled(p.enabled, egui::Button::new(label)).clicked() {
                        match dest.ok_or_else(|| "no home folder".to_string()).and_then(|d| copy_plugin(&p.dir, &d.join(&p.name))) {
                            Ok(()) => panels.notify(format!("copied plugin {}", p.name)),
                            Err(e) => panels.notify(format!("copy {}: {e}", p.name)),
                        }
                        changed = true;
                    }
                    if ui.button("Delete").on_hover_text("Moves the folder to the Trash").clicked() {
                        match trash(&p.dir) {
                            Ok(to) => panels.notify(format!("moved {} to {}", p.name, to.display())),
                            Err(e) => panels.notify(format!("delete {}: {e}", p.name)),
                        }
                        changed = true;
                    }
                    if ui.button("Show in Finder").clicked() {
                        open(&[std::ffi::OsStr::new("-R"), p.dir.as_os_str()]);
                    }
                });
            });
        }
        if changed {
            self.rescan(project);
        }
    }

    fn skills_ui(&mut self, ui: &mut egui::Ui, panels: &mut Panels, project: &Path) {
        if self.skills.is_empty() {
            ui.label("No skills found in ~/.claude/skills, ~/.omp/agent/skills, ~/.editor/skills-src or this project's .omp/skills.");
            return;
        }
        let links = project_skills(project);
        let mut changed = false;
        for s in &self.skills {
            let mut on = s.local || skill_enabled(&links, s);
            ui.horizontal(|ui| {
                let fresh = self.installed.as_ref().is_some_and(|d| s.dir.starts_with(d));
                let mut label = egui::RichText::new(&s.name).strong();
                if fresh {
                    label = label.color(ui.visuals().hyperlink_color);
                }
                let r = ui.add_enabled(!s.local, egui::Checkbox::new(&mut on, label));
                if r.changed() {
                    match set_skill(&links, &s.name, &s.dir, on) {
                        Ok(()) => panels.notify(format!("skill {} {} (agent picks it up on its next session)", s.name, if on { "enabled" } else { "disabled" })),
                        Err(e) => panels.notify(format!("skill {}: {e}", s.name)),
                    }
                    changed = true;
                }
                r.on_hover_text(s.dir.display().to_string());
                ui.weak(&s.origin);
            });
            if !s.description.is_empty() {
                ui.indent(&s.dir, |ui| ui.label(&s.description));
            }
        }
        if changed {
            self.rescan(project);
        }
    }

    fn registry_ui(&mut self, ui: &mut egui::Ui, panels: &mut Panels) {
        ui.horizontal_wrapped(|ui| {
            if ui.button("Awesome Claude video skills").on_hover_text(VIDEO_SKILLS).clicked() {
                open(&[std::ffi::OsStr::new(VIDEO_SKILLS)]);
            }
            if ui.button("Claude skill registry").on_hover_text(SKILL_REGISTRY).clicked() {
                open(&[std::ffi::OsStr::new(SKILL_REGISTRY)]);
            }
        });
        ui.horizontal(|ui| {
            ui.label("Install from git URL");
            ui.add(egui::TextEdit::singleline(&mut self.git_url).hint_text("https://github.com/owner/repo").desired_width(320.0));
            let busy = self.cloning.is_some();
            if busy {
                ui.spinner();
            }
            let go = ui.add_enabled(!busy && !self.git_url.trim().is_empty(), egui::Button::new("Install")).clicked();
            if go {
                let url = self.git_url.trim().to_string();
                match repo_name(&url) {
                    None => panels.notify("can't tell the repository name from that URL"),
                    Some(name) => {
                        let dest = skills_src().join(&name);
                        if dest.exists() {
                            panels.notify(format!("{} already exists; its skills are listed above", dest.display()));
                            self.installed = Some(dest);
                        } else {
                            let (tx, rx) = channel();
                            let ctx = ui.ctx().clone();
                            std::thread::spawn(move || {
                                let _ = tx.send(git_clone(&url, &dest));
                                ctx.request_repaint();
                            });
                            self.cloning = Some(rx);
                            self.git_url.clear();
                        }
                    }
                }
            }
        });
        ui.weak(format!("Clones into {}; tick the skills found inside it above.", skills_src().display()));
    }
}

fn open(args: &[&std::ffi::OsStr]) {
    let _ = std::process::Command::new("open").args(args).spawn();
}

pub(crate) fn git_clone(url: &str, dest: &Path) -> Result<PathBuf, String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let out = std::process::Command::new("git")
        .args(["clone", "--depth", "1", "--", url])
        .arg(dest)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(dest.to_path_buf())
}

/// Folder name for a clone: the URL's last segment without `.git`.
pub fn repo_name(url: &str) -> Option<String> {
    let last = url.trim().trim_end_matches('/').rsplit(['/', ':']).next()?;
    let name = last.strip_suffix(".git").unwrap_or(last);
    let ok = !name.is_empty() && name != "." && name != ".." && name.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    ok.then(|| name.to_string())
}

// ---------------------------------------------------------------- plugins

fn scan_plugins(project: &Path) -> Vec<Plugin> {
    let mut out: Vec<Plugin> = ed_mcp::plugins::list(project)
        .into_iter()
        .map(|l| Plugin {
            kind: kind_name(l.manifest.kind).into(),
            about: l.manifest.about,
            params: l.manifest.params,
            name: l.name,
            dir: l.dir,
            shared: l.shared,
            enabled: true,
        })
        .collect();
    for (root, shared) in [(Some(project.join("plugins")), false), (ed_render::plugins::library_dir(), true)] {
        let Some(root) = root else { continue };
        let Ok(entries) = std::fs::read_dir(root.join(".disabled")) else { continue };
        let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.join("plugin.json").is_file()).collect();
        dirs.sort();
        for dir in dirs {
            let name = dir.file_name().unwrap().to_string_lossy().into_owned();
            let manifest: Option<ed_mcp::plugins::Manifest> = std::fs::read(dir.join("plugin.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
            let (kind, about, params) = match manifest {
                Some(m) => (kind_name(m.kind).to_string(), m.about, m.params),
                None => ("?".into(), "unreadable plugin.json".into(), Vec::new()),
            };
            out.push(Plugin { name, kind, about, params, dir, shared, enabled: false });
        }
    }
    out
}

fn kind_name(k: ed_mcp::plugins::PluginKind) -> &'static str {
    match k {
        ed_mcp::plugins::PluginKind::Fx => "fx",
        ed_mcp::plugins::PluginKind::Layer => "layer",
        ed_mcp::plugins::PluginKind::Tool => "tool",
    }
}

/// The plugins root a plugin folder belongs to (`plugins/` or `~/.editor/plugins`).
fn plugin_root(dir: &Path, enabled: bool) -> PathBuf {
    let parent = dir.parent().unwrap_or(dir);
    if enabled { parent.to_path_buf() } else { parent.parent().unwrap_or(parent).to_path_buf() }
}

/// Disabling moves `<root>/<name>` to `<root>/.disabled/<name>`; enabling moves it back.
pub fn set_plugin_enabled(root: &Path, name: &str, on: bool) -> Result<(), String> {
    let live = root.join(name);
    let off = root.join(".disabled").join(name);
    let (from, to) = if on { (off, live) } else { (live, off) };
    if to.exists() {
        return Err(format!("{} already exists", to.display()));
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&from, &to).map_err(|e| e.to_string())
}

fn copy_plugin(from: &Path, to: &Path) -> Result<(), String> {
    if to.exists() {
        return Err(format!("{} already exists", to.display()));
    }
    copy_dir(from, to).map_err(|e| e.to_string())
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.flatten() {
        let p = e.path();
        let dest = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            if e.file_name() != "__pycache__" {
                copy_dir(&p, &dest)?;
            }
        } else {
            std::fs::copy(&p, &dest)?;
        }
    }
    Ok(())
}

/// Moves a folder to `~/.Trash` (never deletes outright).
fn trash(dir: &Path) -> Result<PathBuf, String> {
    let bin = home().join(".Trash");
    std::fs::create_dir_all(&bin).map_err(|e| e.to_string())?;
    let name = dir.file_name().ok_or("no folder name")?.to_string_lossy().into_owned();
    let mut to = bin.join(&name);
    let mut n = 1;
    while to.exists() {
        n += 1;
        to = bin.join(format!("{name} {n}"));
    }
    std::fs::rename(dir, &to).map_err(|e| e.to_string())?;
    Ok(to)
}

// ---------------------------------------------------------------- skills

/// The project's skill folder omp scans.
pub(crate) fn project_skills(project: &Path) -> PathBuf {
    project.join(".omp/skills")
}

fn scan_skills(project: &Path) -> Vec<Skill> {
    let mut out: Vec<Skill> = Vec::new();
    let links = project_skills(project);
    // Real folders in the project first, then the global sources.
    for dir in skill_dirs(&links, 1) {
        if !is_symlink(&dir) {
            push_skill(&mut out, &dir, "project", true);
        }
    }
    let h = home();
    for (root, origin, depth) in [
        (h.join(".claude/skills"), "~/.claude/skills", 2),
        (h.join(".omp/agent/skills"), "~/.omp/agent/skills", 2),
        (skills_src(), "~/.editor/skills-src", 3),
    ] {
        for dir in skill_dirs(&root, depth) {
            push_skill(&mut out, &dir, origin, false);
        }
    }
    // Links to skills found nowhere else still get a row (so they can be unticked).
    for dir in skill_dirs(&links, 1) {
        if is_symlink(&dir) {
            push_skill(&mut out, &dir, "linked", false);
        }
    }
    out
}

fn push_skill(out: &mut Vec<Skill>, dir: &Path, origin: &str, local: bool) {
    let real = if local { dir.to_path_buf() } else { std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()) };
    if out.iter().any(|s| s.dir == real) {
        return;
    }
    let text = std::fs::read_to_string(dir.join("SKILL.md")).unwrap_or_default();
    let (name, description) = frontmatter(&text);
    let folder = if origin == "linked" { dir } else { real.as_path() };
    let fallback = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let name = name.filter(|n| is_skill_name(n)).unwrap_or(fallback);
    out.push(Skill { name, description: description.unwrap_or_default(), dir: real, origin: origin.into(), local });
}

pub(crate) fn is_skill_name(n: &str) -> bool {
    !n.is_empty() && n != "." && n != ".." && n.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

/// Folders containing SKILL.md under `root`, up to `depth` levels down; does not
/// descend into a skill or hidden folders.
pub fn skill_dirs(root: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if root.join("SKILL.md").is_file() {
        out.push(root.to_path_buf());
        return out;
    }
    if depth == 0 {
        return out;
    }
    let Ok(entries) = std::fs::read_dir(root) else { return out };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.') && e.file_name() != "node_modules")
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for d in dirs {
        out.extend(skill_dirs(&d, depth - 1));
    }
    out
}

/// `name:` and `description:` from SKILL.md's YAML frontmatter (folded/literal
/// block scalars and quotes handled).
pub fn frontmatter(text: &str) -> (Option<String>, Option<String>) {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None);
    }
    let body: Vec<&str> = lines.take_while(|l| l.trim() != "---").collect();
    let field = |key: &str| -> Option<String> {
        let i = body.iter().position(|l| l.strip_prefix(key).is_some_and(|r| r.starts_with(':')))?;
        let first = body[i][key.len() + 1..].trim();
        let value = if first.is_empty() || first.starts_with('|') || first.starts_with('>') {
            let keep_lines = first.starts_with('|');
            let rest: Vec<&str> = body[i + 1..].iter().take_while(|l| l.is_empty() || l.starts_with([' ', '\t'])).map(|l| l.trim()).collect();
            rest.join(if keep_lines { "\n" } else { " " }).trim().to_string()
        } else {
            let q = first.chars().next().filter(|c| *c == '"' || *c == '\'');
            match q {
                Some(q) if first.len() >= 2 && first.ends_with(q) => first[1..first.len() - 1].to_string(),
                _ => first.to_string(),
            }
        };
        (!value.is_empty()).then_some(value)
    };
    (field("name"), field("description"))
}

/// Is `<links>/<name>` a symlink to this skill's folder?
fn skill_enabled(links: &Path, s: &Skill) -> bool {
    let link = links.join(&s.name);
    is_symlink(&link) && std::fs::canonicalize(&link).is_ok_and(|t| t == s.dir)
}

/// Ticking links `<links>/<name>` -> `src` (replacing another skill's link of the
/// same name); unticking removes the link. Real folders are never touched.
pub fn set_skill(links: &Path, name: &str, src: &Path, on: bool) -> Result<(), String> {
    if !is_skill_name(name) {
        return Err(format!("bad skill name `{name}`"));
    }
    let link = links.join(name);
    let exists = std::fs::symlink_metadata(&link).is_ok();
    if exists && !is_symlink(&link) {
        return Err(format!("{} is a real folder, not a link; leaving it alone", link.display()));
    }
    if exists {
        std::fs::remove_file(&link).map_err(|e| e.to_string())?;
    }
    if on {
        std::fs::create_dir_all(links).map_err(|e| e.to_string())?;
        std::os::unix::fs::symlink(src, &link).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let d = std::env::temp_dir().join(format!("ed-ext-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::canonicalize(&d).unwrap()
    }

    #[test]
    fn parses_frontmatter() {
        let (n, d) = frontmatter("---\nname: cut-to-beat\ndescription: \"Cuts on the beat.\"\n---\n# body");
        assert_eq!(n.as_deref(), Some("cut-to-beat"));
        assert_eq!(d.as_deref(), Some("Cuts on the beat."));
        let (n, d) = frontmatter("---\nname: x\ndescription: >\n  Folded\n  text.\nother: 1\n---\n");
        assert_eq!(n.as_deref(), Some("x"));
        assert_eq!(d.as_deref(), Some("Folded text."));
        let (_, d) = frontmatter("---\ndescription: it's: fine\n---");
        assert_eq!(d.as_deref(), Some("it's: fine"));
        assert_eq!(frontmatter("# no frontmatter\ndescription: nope"), (None, None));
    }

    #[test]
    fn repo_names() {
        assert_eq!(repo_name("https://github.com/a/b-skills.git").as_deref(), Some("b-skills"));
        assert_eq!(repo_name("git@github.com:a/repo/").as_deref(), Some("repo"));
        assert_eq!(repo_name("https://x/.."), None);
    }

    #[test]
    fn skill_links_toggle_and_spare_real_folders() {
        let root = temp("skills");
        let src = root.join("src/deep/my-skill");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("SKILL.md"), "---\nname: my-skill\ndescription: d\n---\n").unwrap();
        assert_eq!(skill_dirs(&root.join("src"), 3), vec![src.clone()]);
        assert!(skill_dirs(&root.join("src"), 1).is_empty());

        let links = root.join("project/.omp/skills");
        let skill = Skill { name: "my-skill".into(), description: "d".into(), dir: src.clone(), origin: String::new(), local: false };
        set_skill(&links, "my-skill", &src, true).unwrap();
        assert!(skill_enabled(&links, &skill));
        set_skill(&links, "my-skill", &src, false).unwrap();
        assert!(!skill_enabled(&links, &skill));
        assert!(!links.join("my-skill").exists());
        assert!(src.join("SKILL.md").is_file(), "source untouched");

        std::fs::create_dir_all(links.join("real")).unwrap();
        assert!(set_skill(&links, "real", &src, false).is_err());
        assert!(links.join("real").is_dir());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn plugin_disable_enable_moves_folder() {
        let root = temp("plugins");
        std::fs::create_dir_all(root.join("glow")).unwrap();
        std::fs::write(root.join("glow/plugin.json"), "{\"kind\":\"fx\"}").unwrap();
        set_plugin_enabled(&root, "glow", false).unwrap();
        assert!(!root.join("glow").exists());
        assert!(root.join(".disabled/glow/plugin.json").is_file());
        assert_eq!(plugin_root(&root.join(".disabled/glow"), false), root);
        set_plugin_enabled(&root, "glow", true).unwrap();
        assert!(root.join("glow/plugin.json").is_file());
        assert_eq!(plugin_root(&root.join("glow"), true), root);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
