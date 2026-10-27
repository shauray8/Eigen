//! A round pen on RGBA pixels: ref marks on agent renders, storyboard sketches.

pub struct Pen<'a> {
    pub rgba: &'a mut [u8],
    pub size: [u32; 2],
    pub width: f32,
    pub color: [u8; 3],
}

impl Pen<'_> {
    pub fn polyline(&mut self, pts: impl IntoIterator<Item = [f32; 2]>) {
        let mut prev: Option<[f32; 2]> = None;
        for b in pts {
            let Some(a) = prev.replace(b) else {
                self.dot(b);
                continue;
            };
            let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
            let steps = (len / (self.width * 0.5).max(0.5)).ceil().max(1.0) as u32;
            for i in 1..=steps {
                let t = i as f32 / steps as f32;
                self.dot([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
            }
        }
    }

    fn dot(&mut self, p: [f32; 2]) {
        let r = self.width / 2.0;
        let (x0, x1) = ((p[0] - r).floor().max(0.0) as u32, ((p[0] + r).ceil().max(0.0) as u32).min(self.size[0]));
        let (y0, y1) = ((p[1] - r).floor().max(0.0) as u32, ((p[1] + r).ceil().max(0.0) as u32).min(self.size[1]));
        for y in y0..y1 {
            for x in x0..x1 {
                let (dx, dy) = (x as f32 + 0.5 - p[0], y as f32 + 0.5 - p[1]);
                if dx * dx + dy * dy <= r * r {
                    let i = ((y * self.size[0] + x) * 4) as usize;
                    self.rgba[i..i + 3].copy_from_slice(&self.color);
                    self.rgba[i + 3] = 255;
                }
            }
        }
    }
}
