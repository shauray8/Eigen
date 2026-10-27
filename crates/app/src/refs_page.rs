//! References page: every ref and review note of the project, kept with a
//! snapshot of the frame as it looked when it was made
//! (`<project>/.editor/refs/<id>.png`). Send one to the agent again, jump to
//! it, resolve or reopen notes, delete.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eframe::egui::{self, Color32, RichText, TextureHandle, vec2};
use ed_mcp::Shared;
use ed_mcp::refs::{Author, Ref, Target};

use crate::app::Panels;

const THUMB_W: u32 = 320;

fn thumb_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(".editor/refs").join(format!("{id}.png"))
}

/// Renders the frame a ref points at, with its mark drawn, and saves it as
/// the ref's snapshot. Returns the pixels and size.
pub fn snapshot(shared: &Shared, r: &Ref) -> Option<(Vec<u8>, [u32; 2])> {
    let (scene, res, marks) = {
        let s = shared.session.lock();
        let doc = s.doc();
        let comp = r.comp.as_deref().filter(|c| doc.objects.contains_key(*c));
        let f = r.frames[0];
        (ed_core::eval::evaluate_comp(doc, comp, f), doc.comp_canvas(comp).0, ed_mcp::refs::marks(r, doc, f))
    };
    let size = [THUMB_W, ((THUMB_W as f32 * res[1] as f32 / res[0].max(1) as f32).round() as u32).max(1)];
    let mut px = shared.renderer.lock().render_rgba(&scene, size).ok()?;
    ed_mcp::refs::draw(&mut px, size, res, &marks);
    let path = thumb_path(&shared.dir(), &r.id);
    let _ = std::fs::create_dir_all(path.parent()?);
    if let Ok(png) = ed_render::encode_png(&px, size) {
        let _ = std::fs::write(path, png);
    }
    Some((px, size))
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Filter {
    #[default]
    All,
    Pointers,
    Open,
    Resolved,
}

#[derive(Default)]
pub struct RefsPage {
    filter: Filter,
    search: String,
    thumbs: HashMap<String, TextureHandle>,
    /// The project the cached thumbnails belong to.
    project: Option<PathBuf>,
}

impl RefsPage {
    fn thumb(&mut self, ctx: &egui::Context, shared: &Shared, r: &Ref, budget: &mut u32) -> Option<TextureHandle> {
        if let Some(t) = self.thumbs.get(&r.id) {
            return Some(t.clone());
        }
        let path = thumb_path(&shared.dir(), &r.id);
        let (px, size) = match ed_media::load_image(&path) {
            Ok(p) => (p.rgba, [p.w, p.h]),
            // Missing (e.g. a note the director posted): render it now, a few per frame.
            Err(_) if *budget > 0 => {
                *budget -= 1;
                snapshot(shared, r)?
            }
            Err(_) => {
                ctx.request_repaint();
                return None;
            }
        };
        let img = egui::ColorImage::from_rgba_unmultiplied([size[0] as usize, size[1] as usize], &px);
        let t = ctx.load_texture(format!("ref-{}", r.id), img, Default::default());
        self.thumbs.insert(r.id.clone(), t.clone());
        Some(t)
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, panels: &mut Panels) {
        let dir = panels.dir();
        if self.project.as_ref() != Some(&dir) {
            self.thumbs.clear();
            self.project = Some(dir.clone());
        }
        let refs: Vec<Ref> = panels.shared.refs.lock().all().iter().rev().cloned().collect();
        let fps = panels.shared.session.lock().doc().scene.fps;

        ui.horizontal(|ui| {
            ui.heading("References");
            ui.label(RichText::new(format!("{} saved", refs.len())).color(Color32::from_gray(140)));
            ui.add_space(16.0);
            for (f, label) in [(Filter::All, "All"), (Filter::Pointers, "Pointers"), (Filter::Open, "Open notes"), (Filter::Resolved, "Resolved")] {
                ui.selectable_value(&mut self.filter, f, label);
            }
            ui.add_space(8.0);
            ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("search").desired_width(180.0));
        });
        ui.label(
            RichText::new("Point at things in the Timeline viewer (Region, Note, right-click a layer); each lands in the agent's prompt and is kept here.")
                .small()
                .color(Color32::from_gray(130)),
        );
        ui.add_space(8.0);

        let search = self.search.to_lowercase();
        let shown: Vec<&Ref> = refs
            .iter()
            .filter(|r| match (self.filter, &r.target) {
                (Filter::All, _) => true,
                (Filter::Pointers, t) => !matches!(t, Target::Note { .. }),
                (Filter::Open, Target::Note { done, .. }) => !done,
                (Filter::Resolved, Target::Note { done, .. }) => *done,
                _ => false,
            })
            .filter(|r| search.is_empty() || r.describe(fps).to_lowercase().contains(&search))
            .collect();
        if shown.is_empty() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| ui.label(RichText::new("Nothing here yet.").color(Color32::from_gray(120))));
            return;
        }

        let mut budget = 2;
        let mut act: Option<(String, &'static str)> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(14.0, 14.0);
                for r in shown {
                    let thumb = self.thumb(ui.ctx(), &panels.shared, r, &mut budget);
                    egui::Frame::new()
                        .fill(Color32::from_rgb(34, 35, 40))
                        .corner_radius(8)
                        .inner_margin(8)
                        .show(ui, |ui| {
                            ui.set_width(THUMB_W as f32 * 0.85);
                            ui.vertical(|ui| {
                                let w = ui.available_width();
                                match &thumb {
                                    Some(t) => {
                                        let s = t.size_vec2();
                                        ui.add(egui::Image::new(t).fit_to_exact_size(vec2(w, w * s.y / s.x)).corner_radius(4));
                                    }
                                    None => {
                                        ui.add_sized(vec2(w, w * 9.0 / 16.0), egui::Spinner::new());
                                    }
                                }
                                let (badge, color) = match &r.target {
                                    Target::Note { done: true, .. } => ("resolved note", Color32::from_gray(140)),
                                    Target::Note { by: Author::Director, .. } => ("director note", Color32::from_rgb(90, 220, 255)),
                                    Target::Note { .. } => ("note", Color32::from_rgb(255, 225, 70)),
                                    Target::Region { .. } => ("region", Color32::from_rgb(255, 43, 214)),
                                    Target::Object { .. } => ("object", Color32::from_rgb(255, 43, 214)),
                                };
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(format!("@{}", r.id)).strong());
                                    ui.label(RichText::new(badge).small().color(color));
                                });
                                let line = r.describe(fps);
                                let about = line.split_once(" = ").map_or(line.as_str(), |(_, a)| a);
                                ui.label(RichText::new(about).small().color(Color32::from_gray(190)));
                                if let Target::Note { reply, .. } = &r.target
                                    && !reply.is_empty()
                                {
                                    ui.label(RichText::new(format!("↳ {reply}")).small().color(Color32::from_gray(150)));
                                }
                                ui.horizontal(|ui| {
                                    if ui.small_button("Send to agent").clicked() {
                                        act = Some((r.id.clone(), "send"));
                                    }
                                    if ui.small_button("Show").clicked() {
                                        act = Some((r.id.clone(), "show"));
                                    }
                                    match &r.target {
                                        Target::Note { done: false, .. } if ui.small_button("Resolve").clicked() => act = Some((r.id.clone(), "resolve")),
                                        Target::Note { done: true, .. } if ui.small_button("Reopen").clicked() => act = Some((r.id.clone(), "reopen")),
                                        _ => {}
                                    }
                                    if ui.small_button("🗑").on_hover_text("Delete").clicked() {
                                        act = Some((r.id.clone(), "delete"));
                                    }
                                });
                            });
                        });
                }
            });
        });

        let Some((id, what)) = act else { return };
        match what {
            "send" => panels.attach_ref(&id),
            "show" => {
                let Some(r) = panels.shared.refs.lock().get(&id).cloned() else { return };
                panels.open_comp(r.comp.clone());
                panels.shared.session.lock().frame = r.frames[0];
                panels.marks = if r.frames[0] == r.frames[1] { [None, None] } else { [Some(r.frames[0]), Some(r.frames[1])] };
                panels.pointed.push(id);
                panels.page = crate::app::Page::Edit;
            }
            "resolve" => {
                let _ = panels.shared.refs.lock().resolve(&id, "resolved by the human");
            }
            "reopen" => panels.shared.refs.lock().reopen(&id),
            "delete" => {
                panels.shared.refs.lock().remove(&id);
                let _ = std::fs::remove_file(thumb_path(&dir, &id));
                self.thumbs.remove(&id);
            }
            _ => {}
        }
    }
}
