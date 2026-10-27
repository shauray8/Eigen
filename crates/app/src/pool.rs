//! Media pool and source viewer, like an NLE's: the pool lists the project's
//! compositions (what the human and the agent built) and its media files.
//! Clicking an item shows it in the source viewer without touching any
//! timeline; double-clicking a comp opens it in the timeline; ＋ places the
//! item at the playhead of the open timeline.

use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui::{self, Color32, RichText, TextureHandle};
use ed_core::data::{Fit, Kind};
use ed_core::eval::{self, EvalMedia, EvalScene, Layer2D};
use ed_media::{Item, MediaKind};

use crate::app::Panels;
use crate::viewport::Canvas;

/// What the source viewer shows.
#[derive(Clone, PartialEq)]
pub enum Source {
    Comp(String),
    Media(Item),
}

#[derive(Default)]
pub struct Pool {
    items: Vec<Item>,
    info: HashMap<String, String>,
    /// Length in seconds of video/audio files, probed once.
    seconds: HashMap<String, f32>,
    thumbs: HashMap<String, Option<TextureHandle>>,
    filter: String,
    loaded: bool,
    /// Source viewer state.
    pub source: Option<Source>,
    source_frame: f32,
    source_playing: bool,
    source_canvas: Canvas,
}

impl Pool {
    pub fn refresh(&mut self) {
        self.loaded = false;
    }

    fn reload(&mut self, dir: &std::path::Path) {
        self.items = ed_media::list(dir);
        self.info.retain(|k, _| self.items.iter().any(|i| &i.src == k));
        self.thumbs.retain(|k, _| self.items.iter().any(|i| &i.src == k));
        self.loaded = true;
    }

    fn thumb(&mut self, ctx: &egui::Context, dir: &std::path::Path, item: &Item) -> Option<TextureHandle> {
        self.thumbs
            .entry(item.src.clone())
            .or_insert_with(|| {
                let pic = ed_media::thumbnail(&dir.join(&item.src), 128).ok()?;
                let img = egui::ColorImage::from_rgba_unmultiplied([pic.w as usize, pic.h as usize], &pic.rgba);
                Some(ctx.load_texture(&item.src, img, Default::default()))
            })
            .clone()
    }

