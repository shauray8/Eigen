//! glTF / GLB models: the triangles of the default scene with node transforms
//! applied, one part per material, each with that material's maps.

use std::path::Path;

use glam::{Mat3, Mat4, Vec3};
use gltf::image::Format;

use crate::prims::Mesh;

/// RGBA8 pixels, rows top-down.
pub struct Image {
    pub rgba: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

/// A material's maps, laid out like a texture set: color (sRGB, base color
/// factor baked in), tangent-space normal (+Y), roughness in green.
pub struct Maps {
    pub color: Option<Image>,
    pub normal: Option<Image>,
    pub rough: Option<Image>,
}

/// Triangles sharing one material; `maps` is None when the material gives nothing to draw with.
pub struct Part {
    pub mesh: Mesh,
    pub maps: Option<Maps>,
}

/// Loads a `.gltf` (with its files) or `.glb`. glTF UVs are already top-down, like
/// the renderer's (OBJ ones are flipped on load). Missing normals become face normals.
pub fn load_gltf(path: &Path) -> Option<Vec<Part>> {
    let (doc, buffers, images) = gltf::import(path).ok()?;
    let scene = doc.default_scene().or_else(|| doc.scenes().next())?;
    let mut groups: Vec<(Option<usize>, Mesh)> = Vec::new();
    let mut stack: Vec<(gltf::Node, Mat4)> = scene.nodes().map(|n| (n, Mat4::IDENTITY)).collect();
    while let Some((node, parent)) = stack.pop() {
        let world = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
        stack.extend(node.children().map(|c| (c, world)));
        let Some(mesh) = node.mesh() else { continue };
        let normal_m = Mat3::from_mat4(world).inverse().transpose();
        let mirrored = world.determinant() < 0.0;
        for prim in mesh.primitives() {
            if prim.mode() != gltf::mesh::Mode::Triangles {
                continue;
            }
            let r = prim.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
            let Some(pos) = r.read_positions() else { continue };
            let pos: Vec<Vec3> = pos.map(|p| world.transform_point3(Vec3::from(p))).collect();
            let nrm: Option<Vec<Vec3>> = r.read_normals().map(|n| n.map(|n| (normal_m * Vec3::from(n)).normalize_or_zero()).collect());
            let uv: Vec<[f32; 2]> = r.read_tex_coords(0).map(|t| t.into_f32().collect()).unwrap_or_default();
            let mut idx: Vec<u32> = match r.read_indices() {
                Some(i) => i.into_u32().collect(),
                None => (0..pos.len() as u32).collect(),
            };
            idx.truncate(idx.len() / 3 * 3);
            if idx.iter().any(|&i| i as usize >= pos.len()) {
                continue;
            }
            if mirrored {
                idx.chunks_exact_mut(3).for_each(|t| t.swap(1, 2));
            }
            let key = prim.material().index();
            let at = match groups.iter().position(|g| g.0 == key) {
                Some(k) => k,
                None => {
                    groups.push((key, Default::default()));
                    groups.len() - 1
                }
            };
            let (v, out) = &mut groups[at].1;
            let t = |k: u32| uv.get(k as usize).copied().unwrap_or([0.0, 0.0]);
            let vert = |p: Vec3, n: Vec3, t: [f32; 2]| [p.x, p.y, p.z, n.x, n.y, n.z, t[0], t[1]];
            match &nrm {
                Some(nrm) => {
                    let base = v.len() as u32;
                    for k in 0..pos.len() as u32 {
                        v.push(vert(pos[k as usize], nrm.get(k as usize).copied().unwrap_or(Vec3::Y), t(k)));
                    }
                    out.extend(idx.iter().map(|i| base + i));
                }
                None => {
                    for tri in idx.chunks_exact(3) {
                        let [a, b, c] = [0, 1, 2].map(|j| pos[tri[j] as usize]);
                        let n = (b - a).cross(c - a).normalize_or_zero();
                        for &k in tri {
                            out.push(v.len() as u32);
                            v.push(vert(pos[k as usize], n, t(k)));
                        }
                    }
                }
            }
        }
    }
    let parts: Vec<Part> = groups
        .into_iter()
        .filter(|g| !g.1.1.is_empty())
        .map(|(key, mesh)| Part { mesh, maps: key.and_then(|k| doc.materials().nth(k)).and_then(|m| maps(&m, &images)) })
        .collect();
    (!parts.is_empty()).then_some(parts)
}

fn maps(m: &gltf::Material, images: &[gltf::image::Data]) -> Option<Maps> {
    let img = |t: gltf::Texture| images.get(t.source().index()).and_then(rgba);
    let pbr = m.pbr_metallic_roughness();
    let mut color = pbr.base_color_texture().and_then(|t| img(t.texture()));
    let f = pbr.base_color_factor();
    let opaque = m.alpha_mode() == gltf::material::AlphaMode::Opaque;
    if color.is_some() || f != [1.0; 4] {
        // The factor is linear; on sRGB bytes it scales by about its 1/2.2 power.
        let k = [f[0].powf(1.0 / 2.2), f[1].powf(1.0 / 2.2), f[2].powf(1.0 / 2.2), if opaque { 1.0 } else { f[3] }];
        let c = color.get_or_insert_with(|| Image { rgba: vec![255; 4], w: 1, h: 1 });
        for px in c.rgba.chunks_exact_mut(4) {
            for ch in 0..3 {
                px[ch] = (px[ch] as f32 * k[ch]).round() as u8;
            }
            px[3] = if opaque { 255 } else { (px[3] as f32 * k[3]).round() as u8 };
        }
    }
    let normal = m.normal_texture().and_then(|t| img(t.texture()));
    let mut rough = pbr.metallic_roughness_texture().and_then(|t| img(t.texture()));
    let rf = pbr.roughness_factor();
    if let Some(r) = rough.as_mut().filter(|_| rf != 1.0) {
        for px in r.rgba.chunks_exact_mut(4) {
            px[1] = (px[1] as f32 * rf).round() as u8;
        }
    }
    (color.is_some() || normal.is_some() || rough.is_some()).then_some(Maps { color, normal, rough })
}

/// Decoded glTF image as RGBA8 (16-bit channels keep their high byte; float images are skipped).
fn rgba(d: &gltf::image::Data) -> Option<Image> {
    let (channels, wide) = match d.format {
        Format::R8 => (1, false),
        Format::R8G8 => (2, false),
        Format::R8G8B8 => (3, false),
        Format::R8G8B8A8 => (4, false),
        Format::R16 => (1, true),
        Format::R16G16 => (2, true),
        Format::R16G16B16 => (3, true),
        Format::R16G16B16A16 => (4, true),
        _ => return None,
    };
    let size = if wide { 2 } else { 1 };
    let mut out = Vec::with_capacity(d.width as usize * d.height as usize * 4);
    for p in d.pixels.chunks_exact(channels * size) {
        let c = |i: usize| if wide { (u16::from_ne_bytes([p[2 * i], p[2 * i + 1]]) >> 8) as u8 } else { p[i] };
        out.extend(match channels {
            1 => [c(0), c(0), c(0), 255],
            2 => [c(0), c(0), c(0), c(1)],
            3 => [c(0), c(1), c(2), 255],
            _ => [c(0), c(1), c(2), c(3)],
        });
    }
    (out.len() == d.width as usize * d.height as usize * 4).then_some(Image { rgba: out, w: d.width, h: d.height })
}

#[cfg(test)]
mod tests {
    /// One triangle (positions + UVs, no normals or indices) in a data-URI buffer,
    /// under a node moved by (1,2,3), with a red base color factor.
    const TRIANGLE: &str = r#"{
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"mesh": 0, "translation": [1, 2, 3]}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "TEXCOORD_0": 1}, "material": 0}]}],
        "materials": [{"pbrMetallicRoughness": {"baseColorFactor": [1, 0, 0, 1]}}],
        "buffers": [{"byteLength": 60, "uri": "data:application/octet-stream;base64,AAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAgD8AAAAAAAAAAAAAgD8AAIA/AACAPwAAAAAAAAAA"}],
        "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": 36}, {"buffer": 0, "byteOffset": 36, "byteLength": 24}],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0, 0, 0], "max": [1, 1, 0]},
            {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC2"}
        ]
    }"#;

    #[test]
    fn gltf_triangle_with_transform_flat_normal_and_color() {
        let dir = std::env::temp_dir().join(format!("ed-gltf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tri.gltf");
        std::fs::write(&path, TRIANGLE).unwrap();
        let parts = super::load_gltf(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(parts.len(), 1);
        let (v, i) = &parts[0].mesh;
        assert_eq!((v.len(), i.clone()), (3, vec![0, 1, 2]));
        assert_eq!(v[1][..3], [2.0, 2.0, 3.0]); // translated
        assert_eq!(v[0][3..6], [0.0, 0.0, 1.0]); // face normal
        assert_eq!(v[0][6..], [0.0, 1.0]); // UVs as stored (already top-down)
        let color = parts[0].maps.as_ref().unwrap().color.as_ref().unwrap();
        assert_eq!((color.w, color.h, &color.rgba[..]), (1, 1, &[255, 0, 0, 255][..]));
    }

    /// Live: fetches Poly Haven's `studio_small_09` HDRI and `potted_plant_04` model
    /// into `$TMPDIR/ed-polyhaven` (kept, so it can be rendered) and loads the model
    /// (`cargo test -p ed-render -- --ignored --nocapture polyhaven`).
    #[test]
    #[ignore]
    fn polyhaven_fetch_and_load() {
        use ed_media::web::{PolyHaven, polyhaven_fetch};
        let dir = std::env::temp_dir().join("ed-polyhaven");
        let env = polyhaven_fetch(&dir, PolyHaven::Hdri, "studio_small_09", "1k").unwrap();
        let src = polyhaven_fetch(&dir, PolyHaven::Model, "potted_plant_04", "1k").unwrap();
        println!("scene env={env} ({} bytes)", std::fs::metadata(dir.join(&env)).unwrap().len());
        let parts = super::load_gltf(&dir.join(&src)).unwrap();
        for (k, p) in parts.iter().enumerate() {
            let m = p.maps.as_ref();
            let has = |i: Option<&super::Image>| i.map_or("-".to_string(), |i| format!("{}x{}", i.w, i.h));
            println!(
                "model src={src} part {k}: {} vertices, {} triangles, color {} normal {} rough {}",
                p.mesh.0.len(),
                p.mesh.1.len() / 3,
                has(m.and_then(|m| m.color.as_ref())),
                has(m.and_then(|m| m.normal.as_ref())),
                has(m.and_then(|m| m.rough.as_ref())),
            );
        }
        assert!(parts.iter().any(|p| p.maps.as_ref().is_some_and(|m| m.color.is_some())));
    }
}
