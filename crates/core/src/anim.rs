//! Animation curves. Like Blender's FCurve: one curve per scalar channel,
//! interpolation stored on the left key of each segment.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Ease {
    Const,
    Linear,
    InQuad,
    OutQuad,
    InOutQuad,
    InCubic,
    OutCubic,
    #[default]
    InOutCubic,
    InOutSine,
    OutBack,
    OutExpo,
}

impl Ease {
    const ALL: [Ease; 11] = [
        Ease::Const,
        Ease::Linear,
        Ease::InQuad,
        Ease::OutQuad,
        Ease::InOutQuad,
        Ease::InCubic,
        Ease::OutCubic,
        Ease::InOutCubic,
        Ease::InOutSine,
        Ease::OutBack,
        Ease::OutExpo,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Ease::Const => "const",
            Ease::Linear => "linear",
            Ease::InQuad => "inQuad",
            Ease::OutQuad => "outQuad",
            Ease::InOutQuad => "inOutQuad",
            Ease::InCubic => "inCubic",
            Ease::OutCubic => "outCubic",
            Ease::InOutCubic => "inOutCubic",
            Ease::InOutSine => "inOutSine",
            Ease::OutBack => "outBack",
            Ease::OutExpo => "outExpo",
        }
    }

    pub fn parse(s: &str) -> Option<Ease> {
        Ease::ALL.into_iter().find(|e| e.name() == s)
    }

    pub fn apply(self, t: f32) -> f32 {
        use std::f32::consts::PI;
        let t = t.clamp(0.0, 1.0);
        match self {
            Ease::Const => 0.0,
            Ease::Linear => t,
            Ease::InQuad => t * t,
            Ease::OutQuad => 1.0 - (1.0 - t) * (1.0 - t),
            Ease::InOutQuad => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
                }
            }
            Ease::InCubic => t * t * t,
            Ease::OutCubic => 1.0 - (1.0 - t).powi(3),
            Ease::InOutCubic => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
                }
            }
            Ease::InOutSine => -((PI * t).cos() - 1.0) / 2.0,
            Ease::OutBack => {
                let c1 = 1.70158;
                let c3 = c1 + 1.0;
                1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
            }
            Ease::OutExpo => {
                if t >= 1.0 {
                    1.0
                } else {
                    1.0 - 2f32.powf(-10.0 * t)
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    pub frame: f32,
    pub value: f32,
    /// Interpolation from this key to the next.
    pub ease: Ease,
}

#[derive(Clone, Debug, Default)]
pub struct FCurve {
    /// Sorted by frame, unique frames.
    pub keys: Vec<Key>,
}

impl FCurve {
    pub fn insert(&mut self, key: Key) {
        match self.keys.binary_search_by(|k| k.frame.total_cmp(&key.frame)) {
            Ok(i) => self.keys[i] = key,
            Err(i) => self.keys.insert(i, key),
        }
    }

    pub fn remove(&mut self, frame: f32) -> bool {
        let before = self.keys.len();
        self.keys.retain(|k| k.frame != frame);
        before != self.keys.len()
    }

    pub fn eval(&self, frame: f32) -> Option<f32> {
        let first = self.keys.first()?;
        let last = self.keys.last()?;
        if frame <= first.frame {
            return Some(first.value);
        }
        if frame >= last.frame {
            return Some(last.value);
        }
        let i = self.keys.partition_point(|k| k.frame <= frame) - 1;
        let (a, b) = (self.keys[i], self.keys[i + 1]);
        let t = (frame - a.frame) / (b.frame - a.frame);
        Some(a.value + (b.value - a.value) * a.ease.apply(t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_between_keys() {
        let mut c = FCurve::default();
        c.insert(Key { frame: 0.0, value: 0.0, ease: Ease::Linear });
        c.insert(Key { frame: 10.0, value: 10.0, ease: Ease::Linear });
        assert_eq!(c.eval(5.0), Some(5.0));
        assert_eq!(c.eval(-1.0), Some(0.0));
        assert_eq!(c.eval(20.0), Some(10.0));
    }
}
