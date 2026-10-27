//! Render frames of a project without the app:
//! `cargo run --release -p ed-render --example render -- <project-dir> <f1,f2,...> <out.png> [width] [extra.ops]`
//! The project's `scene.ops` is loaded, then the optional extra ops on top.

use ed_core::{Session, Source, eval};
use ed_render::{Renderer, contact_sheet, encode_png};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(args.len() >= 3, "usage: render <project-dir> <f1,f2,...> <out.png> [width] [extra.ops]");
    let dir = std::path::PathBuf::from(&args[0]);
    let frames: Vec<f32> = args[1].split(',').map(|f| f.trim().parse()).collect::<Result<_, _>>()?;
    let width: u32 = args.get(3).map_or(Ok(640), |w| w.parse())?;
    let mut s = Session::new();
    s.run(&std::fs::read_to_string(dir.join("scene.ops"))?, Source::Agent)?;
    if let Some(extra) = args.get(4) {
        s.run(&std::fs::read_to_string(extra)?, Source::Agent)?;
    }
    let r = Renderer::headless().ok_or_else(|| anyhow::anyhow!("no GPU adapter"))?;
    r.set_root(dir);
    let res = s.doc().scene.res;
    let size = [width, (width as f32 * res[1] as f32 / res[0] as f32).round() as u32];
    let mut shots = Vec::new();
    for &f in &frames {
        let t0 = std::time::Instant::now();
        shots.push(r.render_rgba(&eval::evaluate(s.doc(), f), size)?);
        eprintln!("frame {f}: {:.0} ms", t0.elapsed().as_secs_f32() * 1000.0);
    }
    let (sheet, sheet_size) = contact_sheet(&shots, size, 3.min(frames.len() as u32));
    std::fs::write(&args[2], encode_png(&sheet, sheet_size)?)?;
    Ok(())
}
