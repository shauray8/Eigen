//! Eigen: one process holding the document, the renderer, the agent
//! endpoint and the agent's terminal session (omp).
//!
//! Usage: `eigen [project-dir]`. Without a folder it opens on the Project
//! Manager; projects live in `~/Movies/Editor Projects` (or `$ED_PROJECTS`)
//! and can be switched without restarting. A project's scene is
//! `<dir>/scene.ops`, an op-language script.

mod agent;
mod app;
mod extensions;
mod menu;
mod moodboard;
mod pool;
mod projects;
mod skill_catalog;
mod refs_page;
mod storyboard;
mod terminal;
mod theme;
mod tmux;
mod timeline;
mod viewport;

use std::path::PathBuf;

use anyhow::{Context, Result};

pub const PORT: u16 = 4777;

fn main() -> Result<()> {
    let project = match std::env::args().nth(1) {
        Some(arg) => {
            let dir = PathBuf::from(arg);
            std::fs::create_dir_all(&dir)?;
            Some(dir.canonicalize()?)
        }
        None => None,
    };
    let port = std::env::var("ED_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(PORT);
    // Until a project opens, the shared state points at an empty scratch folder.
    let scratch = std::env::temp_dir().join("editor-scratch");
    std::fs::create_dir_all(&scratch)?;

    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/eigen-icon.png")).context("app icon")?;
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default().with_title("Eigen").with_icon(icon).with_inner_size([1600.0, 1000.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "Eigen",
        options,
        Box::new(move |cc| {
            let rs = cc.wgpu_render_state.as_ref().context("wgpu renderer unavailable")?;
            // wgpu's default handler panics. A display change (monitor plugged or
            // unplugged, window moved to another screen, sleep) can fail one surface
            // reconfigure inside a macOS callback that cannot unwind, which aborted a
            // 15-hour session. Log instead; the next frame configures again.
            rs.device.on_uncaptured_error(std::sync::Arc::new(|e: eframe::wgpu::Error| eprintln!("wgpu error (continuing): {e}")));
            let renderer = ed_render::Renderer::new(rs.device.clone(), rs.queue.clone());
            let ctx = cc.egui_ctx.clone();
            let shared = ed_mcp::Shared::new(scratch, ed_core::Session::new(), renderer, move || ctx.request_repaint());
            ed_mcp::spawn(shared.clone(), port);
            Ok(Box::new(app::App::new(cc, shared, project, port)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}
