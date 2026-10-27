//! Moodboard page: folders of reference images shared by every project
//! (`~/.editor/moodboards`). Tick the folders this project follows; the agent
//! reads the active ones through its `moodboard` tool.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eframe::egui::{self, Color32, Pos2, Rect, Sense, TextureHandle, vec2};
use ed_mcp::moodboard as mb;

use crate::app::Panels;

const THUMB: egui::Vec2 = vec2(180.0, 135.0);
/// New thumbnails decoded per frame, so a big folder opens without a stall.
const LOADS_PER_FRAME: usize = 4;

#[derive(Default)]
pub struct Moodboard {
    selected: Option<String>,
    /// Folder being renamed and the name typed so far.
    renaming: Option<(String, String)>,
    thumbs: HashMap<PathBuf, Option<TextureHandle>>,
    /// The image shown large.
    large: Option<(PathBuf, Option<TextureHandle>)>,
    /// Folder waiting for a second click on "Move to Trash".
    confirm_trash: Option<String>,
}

fn texture(ctx: &egui::Context, path: &Path, max: u32) -> Option<TextureHandle> {
    let pic = mb::fit(&ed_media::load_image(path).ok()?, [max, max]);
    let img = egui::ColorImage::from_rgba_unmultiplied([pic.w as usize, pic.h as usize], &pic.rgba);
    Some(ctx.load_texture(path.display().to_string(), img, Default::default()))
}

/// `t` fitted and centred inside `rect`.
fn paint_fit(painter: &egui::Painter, t: &TextureHandle, rect: Rect) {
    let s = t.size_vec2();
    let k = (rect.width() / s.x).min(rect.height() / s.y);
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    painter.image(t.id(), Rect::from_center_size(rect.center(), s * k), uv, Color32::WHITE);
}

impl Moodboard {
    pub fn ui(&mut self, ui: &mut egui::Ui, panels: &mut Panels) {
        let root = mb::root();
        let project = panels.dir();
        let folders = mb::folders(&root);
        if self.selected.as_ref().is_none_or(|s| !folders.contains(s)) {
            self.selected = folders.first().cloned();
        }

        // Files dropped on the window land in the selected folder.
        let dropped = crate::pool::dropped(ui.ctx());
        if !dropped.is_empty() {
            let folder = match &self.selected {
                Some(f) => Ok(f.clone()),
                None => mb::create(&root, "moodboard"),
            };
            match folder {
                Ok(f) => self.add_files(panels, &root, &f, &dropped),
                Err(e) => panels.notify(format!("moodboard: {e:#}")),
            }
        }

        egui::Panel::left("moodboard-folders").resizable(true).default_size(240.0).size_range(160.0..=480.0).show(ui, |ui| {
            self.folder_list(ui, panels, &root, &project, &folders);
        });
        egui::CentralPanel::default().frame(egui::Frame::NONE.inner_margin(egui::Margin { left: 12, ..Default::default() })).show(ui, |ui| {
            match self.selected.clone() {
                Some(folder) => self.folder_view(ui, panels, &root, &project, &folder),
                None => {
                    ui.weak("No moodboards yet. Make a folder, then drop images on the window.");
                }
            }
        });
        self.large_view(ui.ctx());
    }