    fn show(&mut self, source: Source) {
        if self.source.as_ref() != Some(&source) {
            self.source = Some(source);
            self.source_frame = 0.0;
            self.source_playing = false;
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, panels: &mut Panels) {
        let dir = panels.dir();
        if !self.loaded {
            self.reload(&dir);
        }
        ui.horizontal(|ui| {
            if ui.button("Import…").clicked() {
                panels.import_dialog();
                self.refresh();
            }
            if ui.button("＋ Comp").on_hover_text("New empty composition").clicked() {
                let name = {
                    let s = panels.shared.session.lock();
                    (1..).map(|i| format!("comp{i}")).find(|n| !s.doc().objects.contains_key(n)).unwrap()
                };
                panels.run(&format!("add comp {name}"));
                self.show(Source::Comp(name));
            }
            if ui.button("⟳").on_hover_text("Rescan folders").clicked() {
                self.refresh();
            }
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("filter").desired_width(120.0));
        });
        let filter = self.filter.to_lowercase();
        let (comps, open): (Vec<(String, [u32; 2], f32, usize)>, Option<String>) = {
            let s = panels.shared.session.lock();
            let doc = s.doc();
            let comps = doc
                .objects
                .iter()
                .filter(|(n, o)| o.kind == Kind::Comp && n.to_lowercase().contains(&filter))
                .map(|(n, _)| {
                    let (res, len) = doc.comp_canvas(Some(n));
                    let count = doc.objects.values().filter(|o| o.comp.as_deref() == Some(n)).count();
                    (n.clone(), res, len, count)
                })
                .collect();
            (comps, s.comp.clone())
        };
        let items: Vec<Item> = self.items.iter().filter(|i| i.src.to_lowercase().contains(&filter)).cloned().collect();
        let fps = panels.shared.session.lock().doc().scene.fps;

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            egui::CollapsingHeader::new(RichText::new(format!("Comps ({})", comps.len())).strong()).default_open(true).show(ui, |ui| {
                if comps.is_empty() {
                    ui.weak("Pieces built as compositions (shots, titles, elements) collect here.");
                }
                for (name, res, len, count) in comps {
                    let shown = self.source == Some(Source::Comp(name.clone()));
                    let row = ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(56.0, 32.0), egui::Sense::hover());
                        ui.painter().rect_filled(rect, 4.0, if open.as_deref() == Some(&name) { Color32::from_rgb(90, 60, 20) } else { Color32::from_gray(40) });
                        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "▦", egui::FontId::proportional(18.0), Color32::from_rgb(240, 150, 60));
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&name).strong().color(if shown { Color32::WHITE } else { Color32::from_gray(210) }));
                            ui.weak(format!("{}×{} · {:.1}s · {count} objects", res[0], res[1], len / fps));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.button("＋").on_hover_text("Place at the playhead of the open timeline").clicked()
                        })
                        .inner
                    });
                    let resp = row.response.interact(egui::Sense::click());
                    if row.inner {
                        panels.place_comp(&name);
                    } else if resp.double_clicked() {
                        panels.open_comp(Some(name.clone()));
                    } else if resp.clicked() {
                        self.show(Source::Comp(name.clone()));
                    }
                }
            });
            egui::CollapsingHeader::new(RichText::new(format!("Media ({})", items.len())).strong()).default_open(true).show(ui, |ui| {
                if items.is_empty() {
                    ui.weak("Drop images, video or music anywhere on the window,\nor ask the agent to find some.");
                }
                for item in items {
                    let info = self.info.entry(item.src.clone()).or_insert_with(|| ed_media::describe(&dir, &item)).clone();
                    let detail = info.split_once(' ').map_or("", |(_, d)| d).to_string();
                    let thumb = if item.kind == MediaKind::Audio { None } else { self.thumb(ui.ctx(), &dir, &item) };
                    let shown = self.source == Some(Source::Media(item.clone()));
                    let row = ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(56.0, 32.0), egui::Sense::hover());
                        match &thumb {
                            Some(t) => {
                                let size = t.size_vec2();
                                let scale = (rect.width() / size.x).min(rect.height() / size.y);
                                let r = egui::Rect::from_center_size(rect.center(), size * scale);
                                ui.painter().image(t.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                            }
                            None => {
                                ui.painter().rect_filled(rect, 4.0, Color32::from_gray(40));
                                let icon = match item.kind {
                                    MediaKind::Audio => "♪",
                                    MediaKind::Video => "▶",
                                    MediaKind::Image => "🖼",
                                };
                                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, icon, egui::FontId::proportional(18.0), Color32::GRAY);
                            }
                        }
                        ui.vertical(|ui| {
                            let name = item.src.rsplit('/').next().unwrap_or(&item.src);
                            ui.label(RichText::new(name).strong().color(if shown { Color32::WHITE } else { Color32::from_gray(210) }));
                            ui.weak(detail);
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.button("＋").on_hover_text("Place at the playhead of the open timeline").clicked()
                        })
                        .inner
                    });
                    let resp = row.response.interact(egui::Sense::click());
                    if row.inner || resp.double_clicked() {
                        panels.add_media(&item);
                    } else if resp.clicked() {
                        self.show(Source::Media(item.clone()));
                    }
                }
            });
        });
    }

    /// The source viewer: plays the pool item on its own, outside any timeline.
    pub fn source_ui(&mut self, ui: &mut egui::Ui, panels: &mut Panels, rs: &eframe::egui_wgpu::RenderState) {
        let Some(source) = self.source.clone() else {
            ui.centered_and_justified(|ui| ui.weak("Click a comp or a file in the media pool to view it here."));
            return;
        };
        let dir: PathBuf = panels.dir();
        // Length in frames, and how to evaluate a frame.
        let (fps, res, version) = {
            let s = panels.shared.session.lock();
            (s.doc().scene.fps, s.doc().scene.res, s.version)
        };
        let len = match &source {
            Source::Comp(c) => {
                let s = panels.shared.session.lock();
                if !s.doc().objects.get(c).is_some_and(|o| o.kind == Kind::Comp) {
                    drop(s);
                    self.source = None;
                    return;
                }
                s.doc().comp_canvas(Some(c)).1
            }
            Source::Media(item) if item.kind == MediaKind::Image => 1.0,
            Source::Media(item) => {
                let secs = *self.seconds.entry(item.src.clone()).or_insert_with(|| ed_media::probe(&dir.join(&item.src)).map_or(0.0, |i| i.seconds));
                (secs * fps).max(1.0)
            }
        };

        // Transport.
        ui.horizontal(|ui| {
            if ui.button(if self.source_playing { "⏸" } else { "▶" }).clicked() {
                self.source_playing = !self.source_playing;
            }
            let mut f = self.source_frame;
            ui.add(egui::Slider::new(&mut f, 0.0..=(len - 1.0).max(0.0)).integer().show_value(false));
            if f != self.source_frame {
                self.source_frame = f;
                self.source_playing = false;
            }
            ui.monospace(format!("{:>4} / {}", self.source_frame as i32, (len - 1.0).max(0.0) as i32));
            let label = match &source {
                Source::Comp(c) => format!("▦ {c}"),
                Source::Media(item) => item.src.clone(),
            };
            ui.strong(label);
            if let Source::Comp(c) = &source {
                if ui.button("Open in timeline").clicked() {
                    panels.open_comp(Some(c.clone()));
                }
                if ui.button("Place").on_hover_text("at the playhead of the open timeline").clicked() {
                    panels.place_comp(c);
                }
            }
        });
        if self.source_playing {
            let dt = ui.input(|i| i.stable_dt).min(0.1);
            self.source_frame += dt * fps;
            if self.source_frame >= len {
                self.source_frame = 0.0;
            }
            ui.ctx().request_repaint();
        }
        let frame = self.source_frame.floor();

        let avail = ui.available_rect_before_wrap();
        ui.allocate_rect(avail, egui::Sense::hover());
        match &source {
            Source::Media(item) if item.kind == MediaKind::Audio => {
                ui.painter().text(avail.center(), egui::Align2::CENTER_CENTER, format!("♪ {:.1}s", len / fps), egui::FontId::proportional(20.0), Color32::GRAY);
            }
            Source::Comp(c) => {
                let res = panels.shared.session.lock().doc().comp_canvas(Some(c)).0;
                let rect = Canvas::fit(avail, res);
                let shared = panels.shared.clone();
                self.source_canvas.show(ui, rs, rect, (version, frame.to_bits(), c.as_str()), |target| {
                    let scene = eval::evaluate_comp(shared.session.lock().doc(), Some(c), frame);
                    shared.renderer.lock().render_canvas(&scene, target);
                });
            }
            Source::Media(item) => {
                let rect = Canvas::fit(avail, res);
                let shared = panels.shared.clone();
                let time = (item.kind == MediaKind::Video).then_some(frame / fps);
                self.source_canvas.show(ui, rs, rect, (frame.to_bits(), item.src.as_str()), |target| {
                    let scene = picture(res, &item.src, time);
                    shared.renderer.lock().render_canvas(&scene, target);
                });
            }
        }
    }
}

/// A canvas of `res` showing one picture or video frame, letterboxed.
fn picture(res: [u32; 2], src: &str, time: Option<f32>) -> EvalScene {
    let [w, h] = res.map(|v| v as f32);
    EvalScene {
        frame: 0.0,
        res,
        bg: [0.0, 0.0, 0.0, 1.0],
        ambient: 0.0,
        env: String::new(),
        envrot: 0.0,
        bloom: 0.0,
        grain: 0.0,
        vignette: 0.0,
        meshes: Vec::new(),
        points: Vec::new(),
        lights: Vec::new(),
        camera: None,
        layers: vec![Layer2D::Media(EvalMedia {
            src: src.to_string(),
            time,
            center: [w / 2.0, h / 2.0],
            size: [w, h],
            rot_deg: 0.0,
            opacity: 1.0,
            fit: Fit::Contain,
        })],
    }
}

/// Files dropped on the window this frame.
pub fn dropped(ctx: &egui::Context) -> Vec<PathBuf> {
    ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect())
}
