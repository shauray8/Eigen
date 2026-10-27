//! Export a project without the app (same path as the `export` tool):
//! `cargo run --release -p ed-mcp --example export -- <project-dir> [out.mp4] [h264|prores] [width] [extra.ops]`
//! The optional extra ops apply on top of `scene.ops` for this export only (e.g. a fast preview).

use ed_core::Session;
use ed_render::Renderer;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(!args.is_empty(), "usage: export <project-dir> [out.mp4] [h264|prores] [width] [extra.ops]");
    let dir = std::path::PathBuf::from(&args[0]);
    let mut session = Session::new();
    session.load(&dir.join("scene.ops"))?;
    if let Some(extra) = args.get(4) {
        session.run(&std::fs::read_to_string(extra)?, ed_core::Source::Agent)?;
    }
    let renderer = Renderer::headless().ok_or_else(|| anyhow::anyhow!("no GPU adapter"))?;
    renderer.set_root(dir.clone());
    let shared = ed_mcp::Shared::new(dir.clone(), session, renderer, || {});
    let out = dir.join(args.get(1).map_or("out.mp4", String::as_str));
    let codec = args.get(2).map_or("h264", String::as_str);
    let width = args.get(3).map(|w| w.parse()).transpose()?;
    println!("{}", ed_mcp::export_video(&shared, &out, codec, width)?);
    Ok(())
}