    fn folder_list(&mut self, ui: &mut egui::Ui, panels: &mut Panels, root: &Path, project: &Path, folders: &[String]) {
        ui.horizontal(|ui| {
            ui.heading("Moodboards");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("＋ New").on_hover_text("New folder (shared by every project)").clicked() {
                    match mb::create(root, "untitled") {
                        Ok(name) => {
                            self.renaming = Some((name.clone(), name.clone()));
                            self.selected = Some(name);
                        }
                        Err(e) => panels.notify(format!("moodboard: {e:#}")),
                    }
                }
            });
        });
        ui.weak("Tick the folders this project follows.");
        ui.separator();
        let active = mb::active(project);
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for f in folders {
                ui.horizontal(|ui| {
                    let mut on = active.contains(f);
                    if ui.checkbox(&mut on, "").on_hover_text("Active for this project").changed()
                        && let Err(e) = mb::set_active(project, f, on)
                    {
                        panels.notify(format!("moodboard: {e:#}"));
                    }
                    if let Some((old, name)) = self.renaming.as_mut().filter(|(old, _)| old == f) {
                        let edit = ui.text_edit_singleline(name);
                        if !edit.has_focus() && !edit.lost_focus() {
                            edit.request_focus();
                        }
                        if edit.lost_focus() {
                            let (old, name) = (old.clone(), name.trim().to_string());
                            self.renaming = None;
                            if ui.input(|i| !i.key_pressed(egui::Key::Escape)) {
                                match mb::rename(root, project, &old, &name) {
                                    Ok(()) => self.selected = Some(name),
                                    Err(e) => panels.notify(format!("moodboard: {e:#}")),
                                }
                            }
                        }
                    } else {
                        let label = format!("{f}  ({})", mb::images(root, f).len());
                        let r = ui.selectable_label(self.selected.as_ref() == Some(f), label);
                        if r.clicked() {
                            self.selected = Some(f.clone());
                        }
                        if r.double_clicked() {
                            self.renaming = Some((f.clone(), f.clone()));
                        }
                        r.context_menu(|ui| {
                            if ui.button("Rename").clicked() {
                                self.renaming = Some((f.clone(), f.clone()));
                                ui.close();
                            }
                            if ui.button("Move to Trash").clicked() {
                                self.trash_folder(panels, root, project, f);
                                ui.close();
                            }
                        });
                    }
                });
            }
        });
    }

    fn trash_folder(&mut self, panels: &mut Panels, root: &Path, project: &Path, folder: &str) {
        self.confirm_trash = None;
        let _ = mb::set_active(project, folder, false);
        match mb::trash(root, folder) {
            Ok(()) => {
                self.thumbs.retain(|p, _| !p.starts_with(root.join(folder)));
                panels.notify(format!("moved moodboard `{folder}` to the Trash"));
            }
            Err(e) => panels.notify(format!("moodboard: {e:#}")),
        }
    }

    fn add_files(&mut self, panels: &mut Panels, root: &Path, folder: &str, files: &[PathBuf]) {
        let mut added = 0;
        for f in files {
            match mb::add(root, folder, &f.to_string_lossy()) {
                Ok(_) => added += 1,
                Err(e) => panels.notify(format!("moodboard: {e:#}")),
            }
        }
        if added > 0 {
            panels.notify(format!("added {added} image{} to `{folder}`", if added == 1 { "" } else { "s" }));
        }
    }

    fn folder_view(&mut self, ui: &mut egui::Ui, panels: &mut Panels, root: &Path, project: &Path, folder: &str) {
        let images = mb::images(root, folder);
        ui.horizontal(|ui| {
            ui.heading(folder);
            let mut on = mb::active(project).iter().any(|f| f == folder);
            if ui.checkbox(&mut on, "Active").changed()
                && let Err(e) = mb::set_active(project, folder, on)
            {
                panels.notify(format!("moodboard: {e:#}"));
            }
            if ui.button("Add…").on_hover_text("Copy images in (or drop them on the window)").clicked()
                && let Some(files) = rfd::FileDialog::new().add_filter("images", &mb::EXTS).pick_files()
            {
                self.add_files(panels, root, folder, &files);
            }
            if ui.button("Send to agent").on_hover_text("Put this moodboard in the agent's prompt").clicked() {
                let _ = mb::set_active(project, folder, true);
                panels.attach(&format!("Follow moodboard `{folder}`"));
            }
            if ui.button("Rename").clicked() {
                self.renaming = Some((folder.to_string(), folder.to_string()));
            }
            if self.confirm_trash.as_deref() == Some(folder) {
                if ui.button(egui::RichText::new("Really move to Trash?").color(Color32::LIGHT_RED)).clicked() {
                    self.trash_folder(panels, root, project, folder);
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_trash = None;
                }
            } else if ui.button("Move to Trash").clicked() {
                self.confirm_trash = Some(folder.to_string());
            }
        });
        ui.separator();
        if images.is_empty() {
            ui.weak("Empty. Drop images on the window or use Add…");
            return;
        }
        let mut loads = 0;
        let mut remove = None;
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                for path in &images {
                    if !self.thumbs.contains_key(path) {
                        if loads >= LOADS_PER_FRAME {
                            ui.ctx().request_repaint();
                        } else {
                            loads += 1;
                            self.thumbs.insert(path.clone(), texture(ui.ctx(), path, 360));
                        }
                    }
                    let (rect, r) = ui.allocate_exact_size(THUMB, Sense::click());
                    let painter = ui.painter_at(rect);
                    painter.rect_filled(rect, 3.0, Color32::from_gray(24));
                    if let Some(Some(t)) = self.thumbs.get(path) {
                        paint_fit(&painter, t, rect.shrink(2.0));
                    }
                    if r.hovered() {
                        painter.rect_stroke(rect, 3.0, egui::Stroke::new(1.5, ui.visuals().selection.stroke.color), egui::StrokeKind::Inside);
                    }
                    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                    let r = r.on_hover_text(&name);
                    if r.clicked() {
                        self.large = Some((path.clone(), None));
                    }
                    r.context_menu(|ui| {
                        if ui.button("View").clicked() {
                            self.large = Some((path.clone(), None));
                            ui.close();
                        }
                        if ui.button("Remove (to Trash)").clicked() {
                            remove = Some(path.clone());
                            ui.close();
                        }
                    });
                }
            });
        });
        if let Some(path) = remove {
            match mb::trash_path(&path) {
                Ok(()) => {
                    self.thumbs.remove(&path);
                }
                Err(e) => panels.notify(format!("moodboard: {e:#}")),
            }
        }
    }

    fn large_view(&mut self, ctx: &egui::Context) {
        let Some((path, tex)) = &mut self.large else { return };
        if tex.is_none() {
            *tex = texture(ctx, path, 2048);
        }
        let screen = ctx.content_rect();
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("moodboard-large")).show(ctx, |ui| {
            let size = vec2(screen.width() * 0.85, screen.height() * 0.85);
            ui.label(path.file_name().unwrap_or_default().to_string_lossy());
            let (rect, r) = ui.allocate_exact_size(size, Sense::click());
            match tex {
                Some(t) => paint_fit(ui.painter(), t, rect),
                None => {
                    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "cannot read this image", egui::FontId::default(), Color32::GRAY);
                }
            }
            close = r.clicked();
        });
        if close || modal.should_close() {
            self.large = None;
        }
    }
}
