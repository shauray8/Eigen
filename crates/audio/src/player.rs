//! Playback: mixes the timeline's audio clips on the audio thread. While
//! playing, the audio position is the clock the picture follows.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;

use crate::RATE;

#[derive(Clone)]
pub struct Clip {
    /// Interleaved stereo at [`RATE`].
    pub pcm: Arc<Vec<f32>>,
    /// Timeline second where the clip starts.
    pub start: f64,
    /// Seconds skipped at the start of the file.
    pub trim: f64,
    pub volume: f32,
    /// Timeline second where the clip stops (exclusive).
    pub end: f64,
}

#[derive(Default)]
struct State {
    clips: Vec<Clip>,
    playing: bool,
    /// Timeline position in seconds.
    pos: f64,
}

pub struct Player {
    state: Arc<Mutex<State>>,
    _stream: Option<cpal::Stream>,
    cache: Arc<Mutex<HashMap<PathBuf, Option<Arc<Vec<f32>>>>>>,
}

impl Player {
    /// Opens the default output device; without one, playback is silent but
    /// the position still advances via [`Player::advance`].
    pub fn new() -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let stream = Self::open(state.clone()).ok();
        Self { state, _stream: stream, cache: Arc::default() }
    }

    fn open(state: Arc<Mutex<State>>) -> anyhow::Result<cpal::Stream> {
        let device = cpal::default_host().default_output_device().ok_or_else(|| anyhow::anyhow!("no output device"))?;
        let config = cpal::StreamConfig { channels: 2, sample_rate: RATE, buffer_size: cpal::BufferSize::Default };
        let stream = device.build_output_stream(
            config,
            move |out: &mut [f32], _| {
                let mut st = state.lock();
                if !st.playing {
                    out.fill(0.0);
                    return;
                }
                let start = st.pos;
                for (i, frame) in out.chunks_exact_mut(2).enumerate() {
                    let t = start + i as f64 / RATE as f64;
                    let (mut l, mut r) = (0.0, 0.0);
                    for c in &st.clips {
                        if t < c.start || t >= c.end {
                            continue;
                        }
                        let idx = (((t - c.start + c.trim) * RATE as f64) as usize) * 2;
                        if idx + 1 < c.pcm.len() {
                            l += c.pcm[idx] * c.volume;
                            r += c.pcm[idx + 1] * c.volume;
                        }
                    }
                    frame[0] = l.clamp(-1.0, 1.0);
                    frame[1] = r.clamp(-1.0, 1.0);
                }
                st.pos += (out.len() / 2) as f64 / RATE as f64;
            },
            |e| eprintln!("audio: {e}"),
            None,
        )?;
        stream.play()?;
        Ok(stream)
    }

    pub fn has_device(&self) -> bool {
        self._stream.is_some()
    }

    /// Decoded audio for a file, decoding in the background on first use.
    pub fn pcm(&self, path: &PathBuf) -> Option<Arc<Vec<f32>>> {
        let mut cache = self.cache.lock();
        match cache.get(path) {
            Some(p) => p.clone(),
            None => {
                cache.insert(path.clone(), None);
                let (cache, path) = (self.cache.clone(), path.clone());
                std::thread::spawn(move || {
                    let pcm = crate::decode(&path, RATE, 2).map(Arc::new).ok();
                    cache.lock().insert(path, pcm);
                });
                None
            }
        }
    }

    pub fn set_clips(&self, clips: Vec<Clip>) {
        self.state.lock().clips = clips;
    }

    pub fn play(&self, at: f64) {
        let mut st = self.state.lock();
        st.pos = at;
        st.playing = true;
    }

    pub fn stop(&self) {
        self.state.lock().playing = false;
    }

    /// Current timeline position (the clock while playing).
    pub fn position(&self) -> f64 {
        self.state.lock().pos
    }

    /// Advances the clock when there is no audio device.
    pub fn advance(&self, dt: f64) {
        if !self.has_device() {
            let mut st = self.state.lock();
            if st.playing {
                st.pos += dt;
            }
        }
    }
}
