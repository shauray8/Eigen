//! The photographic parts of the 3D pass: the camera's lens and air, the HDR
//! world target the post pass reads, the environment map (with its diffuse
//! light as spherical harmonics), and the shadow cube's faces.

use std::f32::consts::PI;
use std::path::Path;

use glam::{Mat4, Vec3};

use crate::{DEPTH, HDR, SAMPLES, look_at_rh, perspective_rh};

/// Depth of field and haze of the camera a view looks through.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Lens {
    /// Focus distance, scene units.
    pub focus: f32,
    /// Aperture diameter, scene units (0 = pinhole, all sharp).
    pub aperture: f32,
    /// Haze density per scene unit.
    pub haze: f32,
    pub haze_color: [f32; 3],
}

/// Mips the bloom reads below the full-resolution image.
pub(crate) const BLOOM_LODS: u32 = 6;
pub(crate) const SHADOW_SIZE: u32 = 2048;
pub(crate) const SHADOW_NEAR: f32 = 0.1;
pub(crate) const SHADOW_FAR: f32 = 1000.0;

/// Where a view's world is drawn before the post pass: HDR color (multisampled,
/// resolved into a mip chain for bloom) and a depth buffer the post pass reads.
pub(crate) struct WorldTarget {
    pub size: [u32; 2],
    pub msaa: wgpu::TextureView,
    pub depth: wgpu::TextureView,
    /// One view per mip of the resolved HDR image.
    pub levels: Vec<wgpu::TextureView>,
    pub full: wgpu::TextureView,
}

