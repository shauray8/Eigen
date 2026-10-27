//! Audio: decoding (ffmpeg), musical analysis (aubio beat tracking), a music
//! library search over open catalogs plus local folders, and playback (cpal).

pub mod library;
pub mod player;

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, ensure};

/// Playback and mixing rate.
pub const RATE: u32 = 48_000;
const ANALYSIS_RATE: u32 = 22_050;

/// Decodes any file ffmpeg can read to interleaved f32 at `rate`.
pub fn decode(path: &Path, rate: u32, channels: u32) -> Result<Vec<f32>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-ac", &channels.to_string(), "-ar", &rate.to_string(), "-"])
        .output()
        .context("could not run ffmpeg (brew install ffmpeg)")?;
    ensure!(out.status.success(), "ffmpeg could not decode {}: {}", path.display(), String::from_utf8_lossy(&out.stderr));
    Ok(out.stdout.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
}

#[derive(Clone, Debug)]
pub struct Analysis {
    pub duration: f32,
    pub bpm: f32,
    /// First downbeat (bar start) in seconds.
    pub beat0: f32,
    /// Detected beat times in seconds.
    pub beats: Vec<f32>,
    /// RMS loudness per 4-beat bar on the grid, normalized 0..1.
    pub bar_energy: Vec<f32>,
}

fn rms(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
}

/// Tempo, beat grid and per-bar energy of a music file.
pub fn analyze(path: &Path) -> Result<Analysis> {
    let pcm = decode(path, ANALYSIS_RATE, 1)?;
    let duration = pcm.len() as f32 / ANALYSIS_RATE as f32;
    let hop = 512;
    let mut tempo = aubio::Tempo::new(aubio::OnsetMode::SpecFlux, 1024, hop, ANALYSIS_RATE)
        .map_err(|e| anyhow::anyhow!("aubio: {e:?}"))?;
    let mut beats = Vec::new();
    for chunk in pcm.chunks_exact(hop) {
        if tempo.do_result(chunk).map_err(|e| anyhow::anyhow!("aubio: {e:?}"))? > 0.0 {
            beats.push(tempo.get_last_s());
        }
    }
    ensure!(beats.len() >= 8, "no steady beat found in {}", path.display());

    // Tempo from the median beat interval; it is steadier than aubio's running estimate.
    let mut gaps: Vec<f32> = beats.windows(2).map(|w| w[1] - w[0]).filter(|g| *g > 0.2 && *g < 2.0).collect();
    gaps.sort_by(f32::total_cmp);
    let period = gaps[gaps.len() / 2];
    // Fold octave errors (half/double tempo) into the range most music lives in.
    let mut bpm = 60.0 / period;
    while bpm < 80.0 {
        bpm *= 2.0;
    }
    while bpm > 170.0 {
        bpm /= 2.0;
    }
    let bpm = (bpm * 10.0).round() / 10.0;
    let period = 60.0 / bpm;

    // Grid phase: circular mean of beat times modulo the period.
    let (mut sx, mut sy) = (0.0f32, 0.0f32);
    for b in &beats {
        let a = (b / period).fract() * std::f32::consts::TAU;
        sx += a.cos();
        sy += a.sin();
    }
    let phase = (sy.atan2(sx) / std::f32::consts::TAU).rem_euclid(1.0) * period;

    // Downbeat: of the four bar positions, the one whose beats hit hardest.
    let energy_at = |t: f32| {
        let i = (t * ANALYSIS_RATE as f32) as usize;
        rms(&pcm[i.min(pcm.len())..(i + 2048).min(pcm.len())])
    };
    let grid: Vec<f32> = (0..).map(|k| phase + k as f32 * period).take_while(|t| *t < duration).collect();
    let best = (0..4)
        .max_by(|a, b| {
            let e = |k: usize| grid.iter().skip(k).step_by(4).map(|t| energy_at(*t)).sum::<f32>();
            e(*a).total_cmp(&e(*b))
        })
        .unwrap_or(0);
    let beat0 = phase + best as f32 * period;

    let bar = period * 4.0;
    let mut bar_energy: Vec<f32> = (0..)
        .map(|k| beat0 + k as f32 * bar)
        .take_while(|t| *t + bar <= duration)
        .map(|t| {
            let a = (t * ANALYSIS_RATE as f32) as usize;
            let b = ((t + bar) * ANALYSIS_RATE as f32) as usize;
            rms(&pcm[a..b.min(pcm.len())])
        })
        .collect();
    // Normalize between the 5th and 95th percentile so sections stand out
    // even in heavily compressed masters.
    let mut sorted = bar_energy.clone();
    sorted.sort_by(f32::total_cmp);
    if let (Some(lo), Some(hi)) = (sorted.get(sorted.len() / 20), sorted.get(sorted.len() * 19 / 20)) {
        let (lo, hi) = (*lo, hi.max(lo + 1e-6));
        bar_energy.iter_mut().for_each(|e| *e = ((*e - lo) / (hi - lo)).clamp(0.0, 1.0));
    }
    Ok(Analysis { duration, bpm, beat0, beats, bar_energy })
}

impl Analysis {
    /// Compact, agent-readable summary.
    pub fn summary(&self, src: &str) -> String {
        let digits: String = self.bar_energy.iter().map(|e| char::from(b'0' + (e * 9.0).round() as u8)).collect();
        // Sections: runs of loud bars (energy >= 0.7).
        let mut loud = Vec::new();
        let mut start = None;
        for (i, e) in self.bar_energy.iter().chain([0.0].iter()).enumerate() {
            match (start, *e >= 0.7) {
                (None, true) => start = Some(i),
                (Some(s), false) => {
                    if i - s >= 2 {
                        loud.push(format!("{}-{}", s * 4, i * 4));
                    }
                    start = None;
                }
                _ => {}
            }
        }
        let m = (self.duration / 60.0) as u32;
        format!(
            "{src} {m}:{:04.1} bpm={} beat0={:.3}s ({} bars of 4)\n\
             energy per bar (0-9): {digits}\n\
             loud stretches (beats): {}\n\
             use: add audio <name> src={src} bpm={} beat0={:.3} [trim=<s> in=<frame>] ; set scene.music <name> ; then frames like @b16, in=b32",
            self.duration % 60.0,
            self.bpm,
            self.beat0,
            self.bar_energy.len(),
            if loud.is_empty() { "none".into() } else { loud.join(" ") },
            self.bpm,
            self.beat0
        )
    }
}

/// Peak envelope for waveform display: `per_sec` buckets per second of mono.
pub fn peaks(stereo: &[f32], per_sec: u32) -> Vec<f32> {
    let bucket = (RATE / per_sec) as usize * 2;
    stereo.chunks(bucket.max(2)).map(|c| c.iter().fold(0.0f32, |m, v| m.max(v.abs()))).collect()
}
