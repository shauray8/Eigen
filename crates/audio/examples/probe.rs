//! cargo run -p ed-audio --example probe -- "<query>" <project-dir>
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (q, dir) = (&args[1], std::path::Path::new(&args[2]));
    let t = std::time::Instant::now();
    let (tracks, errors) = ed_audio::library::search(q, Some(60.0), None, &[]);
    for t in tracks.iter().take(8) {
        println!("{}", t.line());
    }
    println!("errors: {errors:?} ({:.1}s)", t.elapsed().as_secs_f32());
    let pick = tracks.iter().find(|t| t.id.starts_with("ov:") || t.id.starts_with("cc:")).expect("no track");
    let t = std::time::Instant::now();
    let src = ed_audio::library::fetch(&pick.id, dir)?;
    println!("fetched {src} ({:.1}s)", t.elapsed().as_secs_f32());
    let t = std::time::Instant::now();
    let a = ed_audio::analyze(&dir.join(&src))?;
    println!("{}\n({:.1}s)", a.summary(&src), t.elapsed().as_secs_f32());
    Ok(())
}
