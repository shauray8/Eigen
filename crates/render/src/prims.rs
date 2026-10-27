//! Built-in unit primitives: interleaved position, normal and UV, u32
//! indices, counter-clockwise front faces. UVs run 0..1 per face (box),
//! around the equator (sphere, cylinder side) or across the cap.

use std::f32::consts::{PI, TAU};

use ed_core::data::Kind;
use glam::Vec3;

pub type Mesh = (Vec<[f32; 8]>, Vec<u32>);

fn quad(v: &mut Vec<[f32; 8]>, i: &mut Vec<u32>, n: Vec3, u: Vec3, w: Vec3) {
    let base = v.len() as u32;
    let c = n * 0.5;
    for (a, b) in [(-0.5, -0.5), (0.5, -0.5), (0.5, 0.5), (-0.5, 0.5)] {
        let p = c + u * a + w * b;
        v.push([p.x, p.y, p.z, n.x, n.y, n.z, a + 0.5, 0.5 - b]);
    }
    i.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
}

fn cube() -> Mesh {
    let (mut v, mut i) = (Vec::new(), Vec::new());
    for n in [Vec3::X, Vec3::NEG_X, Vec3::Y, Vec3::NEG_Y, Vec3::Z, Vec3::NEG_Z] {
        let u = if n.y.abs() > 0.5 { Vec3::X } else { Vec3::Y.cross(n) };
        let w = n.cross(u);
        quad(&mut v, &mut i, n, u, w);
    }
    (v, i)
}

fn plane() -> Mesh {
    let (mut v, mut i) = (Vec::new(), Vec::new());
    quad(&mut v, &mut i, Vec3::Y, Vec3::Z, Vec3::X);
    for p in &mut v {
        p[1] = 0.0;
    }
    (v, i)
}

fn sphere() -> Mesh {
    let (rings, segs) = (32u32, 64u32);
    let (mut v, mut i) = (Vec::new(), Vec::new());
    for r in 0..=rings {
        let phi = PI * r as f32 / rings as f32;
        for s in 0..=segs {
            let theta = TAU * s as f32 / segs as f32;
            let n = Vec3::new(phi.sin() * theta.sin(), phi.cos(), phi.sin() * theta.cos());
            let p = n * 0.5;
            v.push([p.x, p.y, p.z, n.x, n.y, n.z, s as f32 / segs as f32 * 2.0, r as f32 / rings as f32]);
        }
    }
    for r in 0..rings {
        for s in 0..segs {
            let a = r * (segs + 1) + s;
            let b = a + segs + 1;
            i.extend([a, b, a + 1, a + 1, b, b + 1]);
        }
    }
    (v, i)
}

fn cylinder() -> Mesh {
    let segs = 64u32;
    let (mut v, mut i) = (Vec::new(), Vec::new());
    // side
    for s in 0..=segs {
        let t = TAU * s as f32 / segs as f32;
        let n = Vec3::new(t.sin(), 0.0, t.cos());
        for y in [-0.5, 0.5] {
            v.push([n.x * 0.5, y, n.z * 0.5, n.x, 0.0, n.z, s as f32 / segs as f32 * 3.0, 0.5 - y]);
        }
    }
    for s in 0..segs {
        let a = s * 2;
        i.extend([a, a + 2, a + 1, a + 1, a + 2, a + 3]);
    }
    // caps
    for (y, ny) in [(0.5f32, 1.0f32), (-0.5, -1.0)] {
        let center = v.len() as u32;
        v.push([0.0, y, 0.0, 0.0, ny, 0.0, 0.5, 0.5]);
        for s in 0..=segs {
            let t = TAU * s as f32 / segs as f32;
            v.push([t.sin() * 0.5, y, t.cos() * 0.5, 0.0, ny, 0.0, t.sin() * 0.5 + 0.5, t.cos() * 0.5 + 0.5]);
        }
        for s in 0..segs {
            let (a, b) = (center + 1 + s, center + 2 + s);
            if ny > 0.0 { i.extend([center, a, b]) } else { i.extend([center, b, a]) }
        }
    }
    (v, i)
}

pub fn build(kind: Kind) -> Mesh {
    match kind {
        Kind::Cube => cube(),
        Kind::Sphere => sphere(),
        Kind::Plane => plane(),
        Kind::Cylinder => cylinder(),
        _ => (Vec::new(), Vec::new()),
    }
}

/// Triangles from an OBJ file (`v`, `vt`, `vn`, `f`; polygons are fanned).
/// Faces without normals get the face normal; V is flipped to top-down.
pub fn load_obj(text: &str) -> Option<Mesh> {
    let (mut pos, mut uv, mut nrm) = (Vec::<Vec3>::new(), Vec::<[f32; 2]>::new(), Vec::<Vec3>::new());
    let (mut v, mut i) = (Vec::new(), Vec::new());
    let nums = |rest: std::str::SplitWhitespace| rest.map(|s| s.parse::<f32>().unwrap_or(0.0)).collect::<Vec<_>>();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("v") => {
                let n = nums(it);
                pos.push(Vec3::new(*n.first()?, *n.get(1)?, *n.get(2)?));
            }
            Some("vt") => {
                let n = nums(it);
                uv.push([*n.first()?, 1.0 - n.get(1).copied().unwrap_or(0.0)]);
            }
            Some("vn") => {
                let n = nums(it);
                nrm.push(Vec3::new(*n.first()?, *n.get(1)?, *n.get(2)?).normalize_or_zero());
            }
            Some("f") => {
                // index into a list, 1-based or negative (relative to the end)
                let at = |s: Option<&str>, len: usize| -> Option<usize> {
                    let k: i64 = s.filter(|s| !s.is_empty())?.parse().ok()?;
                    let k = if k < 0 { len as i64 + k } else { k - 1 };
                    (0..len as i64).contains(&k).then_some(k as usize)
                };
                let corners: Vec<(usize, Option<usize>, Option<usize>)> = it
                    .filter_map(|c| {
                        let mut p = c.split('/');
                        Some((at(p.next(), pos.len())?, at(p.next(), uv.len()), at(p.next(), nrm.len())))
                    })
                    .collect();
                if corners.len() < 3 {
                    continue;
                }
                let face_n = (pos[corners[1].0] - pos[corners[0].0]).cross(pos[corners[2].0] - pos[corners[0].0]).normalize_or_zero();
                let base = v.len() as u32;
                for &(p, t, n) in &corners {
                    let (p, n, t) = (pos[p], n.map_or(face_n, |n| nrm[n]), t.map_or([0.0, 0.0], |t| uv[t]));
                    v.push([p.x, p.y, p.z, n.x, n.y, n.z, t[0], t[1]]);
                }
                for k in 1..corners.len() as u32 - 1 {
                    i.extend([base, base + k, base + k + 1]);
                }
            }
            _ => {}
        }
    }
    (!i.is_empty()).then_some((v, i))
}

#[cfg(test)]
mod tests {
    #[test]
    fn obj_quad_fans_into_two_triangles() {
        let obj = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nvt 0 0\nvn 0 0 1\nf 1/1/1 2/1/1 3/1/1 -1/1/1\n";
        let (v, i) = super::load_obj(obj).unwrap();
        assert_eq!((v.len(), i), (4, vec![0, 1, 2, 0, 2, 3]));
        assert_eq!(v[3][..3], [0.0, 1.0, 0.0]);
        assert_eq!(v[0][5], 1.0);
        assert_eq!(v[0][7], 1.0); // V flipped
    }
}
