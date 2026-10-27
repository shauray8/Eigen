//! Projects, like an NLE's project manager: a library folder of projects
//! (`~/Movies/Editor Projects`, or `$ED_PROJECTS`), plus any folder opened
//! before (recent list in `~/.editor/recent.json`). The app opens on the
//! Project Manager; File ▸ Project Manager (⌘⇧O) brings it back to switch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use eframe::egui::{self, Color32, TextureHandle, vec2};

/// Where new projects are created.
pub fn library_root() -> PathBuf {
    std::env::var_os("ED_PROJECTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join("Movies/Editor Projects"))
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

fn recent_file() -> PathBuf {
    home().join(".editor/recent.json")
}

/// Folders opened before, most recent first.
pub fn recent() -> Vec<PathBuf> {
    std::fs::read(recent_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_recent(list: &[PathBuf]) {
    let f = recent_file();
    let _ = std::fs::create_dir_all(f.parent().unwrap());
    if let Ok(json) = serde_json::to_vec_pretty(list) {
        let _ = std::fs::write(f, json);
    }
}

/// Puts `dir` at the top of the recent list.
pub fn touch(dir: &Path) {
    let mut list = recent();
    list.retain(|p| p != dir);
    list.insert(0, dir.to_path_buf());
    list.truncate(40);
    save_recent(&list);
}

fn forget(dir: &Path) {
    let mut list = recent();
    list.retain(|p| p != dir);
    save_recent(&list);
}

/// Where a project keeps the picture the manager shows for it.
pub fn thumb_path(dir: &Path) -> PathBuf {
    dir.join(".editor/thumb.png")
}

pub struct Project {
    pub name: String,
    pub dir: PathBuf,
    pub modified: Option<SystemTime>,
}

/// Every known project: recent ones first, then the rest of the library by date.
pub fn list() -> Vec<Project> {
    let info = |dir: PathBuf| {
        let modified = std::fs::metadata(dir.join("scene.ops")).or_else(|_| std::fs::metadata(&dir)).and_then(|m| m.modified()).ok();
        Project { name: dir.file_name().map_or_else(|| dir.display().to_string(), |n| n.to_string_lossy().into_owned()), dir, modified }
    };
    let mut out: Vec<Project> = recent().into_iter().filter(|d| d.is_dir()).map(info).collect();
    let mut rest: Vec<Project> = std::fs::read_dir(library_root())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
        .filter(|p| !out.iter().any(|o| &o.dir == p))
        .map(info)
        .collect();
    rest.sort_by(|a, b| b.modified.cmp(&a.modified));
    out.extend(rest);
    out
}

/// A new, empty project folder in the library.
pub fn create(name: &str) -> anyhow::Result<PathBuf> {
    let name = name.trim();
    anyhow::ensure!(!name.is_empty() && !name.contains('/') && !name.starts_with('.'), "pick a name without `/`");
    let dir = library_root().join(name);
    anyhow::ensure!(!dir.exists(), "a project named `{name}` already exists");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.canonicalize()?)
}

fn ago(t: Option<SystemTime>) -> String {
    let Some(secs) = t.and_then(|t| t.elapsed().ok()).map(|d| d.as_secs()) else { return String::new() };
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86400),
    }
}

/// What the manager asks the app to do.
pub enum Choice {
    Open(PathBuf),
}

#[derive(Default)]
pub struct Manager {
    pub open: bool,
    new_name: String,
    selected: Option<PathBuf>,
    rename: Option<(PathBuf, String)>,
    filter: String,
    error: Option<String>,
    thumbs: HashMap<PathBuf, Option<TextureHandle>>,
    /// The project list, re-read when the manager opens.
    projects: Option<Vec<Project>>,
}

impl Manager {
    pub fn show(&mut self) {
        self.open = true;
        self.projects = None;
        self.thumbs.clear();
        self.error = None;
    }

    fn thumb(&mut self, ctx: &egui::Context, dir: &Path) -> Option<TextureHandle> {
        self.thumbs
            .entry(dir.to_path_buf())
            .or_insert_with(|| {
                let pic = ed_media::load_image(&thumb_path(dir)).ok()?;
                let img = egui::ColorImage::from_rgba_unmultiplied([pic.w as usize, pic.h as usize], &pic.rgba);
                Some(ctx.load_texture(dir.display().to_string(), img, Default::default()))
            })
            .clone()
    }

    /// Draws the manager over everything when open. `current`: the open project, if any.
    pub fn ui(&mut self, ctx: &egui::Context, current: Option<&Path>) -> Option<Choice> {
        if !self.open {
            return None;
        }
        let projects = self.projects.get_or_insert_with(list).iter().map(|p| (p.name.clone(), p.dir.clone(), p.modified)).collect::<Vec<_>>();
        let mut choice = None;
        let screen = ctx.content_rect();
        let modal = egui::Modal::new(egui::Id::new("project-manager")).show(ctx, |ui| {
            ui.set_width((screen.width() * 0.8).min(1100.0));
            ui.set_height((screen.height() * 0.8).min(760.0));
            ui.horizontal(|ui| {
                ui.heading("Projects");
                ui.weak(library_root().display().to_string());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if current.is_some() && ui.button("Close").clicked() {
                        self.open = false;
                    }
                    ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("filter").desired_width(160.0));
                });
            });
            ui.separator();
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut self.new_name).hint_text("New project name").desired_width(240.0));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("＋ New Project").clicked() || enter) && !self.new_name.trim().is_empty() {
                    match create(&self.new_name) {
                        Ok(dir) => {
                            self.new_name.clear();
                            choice = Some(Choice::Open(dir));
                        }
                        Err(e) => self.error = Some(format!("{e:#}")),
                    }
                }
                if ui.button("Open Folder…").on_hover_text("Any folder becomes a project").clicked()
                    && let Some(dir) = rfd::FileDialog::new().set_directory(library_root()).pick_folder()
                {
                    choice = Some(Choice::Open(dir));
                }
                if let Some(e) = &self.error {
                    ui.colored_label(Color32::LIGHT_RED, e);
                }
            });
            ui.add_space(6.0);

            let filter = self.filter.to_lowercase();
            egui::ScrollArea::vertical().auto_shrink([false, false]).max_height(ui.available_height() - 40.0).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(14.0, 14.0);
                    for (name, dir, modified) in projects.iter().filter(|(n, _, _)| n.to_lowercase().contains(&filter)) {
                        let is_sel = self.selected.as_ref() == Some(dir);
                        let is_current = current == Some(dir.as_path());
                        let thumb = self.thumb(ui.ctx(), dir);
                        let (rect, resp) = ui.allocate_exact_size(vec2(200.0, 150.0), egui::Sense::click());
                        let p = ui.painter_at(rect);
                        let img = egui::Rect::from_min_size(rect.min, vec2(200.0, 112.0));
                        p.rect_filled(img, 4.0, Color32::from_gray(30));
                        match thumb {
                            Some(t) => {
                                let s = t.size_vec2();
                                let k = (img.width() / s.x).min(img.height() / s.y);
                                let r = egui::Rect::from_center_size(img.center(), s * k);
                                p.image(t.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                            }
                            None => {
                                p.text(img.center(), egui::Align2::CENTER_CENTER, "🎞", egui::FontId::proportional(32.0), Color32::from_gray(90));
                            }
                        }
                        let border = if is_sel { Color32::from_rgb(255, 170, 60) } else if resp.hovered() { Color32::from_gray(120) } else { Color32::from_gray(50) };
                        p.rect_stroke(img, 4.0, egui::Stroke::new(if is_sel { 2.0 } else { 1.0 }, border), egui::StrokeKind::Outside);
                        let title = if is_current { format!("● {name}") } else { name.clone() };
                        p.text(img.left_bottom() + vec2(2.0, 6.0), egui::Align2::LEFT_TOP, title, egui::FontId::proportional(14.0), Color32::WHITE);
                        p.text(img.left_bottom() + vec2(2.0, 24.0), egui::Align2::LEFT_TOP, ago(*modified), egui::FontId::proportional(11.0), Color32::GRAY);
                        if resp.clicked() {
                            self.selected = Some(dir.clone());
                        }
                        if resp.double_clicked() {
                            choice = Some(Choice::Open(dir.clone()));
                        }
                        resp.on_hover_text(dir.display().to_string());
                    }
                });
            });

            // Actions on the selected project.
            ui.separator();
            ui.horizontal(|ui| {
                let Some(sel) = self.selected.clone() else {
                    ui.weak("Double-click a project to open it.");
                    return;
                };
                if ui.button("Open").clicked() {
                    choice = Some(Choice::Open(sel.clone()));
                }
                match &mut self.rename {
                    Some((dir, name)) if *dir == sel => {
                        ui.add(egui::TextEdit::singleline(name).desired_width(200.0));
                        if ui.button("Rename").clicked() {
                            let to = sel.with_file_name(name.trim());
                            if current == Some(sel.as_path()) {
                                self.error = Some("close the project before renaming it".into());
                            } else if name.trim().is_empty() || to.exists() {
                                self.error = Some("pick a new, unused name".into());
                            } else if let Err(e) = std::fs::rename(&sel, &to) {
                                self.error = Some(e.to_string());
                            } else {
                                forget(&sel);
                                touch(&to);
                                self.selected = Some(to);
                                self.projects = None;
                            }
                            self.rename = None;
                        }
                    }
                    _ => {
                        if ui.button("Rename…").clicked() {
                            let name = sel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            self.rename = Some((sel.clone(), name));
                        }
                    }
                }
                if ui.button("Show in Finder").clicked() {
                    let _ = std::process::Command::new("open").arg("-R").arg(&sel).spawn();
                }
                if ui.button("Remove from list").on_hover_text("Keeps the folder on disk").clicked() {
                    forget(&sel);
                    self.selected = None;
                    self.projects = None;
                }
                ui.weak(sel.display().to_string());
            });
        });
        // Clicking outside never dismisses it: a project must be open to work.
        if modal.should_close() && current.is_some() {
            self.open = false;
        }
        if choice.is_some() {
            self.open = false;
        }
        choice
    }
}