impl WorldTarget {
    pub fn new(device: &wgpu::Device, size: [u32; 2]) -> Self {
        let extent = wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 };
        let tex = |label, format, samples, mips, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: mips,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let att = wgpu::TextureUsages::RENDER_ATTACHMENT;
        let read = att | wgpu::TextureUsages::TEXTURE_BINDING;
        let msaa = tex("world msaa", HDR, SAMPLES, 1, att).create_view(&Default::default());
        let depth = tex("world depth", DEPTH, SAMPLES, 1, read).create_view(&Default::default());
        let mips = (32 - size[0].min(size[1]).max(1).leading_zeros()).min(BLOOM_LODS + 1);
        let hdr = tex("world hdr", HDR, 1, mips, read);
        let levels = (0..mips)
            .map(|m| {
                hdr.create_view(&wgpu::TextureViewDescriptor {
                    base_mip_level: m,
                    mip_level_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let full = hdr.create_view(&Default::default());
        Self { size, msaa, depth, levels, full }
    }
}

/// An equirect environment on the GPU.
pub(crate) struct EnvGpu {
    pub view: wgpu::TextureView,
    pub max_lod: f32,
    /// Irradiance / pi as SH9, rgb in xyz.
    pub sh: [[f32; 4]; 9],
}

/// Loads an equirect HDR (or any image) as an environment.
pub(crate) fn load_env(device: &wgpu::Device, queue: &wgpu::Queue, path: &Path) -> Option<EnvGpu> {
    let img = image::open(path).ok()?.to_rgba32f();
    let (w, h) = img.dimensions();
    Some(upload_env(device, queue, w, h, img.into_raw()))
}

pub(crate) fn upload_env(device: &wgpu::Device, queue: &wgpu::Queue, w: u32, h: u32, px: Vec<f32>) -> EnvGpu {
    let levels = 32 - w.max(h).leading_zeros();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("env"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let (mut lw, mut lh, mut cur) = (w, h, px);
    let mut sh = None;
    for mip in 0..levels {
        let half: Vec<u16> = cur.iter().map(|&v| f16(v)).collect();
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: mip, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            bytemuck::cast_slice(&half),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(lw * 8), rows_per_image: None },
            wgpu::Extent3d { width: lw, height: lh, depth_or_array_layers: 1 },
        );
        if sh.is_none() && lw <= 256 {
            sh = Some(project_sh(lw, lh, &cur));
        }
        if mip + 1 < levels {
            (cur, lw, lh) = halve(&cur, lw, lh);
        }
    }
    EnvGpu {
        view: texture.create_view(&Default::default()),
        max_lod: (levels - 1) as f32,
        sh: sh.unwrap_or_else(|| project_sh(lw, lh, &cur)),
    }
}

/// 2x2 box downsample of RGBA f32 pixels.
fn halve(px: &[f32], w: u32, h: u32) -> (Vec<f32>, u32, u32) {
    let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
    let mut out = vec![0.0; (nw * nh * 4) as usize];
    for y in 0..nh {
        for x in 0..nw {
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let sx = (x * 2 + dx).min(w - 1);
                let sy = (y * 2 + dy).min(h - 1);
                let s = ((sy * w + sx) * 4) as usize;
                let o = ((y * nw + x) * 4) as usize;
                for k in 0..4 {
                    out[o + k] += px[s + k] * 0.25;
                }
            }
        }
    }
    (out, nw, nh)
}

/// The SH9 basis, in the order and scale the mesh shader evaluates.
fn sh_basis(d: Vec3) -> [f32; 9] {
    [
        0.282095,
        0.488603 * d.y,
        0.488603 * d.z,
        0.488603 * d.x,
        1.092548 * d.x * d.y,
        1.092548 * d.y * d.z,
        0.315392 * (3.0 * d.z * d.z - 1.0),
        1.092548 * d.x * d.z,
        0.546274 * (d.x * d.x - d.y * d.y),
    ]
}

/// Equirect texel direction, matching `env_spec` in mesh.wgsl.
fn equirect_dir(u: f32, v: f32) -> Vec3 {
    let (phi, theta) = ((u - 0.5) * 2.0 * PI, v * PI);
    Vec3::new(theta.sin() * phi.sin(), theta.cos(), -theta.sin() * phi.cos())
}

/// Irradiance / pi of an equirect map as SH9 (cosine lobe convolved).
fn project_sh(w: u32, h: u32, px: &[f32]) -> [[f32; 4]; 9] {
    let mut c = [[0.0f32; 4]; 9];
    for y in 0..h {
        let v = (y as f32 + 0.5) / h as f32;
        let dw = (2.0 * PI / w as f32) * (PI / h as f32) * (v * PI).sin();
        for x in 0..w {
            let basis = sh_basis(equirect_dir((x as f32 + 0.5) / w as f32, v));
            let s = ((y * w + x) * 4) as usize;
            for (ci, b) in c.iter_mut().zip(basis) {
                for k in 0..3 {
                    ci[k] += px[s + k].min(65000.0) * b * dw;
                }
            }
        }
    }
    let band = [1.0, 2.0 / 3.0, 2.0 / 3.0, 2.0 / 3.0, 0.25, 0.25, 0.25, 0.25, 0.25];
    for (ci, a) in c.iter_mut().zip(band) {
        for v in ci.iter_mut() {
            *v *= a;
        }
    }
    c
}

/// f32 -> IEEE half bits, for non-negative radiance (clamped to the half range).
fn f16(v: f32) -> u16 {
    if !(v > 0.0) {
        return 0;
    }
    let bits = v.min(65000.0).to_bits();
    let exp = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mant = bits & 0x7f_ffff;
    if exp <= 0 {
        if exp < -10 {
            return 0;
        }
        return ((mant | 0x80_0000) >> (14 - exp)) as u16;
    }
    (((exp as u32) << 10) | (mant >> 13)) as u16
}

/// View-projections of the shadow cube's six faces (+x -x +y -y +z -z), each
/// 100 degrees wide so soft-shadow taps near an edge stay on the face.
pub(crate) fn shadow_faces(light: Vec3) -> [Mat4; 6] {
    let proj = perspective_rh(100f32.to_radians(), 1.0, SHADOW_NEAR, SHADOW_FAR);
    let faces = [(Vec3::X, Vec3::Y), (-Vec3::X, Vec3::Y), (Vec3::Y, Vec3::Z), (-Vec3::Y, Vec3::Z), (Vec3::Z, Vec3::Y), (-Vec3::Z, Vec3::Y)];
    faces.map(|(dir, up)| proj * look_at_rh(light, light + dir, up))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats() {
        assert_eq!(f16(1.0), 0x3c00);
        assert_eq!(f16(0.5), 0x3800);
        assert_eq!(f16(-3.0), 0);
        assert_eq!(f16(65504.0 * 2.0), f16(65000.0));
    }

    #[test]
    fn uniform_white_env_gives_unit_irradiance() {
        let (w, h) = (64, 32);
        let sh = project_sh(w, h, &vec![1.0; (w * h * 4) as usize]);
        // Only the constant band survives: irradiance / pi of a white sky is 1.
        let e = sh[0][0] * 0.282095;
        assert!((e - 1.0).abs() < 0.01, "{e}");
        assert!(sh[1][0].abs() < 0.01 && sh[6][0].abs() < 0.01);
    }
}
