//! Renderer (wgpu: Metal on macOS, Vulkan/DX12 elsewhere). One code path for
//! the interactive viewport and for frames the agent asks to see, so what the
//! agent checks is exactly what the human sees.
//!
//! The canvas is the film frame. Canvas layers (shapes, text, views) are
//! composited back to front; a view layer first renders the 3D world through
//! its camera into its own texture.
//!
//! Coordinates: 3D is Y-up, -Z forward (glTF / three.js), unit primitives span
//! -0.5..0.5. The canvas is in scene-resolution pixels, origin top-left.

use std::f32::consts::PI;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use bytemuck::{Pod, Zeroable};
use ed_core::data::{Align, Fit, FxKind, Kind};
use ed_core::eval::{EvalCamera, EvalFx, EvalMedia, EvalPoints, EvalScene, EvalText, Layer2D};
use ed_text::Fonts;
use glam::{Mat3, Mat4, Vec2, Vec3};
use wgpu::util::DeviceExt;

mod cine;
pub mod plugins;
mod model;
mod prims;

pub use cine::Lens;
use cine::{EnvGpu, SHADOW_SIZE, WorldTarget};

pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// Non-sRGB view of the same bytes, for UIs (egui) that composite in gamma space.
pub const UI_VIEW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Linear HDR the 3D world is lit into, before the post pass tonemaps it.
const HDR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const SAMPLES: u32 = 4;
const MAX_LIGHTS: usize = 8;
const FUR_SHELLS: usize = 28;

/// Where the 3D world is seen from.
#[derive(Clone, Copy, Debug)]
pub enum ViewCam {
    /// A free orbit view for building the world.
    Orbit { target: Vec3, yaw: f32, pitch: f32, dist: f32 },
    Camera { world: Mat4, fov_deg: f32, lens: Lens },
}

impl ViewCam {
    pub fn default_orbit() -> Self {
        ViewCam::Orbit { target: Vec3::ZERO, yaw: 0.6, pitch: 0.35, dist: 8.0 }
    }

    pub fn of(camera: Option<&EvalCamera>) -> Self {
        camera.map_or(Self::default_orbit(), |c| ViewCam::Camera {
            world: c.world,
            fov_deg: c.fov_deg,
            lens: Lens { focus: c.focus, aperture: c.aperture, haze: c.haze, haze_color: c.haze_color },
        })
    }

    /// (view matrix, eye position, vertical fov in degrees)
    fn resolve(self) -> (Mat4, Vec3, f32) {
        match self {
            ViewCam::Camera { world, fov_deg, .. } => (world.inverse(), world.transform_point3(Vec3::ZERO), fov_deg),
            ViewCam::Orbit { target, yaw, pitch, dist } => {
                let eye = target + dist * Vec3::new(pitch.cos() * yaw.sin(), pitch.sin(), pitch.cos() * yaw.cos());
                (look_at_rh(eye, target, Vec3::Y), eye, 40.0)
            }
        }
    }
}

fn look_at_rh(eye: Vec3, center: Vec3, up: Vec3) -> Mat4 {
    let f = (center - eye).normalize();
    let s = f.cross(up).normalize();
    let u = s.cross(f);
    Mat4::from_cols(
        glam::Vec4::new(s.x, u.x, -f.x, 0.0),
        glam::Vec4::new(s.y, u.y, -f.y, 0.0),
        glam::Vec4::new(s.z, u.z, -f.z, 0.0),
        glam::Vec4::new(-s.dot(eye), -u.dot(eye), f.dot(eye), 1.0),
    )
}

/// Right-handed perspective with wgpu's 0..1 depth range.
fn perspective_rh(fovy: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
    let f = 1.0 / (fovy / 2.0).tan();
    let r = far / (near - far);
    Mat4::from_cols(
        glam::Vec4::new(f / aspect, 0.0, 0.0, 0.0),
        glam::Vec4::new(0.0, f, 0.0, 0.0),
        glam::Vec4::new(0.0, 0.0, r, -1.0),
        glam::Vec4::new(0.0, 0.0, r * near, 0.0),
    )
}

/// Polls a future that wgpu resolves without a runtime.
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}

/// Rotates a canvas-space vector (y down) by degrees.
fn rot2(v: Vec2, deg: f32) -> Vec2 {
    let (s, c) = (deg * PI / 180.0).sin_cos();
    Vec2::new(v.x * c - v.y * s, v.x * s + v.y * c)
}

/// Quad (two triangles) placing a picture of `aspect` (w/h) in a layer box,
/// cropped (`cover`), letterboxed (`contain`) or stretched to fit.
fn fit_quad(center: [f32; 2], size: [f32; 2], rot_deg: f32, opacity: f32, aspect: f32, fit: Fit) -> [ImageVertex; 6] {
    let (bw, bh) = (size[0], size[1]);
    let (a, b) = (aspect, bw / bh.max(1e-3));
    let (mut half, mut uv0, mut uv1) = (Vec2::new(bw, bh) / 2.0, Vec2::ZERO, Vec2::ONE);
    match fit {
        Fit::Stretch => {}
        Fit::Contain if a > b => half.y = bw / a / 2.0,
        Fit::Contain => half.x = bh * a / 2.0,
        Fit::Cover if a > b => {
            let u = b / a;
            (uv0.x, uv1.x) = ((1.0 - u) / 2.0, (1.0 + u) / 2.0);
        }
        Fit::Cover => {
            let v = a / b;
            (uv0.y, uv1.y) = ((1.0 - v) / 2.0, (1.0 + v) / 2.0);
        }
    }
    let corner = |x: f32, y: f32| {
        let p = Vec2::from(center) + rot2(Vec2::new(x, y) * half, rot_deg);
        let uv = uv0 + (uv1 - uv0) * Vec2::new((x + 1.0) / 2.0, (y + 1.0) / 2.0);
        ImageVertex { pos: p.to_array(), uv: uv.to_array(), alpha: opacity }
    };
    [corner(-1.0, -1.0), corner(1.0, -1.0), corner(1.0, 1.0), corner(-1.0, -1.0), corner(1.0, 1.0), corner(-1.0, 1.0)]
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuLight {
    pos: [f32; 4],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MeshGlobals {
    view_proj: [[f32; 4]; 4],
    cam_pos: [f32; 4],
    ambient: [f32; 4],
    lights: [GpuLight; MAX_LIGHTS],
    /// x: shadow light index (-1 none), y: softness, z: 1 / shadow map size.
    shadow: [f32; 4],
    /// x: has env map, y: env yaw (radians), z: max env mip.
    env: [f32; 4],
    sh: [[f32; 4]; 9],
    shadow_vp: [[[f32; 4]; 4]; 6],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PostUniform {
    res: [f32; 2],
    near: f32,
    far: f32,
    coc_k: f32,
    inv_focus: f32,
    max_coc: f32,
    haze: f32,
    haze_color: [f32; 4],
    bloom: f32,
    lods: f32,
    grain: f32,
    vignette: f32,
    /// Frame (grain seed), unused.
    frame: [f32; 4],
}

/// Consecutive instances drawn with one call.
struct Batch {
    mesh: Arc<GpuMesh>,
    tex: Arc<wgpu::BindGroup>,
    range: std::ops::Range<u32>,
}

fn push_batch(batches: &mut Vec<Batch>, mesh: &Arc<GpuMesh>, tex: &Arc<wgpu::BindGroup>, index: u32) {
    if let Some(b) = batches.last_mut() {
        if b.range.end == index && Arc::ptr_eq(&b.mesh, mesh) && Arc::ptr_eq(&b.tex, tex) {
            b.range.end += 1;
            return;
        }
    }
    batches.push(Batch { mesh: mesh.clone(), tex: tex.clone(), range: index..index + 1 });
}

/// Model files kept on the GPU: a scene's static models plus a few frames of
/// each mesh sequence (those load one file per frame).
const MAX_MODELS: usize = 96;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MeshInstance {
    model: [[f32; 4]; 4],
    /// Normal matrix; `normal[0][3]` is the fur shell (0 = the surface itself).
    normal: [[f32; 4]; 3],
    color: [f32; 4],
    material: [f32; 4],
    /// x: has a texture set, y: uv scale, z: emit (unlit), w: soft (blended).
    tex: [f32; 4],
    /// Wind: amplitude, fade-in heights, seconds.
    sway: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PointsUniform {
    model: [[f32; 4]; 4],
    color: [f32; 4],
    /// Dot diameter in target px, frame, target size.
    params: [f32; 4],
    /// x: lit (scene lights shade the dots), yzw: world pivot the dots face out from.
    shade: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FxUniform {
    res: [f32; 2],
    cell: f32,
    amount: f32,
    kind: f32,
    /// Scene frame (the built-in shader calls it `time`).
    frame: f32,
    /// Seconds since the layer's in point (plugins).
    seconds: f32,
    _pad: f32,
    color: [f32; 4],
    params: [f32; 4],
}

/// What a shader-layer plugin reads (`plugin_layer.wgsl`).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LayerUniform {
    size: [f32; 2],
    seconds: f32,
    frame: f32,
    color: [f32; 4],
    params: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CanvasGlobals {
    res: [f32; 2],
    px: f32,
    _pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ShapeInstance {
    center: [f32; 2],
    size: [f32; 2],
    color: [f32; 4],
    params: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PathVertex {
    pos: [f32; 2],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ImageVertex {
    pos: [f32; 2],
    uv: [f32; 2],
    alpha: f32,
}

struct GpuMesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
}

impl GpuMesh {
    fn new(device: &wgpu::Device, (v, i): &prims::Mesh) -> Self {
        let buffer = |contents: &[u8], usage| device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents, usage });
        GpuMesh {
            vertices: buffer(bytemuck::cast_slice(v), wgpu::BufferUsages::VERTEX),
            indices: buffer(bytemuck::cast_slice(i), wgpu::BufferUsages::INDEX),
            count: i.len() as u32,
        }
    }
}

/// A mesh object's parts: one for a primitive or OBJ, one per material for a glTF,
/// each with that material's own maps (used when the object sets no `tex`).
type GpuModel = Vec<(Arc<GpuMesh>, Option<Arc<wgpu::BindGroup>>)>;

/// A render target. Owned by the caller so the viewport and agent renders
/// don't fight over one texture.
pub struct Target {
    pub size: [u32; 2],
    msaa: wgpu::TextureView,
    depth: wgpu::TextureView,
    pub texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl Target {
    pub fn new(device: &wgpu::Device, size: [u32; 2]) -> Self {
        let size = [size[0].max(1), size[1].max(1)];
        let extent = wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 };
        let tex = |label, format, samples, usage, view_formats: &[wgpu::TextureFormat]| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats,
            })
        };
        let att = wgpu::TextureUsages::RENDER_ATTACHMENT;
        let msaa = tex("msaa", FORMAT, SAMPLES, att, &[]).create_view(&Default::default());
        let depth = tex("depth", DEPTH, SAMPLES, att, &[]).create_view(&Default::default());
        let texture = tex(
            "output",
            FORMAT,
            1,
            att | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            &[UI_VIEW_FORMAT],
        );
        let view = texture.create_view(&Default::default());
        Self { size, msaa, depth, texture, view }
    }

    /// View for gamma-space UIs.
    pub fn ui_view(&self) -> wgpu::TextureView {
        self.texture.create_view(&wgpu::TextureViewDescriptor { format: Some(UI_VIEW_FORMAT), ..Default::default() })
    }
}

enum Run {
    Shapes(std::ops::Range<u32>),
    Paths(std::ops::Range<u32>),
    Image(usize, std::ops::Range<u32>),
}

/// A stretch of canvas layers drawn in one pass, optionally followed by an effect.
struct Segment {
    runs: Vec<Run>,
    /// The effect closing the segment, and its plugin pipeline (None: built-in).
    fx: Option<(EvalFx, Option<wgpu::RenderPipeline>)>,
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    mesh_pipeline: wgpu::RenderPipeline,
    glass_pipeline: wgpu::RenderPipeline,
    points_pipeline: wgpu::RenderPipeline,
    points_layout: wgpu::BindGroupLayout,
    /// Point cloud files by `src`: (buffer, point count).
    point_sets: Mutex<HashMap<String, Option<(Arc<wgpu::Buffer>, u32)>>>,
    fx_pipeline: wgpu::RenderPipeline,
    fx_uniform: wgpu::Buffer,
    fx_bind: wgpu::BindGroup,
    /// Layouts shader plugins are built against, their compiled pipelines,
    /// and the uniform a shader layer reads.
    fx_layout: wgpu::PipelineLayout,
    layer_layout: wgpu::PipelineLayout,
    layer_uniform: wgpu::Buffer,
    layer_bind: wgpu::BindGroup,
    plugins: Mutex<plugins::Cache>,
    shape_pipeline: wgpu::RenderPipeline,
    path_pipeline: wgpu::RenderPipeline,
    image_pipeline: wgpu::RenderPipeline,
    image_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    mesh_globals: wgpu::Buffer,
    canvas_globals: wgpu::Buffer,
    mesh_layout: wgpu::BindGroupLayout,
    canvas_bind: wgpu::BindGroup,
    shadow_pipeline: wgpu::RenderPipeline,
    /// Per cube face: (view-projection buffer, its bind group).
    shadow_faces: Vec<(wgpu::Buffer, wgpu::BindGroup)>,
    shadow_layers: Vec<wgpu::TextureView>,
    shadow_array: wgpu::TextureView,
    shadow_sampler: wgpu::Sampler,
    env_sampler: wgpu::Sampler,
    default_env: Arc<EnvGpu>,
    /// Environment maps by `src` (None = failed to load).
    envs: Mutex<HashMap<String, Option<Arc<EnvGpu>>>>,
    post_pipeline: wgpu::RenderPipeline,
    post_uniform: wgpu::Buffer,
    post_bind: wgpu::BindGroup,
    post_layout: wgpu::BindGroupLayout,
    down_pipeline: wgpu::RenderPipeline,
    /// Export motion blur: adds a sub-frame into a float target at the blend
    /// constant's weight, and copies the sum back into an sRGB target.
    accum_pipeline: wgpu::RenderPipeline,
    copy_pipeline: wgpu::RenderPipeline,
    /// HDR targets the world is lit into, by size.
    world_targets: Mutex<Vec<WorldTarget>>,
    meshes: Vec<(Kind, Arc<GpuModel>)>,
    /// Model files (OBJ, glTF) by project-relative path (None = failed to load),
    /// oldest first evicted (mesh sequences load one file per frame).
    models: Mutex<(HashMap<String, Option<Arc<GpuModel>>>, std::collections::VecDeque<String>)>,
    fonts: Mutex<Fonts>,
    /// Offscreen targets for view layers, reused across frames.
    view_targets: Mutex<Vec<Target>>,
    /// Free offscreen targets for composition clips (taken while in use, so
    /// clips inside clips never share one).
    comp_targets: Mutex<Vec<Target>>,
    /// Ping-pong textures for effect passes.
    fx_targets: Mutex<Vec<(wgpu::Texture, wgpu::TextureView)>>,
    tex_layout: wgpu::BindGroupLayout,
    tex_sampler: wgpu::Sampler,
    default_tex: Arc<wgpu::BindGroup>,
    /// Texture sets by name (None: failed to load, don't retry every frame).
    textures: Mutex<HashMap<String, Option<Arc<wgpu::BindGroup>>>>,
    /// Image and video layers' textures by `src`.
    media: Mutex<HashMap<String, Option<MediaTex>>>,
    /// Image and video files used as a mesh's color map (`tex=media/x.png`).
    mesh_media: Mutex<HashMap<String, Option<MeshMedia>>>,
    /// Project directory that `src` and texture names resolve against.
    root: Mutex<PathBuf>,
}

/// A media file as a mesh texture set (color with alpha; neutral normal and roughness).
struct MeshMedia {
    texture: wgpu::Texture,
    bind: Arc<wgpu::BindGroup>,
    video: Option<ed_media::VideoReader>,
}

struct MediaTex {
    texture: wgpu::Texture,
    bind: Arc<wgpu::BindGroup>,
    size: [u32; 2],
    video: Option<ed_media::VideoReader>,
}

fn uniform_bind(device: &wgpu::Device, size: u64) -> (wgpu::Buffer, wgpu::BindGroupLayout, wgpu::BindGroup) {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &layout,
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() }],
    });
    (buffer, layout, bind)
}

struct PipelineSpec<'a> {
    module: &'a wgpu::ShaderModule,
    vs: &'a str,
    fs: &'a str,
    layouts: &'a [&'a wgpu::BindGroupLayout],
    buffers: &'a [wgpu::VertexBufferLayout<'a>],
    depth_write: bool,
    /// Test against depth even without writing (transparent 3D).
    depth_test: bool,
    blend: wgpu::BlendState,
    cull: Option<wgpu::Face>,
    format: wgpu::TextureFormat,
}

fn pipeline(device: &wgpu::Device, spec: PipelineSpec) -> wgpu::RenderPipeline {
    let layouts: Vec<Option<&wgpu::BindGroupLayout>> = spec.layouts.iter().map(|l| Some(*l)).collect();
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &layouts,
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: spec.module,
            entry_point: Some(spec.vs),
            compilation_options: Default::default(),
            buffers: &spec.buffers.iter().cloned().map(Some).collect::<Vec<_>>(),
        },
        primitive: wgpu::PrimitiveState { cull_mode: spec.cull, ..Default::default() },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH,
            depth_write_enabled: Some(spec.depth_write),
            depth_compare: Some(if spec.depth_write || spec.depth_test {
                wgpu::CompareFunction::Less
            } else {
                wgpu::CompareFunction::Always
            }),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: wgpu::MultisampleState { count: SAMPLES, ..Default::default() },
        fragment: Some(wgpu::FragmentState {
            module: spec.module,
            entry_point: Some(spec.fs),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: spec.format,
                blend: Some(spec.blend),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn shader(device: &wgpu::Device, source: &str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(source.into()),
    })
}

/// Uploads RGBA8 pixels, optionally with a full mip chain (for tiled textures).
fn upload(device: &wgpu::Device, queue: &wgpu::Queue, rgba: &[u8], w: u32, h: u32, srgb: bool, mips: bool) -> wgpu::Texture {
    let levels = if mips { 32 - w.max(h).leading_zeros() } else { 1 };
    let format = if srgb { FORMAT } else { wgpu::TextureFormat::Rgba8Unorm };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut level = image::RgbaImage::from_raw(w, h, rgba.to_vec()).expect("pixel count");
    for mip in 0..levels {
        let (lw, lh) = level.dimensions();
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: mip, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            level.as_raw(),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(lw * 4), rows_per_image: None },
            wgpu::Extent3d { width: lw, height: lh, depth_or_array_layers: 1 },
        );
        if mip + 1 < levels {
            level = image::imageops::resize(&level, (lw / 2).max(1), (lh / 2).max(1), image::imageops::FilterType::Triangle);
        }
    }
    texture
}

fn tex_set_bind(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    maps: [&wgpu::Texture; 3],
) -> wgpu::BindGroup {
    let views: Vec<wgpu::TextureView> = maps.iter().map(|t| t.create_view(&Default::default())).collect();
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("texture set"),
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&views[0]) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&views[1]) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&views[2]) },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(sampler) },
        ],
    })
}

impl Renderer {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let mesh_globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mesh globals"),
            size: std::mem::size_of::<MeshGlobals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let frag = wgpu::ShaderStages::FRAGMENT;
        let mesh_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mesh globals"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: frag,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: frag,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: frag,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: frag,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let (canvas_globals, canvas_layout, canvas_bind) =
            uniform_bind(&device, std::mem::size_of::<CanvasGlobals>() as u64);
        let image_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let mesh_module = shader(&device, include_str!("mesh.wgsl"));
        let shape_module = shader(&device, include_str!("shape.wgsl"));
        let canvas_module = shader(&device, include_str!("canvas.wgsl"));
        let tex_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("texture set"),
            entries: &[
                tex_entry(0),
                tex_entry(1),
                tex_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let tex_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });
        let mesh_buffers = [
            wgpu::VertexBufferLayout {
                array_stride: 32,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 11 => Float32x2],
            },
            wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<MeshInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![
                    2 => Float32x4, 3 => Float32x4, 4 => Float32x4, 5 => Float32x4,
                    6 => Float32x4, 7 => Float32x4, 8 => Float32x4, 9 => Float32x4, 10 => Float32x4,
                    12 => Float32x4, 13 => Float32x4
                ],
            },
        ];
        let mesh_pipeline = pipeline(
            &device,
            PipelineSpec {
                module: &mesh_module,
                vs: "vs",
                fs: "fs",
                layouts: &[&mesh_layout, &tex_layout],
                buffers: &mesh_buffers,
                depth_write: true,
                depth_test: true,
                blend: wgpu::BlendState::REPLACE,
                format: HDR,
                cull: Some(wgpu::Face::Back),
            },
        );
        let glass_pipeline = pipeline(
            &device,
            PipelineSpec {
                module: &mesh_module,
                vs: "vs",
                fs: "fs",
                layouts: &[&mesh_layout, &tex_layout],
                buffers: &mesh_buffers,
                depth_write: false,
                depth_test: true,
                blend: wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
                format: HDR,
                cull: Some(wgpu::Face::Back),
            },
        );
        let (_, points_layout, _) = uniform_bind(&device, std::mem::size_of::<PointsUniform>() as u64);
        let points_module = shader(&device, include_str!("points.wgsl"));
        let points_pipeline = pipeline(
            &device,
            PipelineSpec {
                module: &points_module,
                vs: "vs",
                fs: "fs",
                layouts: &[&mesh_layout, &points_layout],
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: 16,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x4],
                }],
                depth_write: true,
                depth_test: true,
                blend: wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
                format: HDR,
                cull: None,
            },
        );
        let (fx_uniform, fx_layout, fx_bind) = uniform_bind(&device, std::mem::size_of::<FxUniform>() as u64);
        let fx_module = shader(&device, include_str!("fx.wgsl"));
        let fx_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&fx_layout), Some(&image_layout)],
            immediate_size: 0,
        });
        let (layer_uniform, layer_bgl, layer_bind) = uniform_bind(&device, std::mem::size_of::<LayerUniform>() as u64);
        let layer_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layer_bgl)],
            immediate_size: 0,
        });
        let fx_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fx"),
            layout: Some(&fx_layout),
            vertex: wgpu::VertexState {
                module: &fx_module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &fx_module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let shape_pipeline = pipeline(
            &device,
            PipelineSpec {
                module: &shape_module,
                vs: "vs",
                fs: "fs",
                layouts: &[&canvas_layout],
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<ShapeInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32x4],
                }],
                depth_write: false,
                depth_test: false,
                blend: wgpu::BlendState::ALPHA_BLENDING,
                format: FORMAT,
                cull: None,
            },
        );
        let path_pipeline = pipeline(
            &device,
            PipelineSpec {
                module: &canvas_module,
                vs: "path_vs",
                fs: "path_fs",
                layouts: &[&canvas_layout],
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<PathVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4],
                }],
                depth_write: false,
                depth_test: false,
                blend: wgpu::BlendState::ALPHA_BLENDING,
                format: FORMAT,
                cull: None,
            },
        );
        let image_pipeline = pipeline(
            &device,
            PipelineSpec {
                module: &canvas_module,
                vs: "image_vs",
                fs: "image_fs",
                layouts: &[&canvas_layout, &image_layout],
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<ImageVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32],
                }],
                depth_write: false,
                depth_test: false,
                blend: wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
                format: FORMAT,
                cull: None,
            },
        );

        // Shadow cube: six depth layers, rendered with the instance layout of meshes.
        let shadow_module = shader(&device, include_str!("shadow.wgsl"));
        let shadow_faces: Vec<(wgpu::Buffer, wgpu::BindGroup)> =
            (0..6).map(|_| { let (b, _, g) = uniform_bind(&device, 64); (b, g) }).collect();
        let (_, shadow_face_layout, _) = uniform_bind(&device, 64);
        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow"),
            layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&shadow_face_layout), Some(&tex_layout)],
                immediate_size: 0,
            })),
            vertex: wgpu::VertexState {
                module: &shadow_module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &mesh_buffers.iter().cloned().map(Some).collect::<Vec<_>>(),
            },
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: wgpu::DepthBiasState { constant: 2, slope_scale: 2.5, clamp: 0.0 },
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shadow_module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[],
            }),
            multiview_mask: None,
            cache: None,
        });
        let shadow_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shadow cube"),
            size: wgpu::Extent3d { width: SHADOW_SIZE, height: SHADOW_SIZE, depth_or_array_layers: 6 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let shadow_layers = (0..6)
            .map(|l| {
                shadow_tex.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: l,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let shadow_array = shadow_tex.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let env_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let default_env = Arc::new(cine::upload_env(&device, &queue, 1, 1, vec![0.0; 4]));

        // Post: lens + film for each rendered view, and the bloom mip chain.
        let (post_uniform, post_globals_layout, post_bind) =
            uniform_bind(&device, std::mem::size_of::<PostUniform>() as u64);
        let post_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post inputs"),
            entries: &[
                tex_entry(0),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: frag,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: true,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: frag,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let fullscreen = |module: &wgpu::ShaderModule, fs: &str, layouts: &[Option<&wgpu::BindGroupLayout>], format| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(fs),
                layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: None,
                    bind_group_layouts: layouts,
                    immediate_size: 0,
                })),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let post_module = shader(&device, include_str!("post.wgsl"));
        let post_pipeline = fullscreen(&post_module, "fs", &[Some(&post_globals_layout), Some(&post_layout)], FORMAT);
        let down_module = shader(&device, include_str!("down.wgsl"));
        let down_pipeline = fullscreen(&down_module, "down", &[Some(&image_layout)], HDR);
        let accum_module = shader(&device, include_str!("accum.wgsl"));
        let copy_pipeline = fullscreen(&accum_module, "fs", &[Some(&image_layout)], FORMAT);
        let weighted = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Constant,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        };
        let accum_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("accum"),
            layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&image_layout)],
                immediate_size: 0,
            })),
            vertex: wgpu::VertexState { module: &accum_module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &accum_module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: HDR,
                    blend: Some(wgpu::BlendState { color: weighted, alpha: weighted }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let meshes = [Kind::Cube, Kind::Sphere, Kind::Plane, Kind::Cylinder]
            .into_iter()
            .map(|k| (k, Arc::new(vec![(Arc::new(GpuMesh::new(&device, &prims::build(k))), None)])))
            .collect();

        let neutral = |rgba: [u8; 4], srgb: bool| upload(&device, &queue, &[rgba[0], rgba[1], rgba[2], rgba[3]], 1, 1, srgb, false);
        let default_tex = Arc::new(tex_set_bind(
            &device,
            &tex_layout,
            &tex_sampler,
            [&neutral([255; 4], true), &neutral([128, 128, 255, 255], false), &neutral([255; 4], false)],
        ));
        Self {
            tex_layout,
            tex_sampler,
            default_tex,
            textures: Mutex::new(HashMap::new()),
            media: Mutex::new(HashMap::new()),
            mesh_media: Mutex::new(HashMap::new()),
            root: Mutex::new(PathBuf::from(".")),
            device,
            queue,
            mesh_pipeline,
            glass_pipeline,
            points_pipeline,
            points_layout,
            point_sets: Mutex::new(HashMap::new()),
            fx_pipeline,
            fx_uniform,
            fx_bind,
            fx_layout,
            layer_layout,
            layer_uniform,
            layer_bind,
            plugins: Mutex::new(plugins::Cache::default()),
            shape_pipeline,
            path_pipeline,
            image_pipeline,
            image_layout,
            sampler,
            mesh_globals,
            canvas_globals,
            mesh_layout,
            canvas_bind,
            shadow_pipeline,
            shadow_faces,
            shadow_layers,
            shadow_array,
            shadow_sampler,
            env_sampler,
            default_env,
            envs: Mutex::new(HashMap::new()),
            post_pipeline,
            post_uniform,
            post_bind,
            post_layout,
            down_pipeline,
            accum_pipeline,
            copy_pipeline,
            world_targets: Mutex::new(Vec::new()),
            meshes,
            models: Mutex::new(Default::default()),
            fonts: Mutex::new(Fonts::default()),
            view_targets: Mutex::new(Vec::new()),
            comp_targets: Mutex::new(Vec::new()),
            fx_targets: Mutex::new(Vec::new()),
        }
    }

    /// A renderer on the default GPU without a window (tests, command-line renders).
    pub fn headless() -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = block_on(instance.request_adapter(&Default::default())).ok()?;
        let (device, queue) = block_on(adapter.request_device(&Default::default())).ok()?;
        Some(Self::new(device, queue))
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Project directory for media `src` paths and texture sets.
    pub fn set_root(&self, dir: PathBuf) {
        *self.root.lock().unwrap() = dir;
        self.envs.lock().unwrap().clear();
        self.point_sets.lock().unwrap().clear();
        *self.models.lock().unwrap() = Default::default();
        self.textures.lock().unwrap().clear();
        self.media.lock().unwrap().clear();
        self.mesh_media.lock().unwrap().clear();
    }

    /// Forget cached media so changed files are reloaded.
    pub fn reload_media(&self) {
        self.envs.lock().unwrap().clear();
        self.point_sets.lock().unwrap().clear();
        *self.models.lock().unwrap() = Default::default();
        self.textures.lock().unwrap().clear();
        self.media.lock().unwrap().clear();
        self.mesh_media.lock().unwrap().clear();
    }

    /// GPU parts of a primitive kind, or of a model's OBJ / glTF / GLB file.
    fn mesh_for(&self, m: &ed_core::eval::EvalMesh) -> Option<Arc<GpuModel>> {
        if m.kind != Kind::Model {
            return self.meshes.iter().find(|(k, _)| *k == m.kind).map(|(_, g)| g.clone());
        }
        let mut guard = self.models.lock().unwrap();
        let (cache, order) = &mut *guard;
        if let Some(g) = cache.get(&m.src) {
            return g.clone();
        }
        while order.len() >= MAX_MODELS {
            if let Some(old) = order.pop_front() {
                cache.remove(&old);
            }
        }
        let path = self.root.lock().unwrap().join(&m.src);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_lowercase();
        let parts = if ext == "gltf" || ext == "glb" {
            model::load_gltf(&path)
        } else {
            std::fs::read_to_string(&path).ok().and_then(|t| prims::load_obj(&t)).map(|mesh| vec![model::Part { mesh, maps: None }])
        };
        let gpu = parts.map(|parts| {
            let load = |img: Option<model::Image>, srgb: bool, fallback: [u8; 4]| match img {
                Some(p) => upload(&self.device, &self.queue, &p.rgba, p.w, p.h, srgb, true),
                None => upload(&self.device, &self.queue, &fallback, 1, 1, srgb, false),
            };
            let gpu: GpuModel = parts
                .into_iter()
                .map(|p| {
                    let bind = p.maps.map(|m| {
                        let maps = [load(m.color, true, [255; 4]), load(m.normal, false, [128, 128, 255, 255]), load(m.rough, false, [255; 4])];
                        Arc::new(tex_set_bind(&self.device, &self.tex_layout, &self.tex_sampler, [&maps[0], &maps[1], &maps[2]]))
                    });
                    (Arc::new(GpuMesh::new(&self.device, &p.mesh)), bind)
                })
                .collect();
            Arc::new(gpu)
        });
        cache.insert(m.src.clone(), gpu.clone());
        order.push_back(m.src.clone());
        gpu
    }

    /// Texture set `textures/<name>/{color,normal,rough}.*`; missing maps fall back to neutral.
    fn texture_set(&self, name: &str) -> Arc<wgpu::BindGroup> {
        if name.is_empty() {
            return self.default_tex.clone();
        }
        let mut cache = self.textures.lock().unwrap();
        if let Some(b) = cache.get(name) {
            return b.clone().unwrap_or_else(|| self.default_tex.clone());
        }
        let dir = self.root.lock().unwrap().join("textures").join(name);
        let load = |stem: &str, srgb: bool, fallback: [u8; 4]| {
            let file = ["jpg", "png", "jpeg"].iter().map(|e| dir.join(format!("{stem}.{e}"))).find(|p| p.exists());
            match file.and_then(|f| ed_media::load_image(&f).ok()) {
                Some(p) => upload(&self.device, &self.queue, &p.rgba, p.w, p.h, srgb, true),
                None => upload(&self.device, &self.queue, &fallback, 1, 1, srgb, false),
            }
        };
        let bind = dir.is_dir().then(|| {
            let maps = [load("color", true, [255; 4]), load("normal", false, [128, 128, 255, 255]), load("rough", false, [255; 4])];
            Arc::new(tex_set_bind(&self.device, &self.tex_layout, &self.tex_sampler, [&maps[0], &maps[1], &maps[2]]))
        });
        cache.insert(name.to_string(), bind.clone());
        bind.unwrap_or_else(|| self.default_tex.clone())
    }

    /// Color map from an image or video file (`tex` with an extension), at `time` seconds.
    /// Alpha is kept straight: it cuts the surface (or blends it, when soft).
    fn mesh_media(&self, src: &str, time: f32) -> Arc<wgpu::BindGroup> {
        let mut cache = self.mesh_media.lock().unwrap();
        if !cache.contains_key(src) {
            let path = self.root.lock().unwrap().join(src);
            let video = ed_media::kind_of(&path) == Some(ed_media::MediaKind::Video);
            let entry = if video {
                ed_media::VideoReader::open(&path, 4096).ok().map(|r| {
                    (upload(&self.device, &self.queue, &r.frame, r.w, r.h, true, false), Some(r))
                })
            } else {
                ed_media::load_image(&path).ok().map(|p| (upload(&self.device, &self.queue, &p.rgba, p.w, p.h, true, true), None))
            };
            let entry = entry.map(|(texture, video)| {
                let flat = upload(&self.device, &self.queue, &[128, 128, 255, 255], 1, 1, false, false);
                let white = upload(&self.device, &self.queue, &[255; 4], 1, 1, false, false);
                let bind = Arc::new(tex_set_bind(&self.device, &self.tex_layout, &self.tex_sampler, [&texture, &flat, &white]));
                MeshMedia { texture, bind, video }
            });
            cache.insert(src.to_string(), entry);
        }
        let Some(entry) = cache.get_mut(src).and_then(|e| e.as_mut()) else { return self.default_tex.clone() };
        if let Some(reader) = entry.video.as_mut() {
            if reader.seek(time).unwrap_or(false) {
                self.queue.write_texture(
                    entry.texture.as_image_copy(),
                    &reader.frame,
                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(reader.w * 4), rows_per_image: None },
                    wgpu::Extent3d { width: reader.w, height: reader.h, depth_or_array_layers: 1 },
                );
            }
        }
        entry.bind.clone()
    }

    /// Texture for an image or video layer at its current time; (bind group, pixel size).
    fn media_frame(&self, m: &EvalMedia) -> Option<(Arc<wgpu::BindGroup>, [u32; 2])> {
        let mut cache = self.media.lock().unwrap();
        if !cache.contains_key(&m.src) {
            let path = self.root.lock().unwrap().join(&m.src);
            let entry = if m.time.is_some() {
                ed_media::VideoReader::open(&path, 1920).ok().map(|r| {
                    let t = upload(&self.device, &self.queue, &r.frame, r.w, r.h, true, false);
                    (t, [r.w, r.h], Some(r))
                })
            } else {
                ed_media::load_image(&path).ok().map(|mut p| {
                    // Canvas images are drawn premultiplied.
                    for px in p.rgba.chunks_exact_mut(4) {
                        let a = px[3] as u16;
                        for c in &mut px[..3] {
                            *c = ((*c as u16 * a + 127) / 255) as u8;
                        }
                    }
                    (upload(&self.device, &self.queue, &p.rgba, p.w, p.h, true, false), [p.w, p.h], None)
                })
            };
            let entry = entry.map(|(texture, size, video)| {
                let view = texture.create_view(&Default::default());
                let bind = Arc::new(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &self.image_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                    ],
                }));
                MediaTex { texture, bind, size, video }
            });
            cache.insert(m.src.clone(), entry);
        }
        let entry = cache.get_mut(&m.src)?.as_mut()?;
        if let (Some(reader), Some(t)) = (entry.video.as_mut(), m.time) {
            if reader.seek(t).unwrap_or(false) {
                self.queue.write_texture(
                    entry.texture.as_image_copy(),
                    &reader.frame,
                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(reader.w * 4), rows_per_image: None },
                    wgpu::Extent3d { width: reader.w, height: reader.h, depth_or_array_layers: 1 },
                );
            }
        }
        Some((entry.bind.clone(), entry.size))
    }

    /// Points of a cloud file (f32 `x y z keep` each), uploaded once per `src`.
    fn point_set(&self, src: &str) -> Option<(Arc<wgpu::Buffer>, u32)> {
        let mut cache = self.point_sets.lock().unwrap();
        if !cache.contains_key(src) {
            let path = self.root.lock().unwrap().join(src);
            let set = std::fs::read(path).ok().filter(|b| b.len() >= 16).map(|mut bytes| {
                bytes.truncate(bytes.len() / 16 * 16);
                (Arc::new(self.vertex_buffer(&bytes).expect("non-empty")), (bytes.len() / 16) as u32)
            });
            cache.insert(src.to_string(), set);
        }
        cache.get(src).cloned().flatten()
    }

    fn vertex_buffer(&self, data: &[u8]) -> Option<wgpu::Buffer> {
        (!data.is_empty()).then(|| {
            self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: data,
                usage: wgpu::BufferUsages::VERTEX,
            })
        })
    }

    fn begin_pass<'e>(enc: &'e mut wgpu::CommandEncoder, target: &Target, clear: [f32; 4]) -> wgpu::RenderPass<'e> {
        enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.msaa,
                depth_slice: None,
                resolve_target: Some(&target.view),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: clear[0] as f64,
                        g: clear[1] as f64,
                        b: clear[2] as f64,
                        a: clear[3] as f64,
                    }),
                    store: wgpu::StoreOp::Discard,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &target.depth,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        })
    }

    /// Environment map for `scene.env` (None: none set or it failed to load).
    fn env_map(&self, src: &str) -> Option<Arc<EnvGpu>> {
        if src.is_empty() {
            return None;
        }
        let mut cache = self.envs.lock().unwrap();
        if let Some(e) = cache.get(src) {
            return e.clone();
        }
        let path = self.root.lock().unwrap().join(src);
        let env = cine::load_env(&self.device, &self.queue, &path).map(Arc::new);
        cache.insert(src.to_string(), env.clone());
        env
    }

    /// Renders the 3D world (no canvas layers). `clear` is premultiplied RGBA:
    /// transparent for views, the scene background for the 3D workspace.
    /// The world is lit in HDR (with the shadow light's cube map), then the
    /// post pass adds the camera's haze, depth of field and bloom and tonemaps
    /// into `target`.
    pub fn render_world(&self, scene: &EvalScene, cam: ViewCam, target: &Target, clear: [f32; 4]) {
        let [w, h] = target.size;
        let (view, eye, fov) = cam.resolve();
        let lens = match cam {
            ViewCam::Camera { lens, .. } => lens,
            ViewCam::Orbit { .. } => Lens::default(),
        };
        let (near, far) = (0.05, 2000.0);
        let proj = perspective_rh(fov * PI / 180.0, w as f32 / h as f32, near, far);

        let mut lights = [GpuLight::zeroed(); MAX_LIGHTS];
        for (slot, l) in lights.iter_mut().zip(&scene.lights) {
            slot.pos = [l.pos.x, l.pos.y, l.pos.z, 1.0];
            slot.color = [l.color[0] * l.energy, l.color[1] * l.energy, l.color[2] * l.energy, 0.0];
        }
        // The first lit light with a shadow casts one.
        let caster = scene.lights.iter().take(MAX_LIGHTS).position(|l| l.shadow > 0.0 && l.energy > 0.0);
        let faces = caster.map(|i| cine::shadow_faces(scene.lights[i].pos));
        let env = self.env_map(&scene.env);
        let e = env.clone().unwrap_or_else(|| self.default_env.clone());
        let a = scene.ambient;
        let globals = MeshGlobals {
            view_proj: (proj * view).to_cols_array_2d(),
            cam_pos: [eye.x, eye.y, eye.z, 1.0],
            ambient: [a, a, a, scene.lights.len().min(MAX_LIGHTS) as f32],
            lights,
            shadow: [
                caster.map_or(-1.0, |i| i as f32),
                caster.map_or(0.0, |i| scene.lights[i].shadow),
                1.0 / SHADOW_SIZE as f32,
                0.0,
            ],
            env: [if env.is_some() { 1.0 } else { 0.0 }, scene.envrot * PI / 180.0, e.max_lod, 0.0],
            sh: e.sh,
            shadow_vp: faces.unwrap_or([Mat4::IDENTITY; 6]).map(|m| m.to_cols_array_2d()),
        };
        self.queue.write_buffer(&self.mesh_globals, 0, bytemuck::bytes_of(&globals));
        let mesh_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mesh globals"),
            layout: &self.mesh_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.mesh_globals.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&self.shadow_array) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.shadow_sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&e.view) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&self.env_sampler) },
            ],
        });

        // Opaque surfaces (and fur shells) in instanced batches, then glass
        // sorted back to front. Shadow casters: opaque surfaces without shells.
        let mut instances: Vec<MeshInstance> = Vec::new();
        let mut opaque: Vec<Batch> = Vec::new();
        let mut casters: Vec<Batch> = Vec::new();
        let mut glass: Vec<(f32, Arc<GpuMesh>, u32, Arc<wgpu::BindGroup>)> = Vec::new();
        for m in &scene.meshes {
            let Some(model) = self.mesh_for(m) else { continue };
            let media_tex = m.tex.contains('.');
            let tex = if media_tex { self.mesh_media(&m.tex, m.tex_time) } else { self.texture_set(&m.tex) };
            // A glTF's own maps apply when the object sets no `tex`.
            let own_maps = m.tex.is_empty() && model.iter().any(|(_, t)| t.is_some());
            let pick = |own: &Option<Arc<wgpu::BindGroup>>| match own {
                Some(t) if own_maps => t.clone(),
                _ => tex.clone(),
            };
            let c = Mat3::from_mat4(m.world).inverse().transpose().to_cols_array_2d();
            let inst = |shell: f32| MeshInstance {
                model: m.world.to_cols_array_2d(),
                normal: [
                    [c[0][0], c[0][1], c[0][2], shell],
                    [c[1][0], c[1][1], c[1][2], 0.0],
                    [c[2][0], c[2][1], c[2][2], 0.0],
                ],
                color: m.color,
                material: m.material,
                tex: [
                    if m.tex.is_empty() && !own_maps { 0.0 } else { 1.0 },
                    m.uvscale,
                    m.emit,
                    if m.soft || m.material[2] > 0.0 { 1.0 } else { 0.0 },
                ],
                sway: m.sway,
            };
            let index = instances.len() as u32;
            instances.push(inst(0.0));
            if m.material[2] > 0.0 || m.soft {
                let depth = (view * m.world.w_axis).z;
                glass.extend(model.iter().map(|(mesh, own)| (depth, mesh.clone(), index, pick(own))));
            } else {
                for (mesh, own) in model.iter() {
                    let tex = pick(own);
                    push_batch(&mut opaque, mesh, &tex, index);
                    // Unlit paint doesn't cast shadows; cut-out cards cast their alpha silhouette.
                    if m.emit < 1.0 {
                        push_batch(&mut casters, mesh, if media_tex || (own_maps && own.is_some()) { &tex } else { &self.default_tex }, index);
                    }
                }
                if m.material[3] > 0.0 {
                    for s in 1..=FUR_SHELLS {
                        let shell = instances.len() as u32;
                        for (mesh, own) in model.iter() {
                            push_batch(&mut opaque, mesh, &pick(own), shell);
                        }
                        instances.push(inst(s as f32 / FUR_SHELLS as f32));
                    }
                }
            }
        }
        glass.sort_by(|a, b| a.0.total_cmp(&b.0));
        let instance_buf = self.vertex_buffer(bytemuck::cast_slice(&instances));
        // Dots are sized in scene pixels, so previews at other sizes look the same.
        let px = w as f32 / scene.res[0].max(1) as f32;
        let clouds: Vec<(Arc<wgpu::Buffer>, u32, wgpu::BindGroup)> = scene
            .points
            .iter()
            .filter_map(|p: &EvalPoints| {
                let (buf, count) = self.point_set(&p.src)?;
                let u = PointsUniform {
                    model: p.world.to_cols_array_2d(),
                    color: p.color,
                    params: [p.size * px, scene.frame, w as f32, h as f32],
                    shade: {
                        let c = p.world.transform_point3(Vec3::ZERO);
                        [p.lit, c.x, c.y, c.z]
                    },
                };
                let ub = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: None,
                    contents: bytemuck::bytes_of(&u),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
                let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &self.points_layout,
                    entries: &[wgpu::BindGroupEntry { binding: 0, resource: ub.as_entire_binding() }],
                });
                Some((buf, count, bind))
            })
            .collect();

        let mut world_targets = self.world_targets.lock().unwrap();
        let wt = match world_targets.iter().position(|t| t.size == target.size) {
            Some(i) => &world_targets[i],
            None => {
                if world_targets.len() >= 4 {
                    world_targets.remove(0);
                }
                world_targets.push(WorldTarget::new(&self.device, target.size));
                world_targets.last().unwrap()
            }
        };

        let mut enc = self.device.create_command_encoder(&Default::default());
        if let (Some(faces), Some(ib)) = (faces, &instance_buf) {
            for (f, vp) in faces.iter().enumerate() {
                let (buf, bind) = &self.shadow_faces[f];
                self.queue.write_buffer(buf, 0, bytemuck::bytes_of(&vp.to_cols_array_2d()));
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("shadow"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &self.shadow_layers[f],
                        depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.shadow_pipeline);
                pass.set_bind_group(0, bind, &[]);
                pass.set_vertex_buffer(1, ib.slice(..));
                for b in &casters {
                    pass.set_bind_group(1, &*b.tex, &[]);
                    pass.set_vertex_buffer(0, b.mesh.vertices.slice(..));
                    pass.set_index_buffer(b.mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..b.mesh.count, 0, b.range.clone());
                }
            }
        }
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("world"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &wt.msaa,
                    depth_slice: None,
                    resolve_target: Some(&wt.levels[0]),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: clear[0] as f64,
                            g: clear[1] as f64,
                            b: clear[2] as f64,
                            a: clear[3] as f64,
                        }),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &wt.depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some(ib) = &instance_buf {
                pass.set_bind_group(0, &mesh_bind, &[]);
                pass.set_vertex_buffer(1, ib.slice(..));
                pass.set_pipeline(&self.mesh_pipeline);
                for b in &opaque {
                    pass.set_bind_group(1, &*b.tex, &[]);
                    pass.set_vertex_buffer(0, b.mesh.vertices.slice(..));
                    pass.set_index_buffer(b.mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..b.mesh.count, 0, b.range.clone());
                }
            }
            if !clouds.is_empty() {
                pass.set_bind_group(0, &mesh_bind, &[]);
                pass.set_pipeline(&self.points_pipeline);
                for (buf, count, bind) in &clouds {
                    pass.set_bind_group(1, bind, &[]);
                    pass.set_vertex_buffer(0, buf.slice(..));
                    pass.draw(0..6, 0..*count);
                }
            }
            if let Some(ib) = &instance_buf {
                pass.set_bind_group(0, &mesh_bind, &[]);
                pass.set_vertex_buffer(1, ib.slice(..));
                pass.set_pipeline(&self.glass_pipeline);
                for (_, mesh, i, tex) in &glass {
                    pass.set_bind_group(1, &**tex, &[]);
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.count, 0, *i..*i + 1);
                }
            }
        }
        // Bloom reads a mip chain of the lit image.
        let bloom = scene.bloom > 0.0 && wt.levels.len() > 1;
        if bloom {
            for m in 1..wt.levels.len() {
                let src = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &self.image_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&wt.levels[m - 1]) },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                    ],
                });
                let mut pass = Self::fullscreen_pass(&mut enc, &wt.levels[m], "bloom");
                pass.set_pipeline(&self.down_pipeline);
                pass.set_bind_group(0, &src, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        let post = PostUniform {
            res: [w as f32, h as f32],
            near,
            far,
            coc_k: lens.aperture / (2.0 * (fov * PI / 360.0).tan()) * h as f32,
            inv_focus: 1.0 / lens.focus.max(0.01),
            max_coc: h as f32 * 0.025,
            haze: lens.haze,
            haze_color: [lens.haze_color[0], lens.haze_color[1], lens.haze_color[2], 1.0],
            bloom: if bloom { scene.bloom } else { 0.0 },
            lods: (wt.levels.len() - 1) as f32,
            grain: scene.grain,
            vignette: scene.vignette,
            frame: [scene.frame, 0.0, 0.0, 0.0],
        };
        self.queue.write_buffer(&self.post_uniform, 0, bytemuck::bytes_of(&post));
        let inputs = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("post inputs"),
            layout: &self.post_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&wt.full) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&wt.depth) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        {
            let mut pass = Self::fullscreen_pass(&mut enc, &target.view, "post");
            pass.set_pipeline(&self.post_pipeline);
            pass.set_bind_group(0, &self.post_bind, &[]);
            pass.set_bind_group(1, &inputs, &[]);
            pass.draw(0..3, 0..1);
        }
        // Submit now: the mesh and post uniforms are rewritten for the next view.
        self.queue.submit([enc.finish()]);
    }

    fn fullscreen_pass<'e>(enc: &'e mut wgpu::CommandEncoder, view: &wgpu::TextureView, label: &str) -> wgpu::RenderPass<'e> {
        enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        })
    }

    fn text_vertices(&self, t: &EvalText, out: &mut Vec<PathVertex>) {
        let spec = ed_text::Spec {
            text: &t.body,
            font: &t.font,
            weight: t.weight,
            size: t.size,
            align: match t.align {
                Align::Left => ed_text::Align::Left,
                Align::Center => ed_text::Align::Center,
                Align::Right => ed_text::Align::Right,
            },
            tracking: t.tracking,
            leading: t.leading,
        };
        let glyphs = self.fonts.lock().unwrap().layout(&spec);
        let center = Vec2::from(t.center);
        let scale = Vec2::from(t.scale);
        for g in glyphs {
            let Some(l) = t.letters.get(g.letter) else { continue };
            let alpha = t.color[3] * l.alpha.clamp(0.0, 1.0);
            if alpha <= 0.0 || l.scale == [0.0, 0.0] {
                continue;
            }
            let color = [t.color[0], t.color[1], t.color[2], alpha];
            let origin = Vec2::from(g.origin);
            let pivot = Vec2::from(g.pivot);
            let place = |v: [f32; 2]| {
                let local = origin + Vec2::new(v[0], -v[1]) * g.scale;
                let q = pivot + rot2((local - pivot) * Vec2::from(l.scale), l.rot_deg) + Vec2::from(l.offset);
                (center + rot2(q * scale, t.rot_deg)).to_array()
            };
            for &i in &g.mesh.indices {
                out.push(PathVertex { pos: place(g.mesh.vertices[i as usize]), color });
            }
        }
    }

    /// An offscreen target of `size` from the free pool (or a new one); give
    /// it back to `comp_targets` when done.
    fn take_target(&self, size: [u32; 2]) -> Target {
        let mut pool = self.comp_targets.lock().unwrap();
        match pool.iter().position(|t| t.size == size) {
            Some(i) => pool.swap_remove(i),
            None => Target::new(&self.device, size),
        }
    }

    /// The compiled pipeline of shader plugin `name`, rebuilt if its file changed.
    fn plugin_pipeline(&self, name: &str, usage: plugins::Use) -> Option<wgpu::RenderPipeline> {
        let root = self.root.lock().unwrap().clone();
        let layout = match usage {
            plugins::Use::Fx => &self.fx_layout,
            plugins::Use::Layer => &self.layer_layout,
        };
        self.plugins
            .lock()
            .unwrap()
            .get(&root, name, usage, |src| {
                let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(name),
                    source: wgpu::ShaderSource::Wgsl(src.into()),
                });
                self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(name),
                    layout: Some(layout),
                    vertex: wgpu::VertexState { module: &module, entry_point: Some("editor_vs"), compilation_options: Default::default(), buffers: &[] },
                    primitive: Default::default(),
                    depth_stencil: None,
                    multisample: Default::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &module,
                        entry_point: Some("editor_fs"),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState { format: FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            })
            .cloned()
    }

    /// Why shader plugin `name` did not draw last time it was used, if it did not.
    pub fn plugin_error(&self, name: &str) -> Option<String> {
        self.plugins.lock().unwrap().error(name)
    }
    /// Composites the canvas: background, then layers back to front. Effect
    /// layers split the stack into segments; each effect reads everything
    /// composited so far and the next segment draws on top of its result.
    pub fn render_canvas(&self, scene: &EvalScene, target: &Target) {
        let res = Vec2::new(scene.res[0] as f32, scene.res[1] as f32);
        let px_scale = target.size[0] as f32 / res.x;

        // 0. Composition clips first: each is a whole canvas render of its own,
        //    finished before this one takes the shared view and effect targets.
        let comps: Vec<Target> = scene
            .layers
            .iter()
            .filter_map(|l| if let Layer2D::Comp(c) = l { Some(c) } else { None })
            .map(|c| {
                let cres = Vec2::new(c.scene.res[0] as f32, c.scene.res[1] as f32);
                // Enough pixels for the box it fills on screen, never more than 8K.
                let k = (c.size[0].abs() / cres.x).max(c.size[1].abs() / cres.y) * px_scale;
                let size = [(cres.x * k).round().clamp(1.0, 8192.0) as u32, (cres.y * k).round().clamp(1.0, 8192.0) as u32];
                let t = self.take_target(size);
                self.render_canvas(&c.scene, &t);
                t
            })
            .collect();

        // Shader-layer plugins draw into their own targets too (None: the
        // plugin is missing or broken; the layer is skipped).
        let shaders: Vec<Option<Target>> = scene
            .layers
            .iter()
            .filter_map(|l| if let Layer2D::Shader(s) = l { Some(s) } else { None })
            .map(|s| {
                let pipeline = self.plugin_pipeline(&s.plugin, plugins::Use::Layer)?;
                let size = [
                    (s.size[0].abs() * px_scale).round().clamp(1.0, 8192.0) as u32,
                    (s.size[1].abs() * px_scale).round().clamp(1.0, 8192.0) as u32,
                ];
                let t = self.take_target(size);
                let u = LayerUniform { size: [size[0] as f32, size[1] as f32], seconds: s.time, frame: s.frame, color: s.color, params: s.params };
                self.queue.write_buffer(&self.layer_uniform, 0, bytemuck::bytes_of(&u));
                let mut enc = self.device.create_command_encoder(&Default::default());
                {
                    let mut pass = fullscreen_pass(&mut enc, &t.view, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
                    pass.set_pipeline(&pipeline);
                    pass.set_bind_group(0, &self.layer_bind, &[]);
                    pass.draw(0..3, 0..1);
                }
                self.queue.submit([enc.finish()]);
                Some(t)
            })
            .collect();

        // 1. Each view renders the world through its camera into its own texture.
        let mut targets = self.view_targets.lock().unwrap();
        let views: Vec<_> = scene
            .layers
            .iter()
            .filter_map(|l| if let Layer2D::View(v) = l { Some(v) } else { None })
            .collect();
        for (i, v) in views.iter().enumerate() {
            let size = [
                (v.size[0].abs() * px_scale).round().clamp(1.0, 8192.0) as u32,
                (v.size[1].abs() * px_scale).round().clamp(1.0, 8192.0) as u32,
            ];
            if targets.get(i).is_none_or(|t| t.size != size) {
                let t = Target::new(&self.device, size);
                if i < targets.len() {
                    targets[i] = t;
                } else {
                    targets.push(t);
                }
            }
            self.render_world(scene, ViewCam::of(v.camera.as_ref()), &targets[i], [0.0; 4]);
        }

        // 2. Build draw runs in layer order, split into segments at effects.
        let mut shapes: Vec<ShapeInstance> = Vec::new();
        let mut paths: Vec<PathVertex> = Vec::new();
        let mut images: Vec<ImageVertex> = Vec::new();
        let mut segments = vec![Segment { runs: Vec::new(), fx: None }];
        let mut view_index = 0;
        let mut comp_index = 0;
        let mut shader_index = 0;
        // Image bindings: views first (index = view order), then media frames.
        let mut media_binds: Vec<Arc<wgpu::BindGroup>> = Vec::new();
        // Full-frame quad, used to carry an effect's result into the next segment.
        let full_quad = |images: &mut Vec<ImageVertex>| {
            let start = images.len() as u32;
            let v = |x: f32, y: f32| ImageVertex { pos: [x * res.x, y * res.y], uv: [x, y], alpha: 1.0 };
            images.extend([v(0.0, 0.0), v(1.0, 0.0), v(1.0, 1.0), v(0.0, 0.0), v(1.0, 1.0), v(0.0, 1.0)]);
            start..start + 6
        };
        for layer in &scene.layers {
            let runs = &mut segments.last_mut().unwrap().runs;
            match layer {
                Layer2D::Shape(s) => {
                    let start = shapes.len() as u32;
                    shapes.push(ShapeInstance {
                        center: s.center,
                        size: s.size,
                        color: s.color,
                        params: [
                            s.rot_deg * PI / 180.0,
                            s.radius,
                            if s.kind == Kind::Ellipse { 1.0 } else { 0.0 },
                            0.0,
                        ],
                    });
                    match runs.last_mut() {
                        Some(Run::Shapes(r)) => r.end += 1,
                        _ => runs.push(Run::Shapes(start..start + 1)),
                    }
                }
                Layer2D::Text(t) => {
                    let start = paths.len() as u32;
                    self.text_vertices(t, &mut paths);
                    let end = paths.len() as u32;
                    match runs.last_mut() {
                        Some(Run::Paths(r)) => r.end = end,
                        _ => runs.push(Run::Paths(start..end)),
                    }
                }
                Layer2D::View(v) => {
                    let start = images.len() as u32;
                    let half = Vec2::from(v.size) / 2.0;
                    let corner = |x: f32, y: f32| {
                        let p = Vec2::from(v.center) + rot2(Vec2::new(x, y) * half, v.rot_deg);
                        ImageVertex { pos: p.to_array(), uv: [(x + 1.0) / 2.0, (y + 1.0) / 2.0], alpha: v.opacity }
                    };
                    images.extend([
                        corner(-1.0, -1.0),
                        corner(1.0, -1.0),
                        corner(1.0, 1.0),
                        corner(-1.0, -1.0),
                        corner(1.0, 1.0),
                        corner(-1.0, 1.0),
                    ]);
                    runs.push(Run::Image(view_index, start..start + 6));
                    view_index += 1;
                }
                Layer2D::Media(m) => {
                    let Some((bind, [mw, mh])) = self.media_frame(m) else { continue };
                    let start = images.len() as u32;
                    images.extend(fit_quad(m.center, m.size, m.rot_deg, m.opacity, mw as f32 / mh as f32, m.fit));
                    media_binds.push(bind);
                    runs.push(Run::Image(1_000_000 + media_binds.len() - 1, start..start + 6));
                }
                Layer2D::Comp(c) => {
                    let start = images.len() as u32;
                    let aspect = c.scene.res[0] as f32 / c.scene.res[1].max(1) as f32;
                    images.extend(fit_quad(c.center, c.size, c.rot_deg, c.opacity, aspect, c.fit));
                    runs.push(Run::Image(2_000_000 + comp_index, start..start + 6));
                    comp_index += 1;
                }
                Layer2D::Shader(_) => {
                    // Same order as the prepass; a broken plugin's layer is skipped.
                    let i = shader_index;
                    shader_index += 1;
                    let (Layer2D::Shader(s), Some(_)) = (layer, &shaders[i]) else { continue };
                    let start = images.len() as u32;
                    images.extend(fit_quad(s.center, s.size, s.rot_deg, s.opacity, s.size[0] / s.size[1].max(1e-3), Fit::Stretch));
                    runs.push(Run::Image(3_000_000 + i, start..start + 6));
                }
                Layer2D::Fx(f) => {
                    // A broken or missing plugin effect passes the canvas through.
                    let pipeline = if f.plugin.is_empty() {
                        None
                    } else {
                        match self.plugin_pipeline(&f.plugin, plugins::Use::Fx) {
                            Some(p) => Some(p),
                            None => continue,
                        }
                    };
                    segments.last_mut().unwrap().fx = Some((f.clone(), pipeline));
                    segments.push(Segment { runs: vec![Run::Image(usize::MAX, full_quad(&mut images))], fx: None });
                }
            }
        }

        let globals = CanvasGlobals { res: res.to_array(), px: 1.0 / px_scale, _pad: 0.0 };
        self.queue.write_buffer(&self.canvas_globals, 0, bytemuck::bytes_of(&globals));
        let shape_buf = self.vertex_buffer(bytemuck::cast_slice(&shapes));
        let path_buf = self.vertex_buffer(bytemuck::cast_slice(&paths));
        let image_buf = self.vertex_buffer(bytemuck::cast_slice(&images));
        let bind_image = |view: &wgpu::TextureView| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.image_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            })
        };
        let image_binds: Vec<wgpu::BindGroup> = targets[..views.len()].iter().map(|t| bind_image(&t.view)).collect();
        let comp_binds: Vec<wgpu::BindGroup> = comps.iter().map(|t| bind_image(&t.view)).collect();
        let shader_binds: Vec<Option<wgpu::BindGroup>> = shaders.iter().map(|t| t.as_ref().map(|t| bind_image(&t.view))).collect();

        // Effect ping-pong textures at target size.
        let mut fx_targets = self.fx_targets.lock().unwrap();
        let fx_count = segments.iter().filter(|s| s.fx.is_some()).count();
        if fx_count > 0 && fx_targets.first().is_none_or(|(t, _)| [t.width(), t.height()] != target.size) {
            *fx_targets = (0..2)
                .map(|_| {
                    let t = self.device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("fx"),
                        size: wgpu::Extent3d { width: target.size[0], height: target.size[1], depth_or_array_layers: 1 },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: FORMAT,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    });
                    let v = t.create_view(&Default::default());
                    (t, v)
                })
                .collect();
        }
        let fx_reads: Vec<wgpu::BindGroup> = fx_targets.iter().map(|(_, v)| bind_image(v)).collect();
        let target_read = bind_image(&target.view);

        let bg = scene.bg;
        let mut pong = 0;
        for (si, seg) in segments.iter().enumerate() {
            let mut enc = self.device.create_command_encoder(&Default::default());
            {
                let clear = if si == 0 { [bg[0] * bg[3], bg[1] * bg[3], bg[2] * bg[3], bg[3]] } else { [0.0; 4] };
                let mut pass = Self::begin_pass(&mut enc, target, clear);
                pass.set_bind_group(0, &self.canvas_bind, &[]);
                for run in &seg.runs {
                    match run {
                        Run::Shapes(r) => {
                            pass.set_pipeline(&self.shape_pipeline);
                            pass.set_vertex_buffer(0, shape_buf.as_ref().unwrap().slice(..));
                            pass.draw(0..6, r.clone());
                        }
                        Run::Paths(r) if !r.is_empty() => {
                            pass.set_pipeline(&self.path_pipeline);
                            pass.set_vertex_buffer(0, path_buf.as_ref().unwrap().slice(..));
                            pass.draw(r.clone(), 0..1);
                        }
                        Run::Paths(_) => {}
                        Run::Image(i, r) => {
                            pass.set_pipeline(&self.image_pipeline);
                            // usize::MAX: the previous effect's output.
                            let bind = match *i {
                                usize::MAX => &fx_reads[1 - pong],
                                i if i >= 3_000_000 => shader_binds[i - 3_000_000].as_ref().unwrap(),
                                i if i >= 2_000_000 => &comp_binds[i - 2_000_000],
                                i if i >= 1_000_000 => &*media_binds[i - 1_000_000],
                                i => &image_binds[i],
                            };
                            pass.set_bind_group(1, bind, &[]);
                            pass.set_vertex_buffer(0, image_buf.as_ref().unwrap().slice(..));
                            pass.draw(r.clone(), 0..1);
                        }
                    }
                }
            }
            if let Some((f, plugin)) = &seg.fx {
                // Effect: target -> fx texture, read by the next segment.
                let u = FxUniform {
                    res: [target.size[0] as f32, target.size[1] as f32],
                    cell: (f.cell * px_scale).max(1.0),
                    amount: f.amount,
                    kind: FxKind::ALL.iter().position(|k| *k == f.kind).unwrap() as f32,
                    frame: scene.frame,
                    seconds: f.seconds,
                    _pad: 0.0,
                    color: f.color,
                    params: f.params,
                };
                self.queue.write_buffer(&self.fx_uniform, 0, bytemuck::bytes_of(&u));
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("fx"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &fx_targets[pong].1,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(plugin.as_ref().unwrap_or(&self.fx_pipeline));
                pass.set_bind_group(0, &self.fx_bind, &[]);
                pass.set_bind_group(1, &target_read, &[]);
                pass.draw(0..3, 0..1);
                drop(pass);
                pong = 1 - pong;
            }
            // One submit per segment: effect uniforms change between them.
            self.queue.submit([enc.finish()]);
        }
        let mut pool = self.comp_targets.lock().unwrap();
        pool.extend(comps);
        pool.extend(shaders.into_iter().flatten());
        pool.truncate(16);
    }

    /// Renders the canvas and reads back tightly packed sRGB RGBA8 pixels.
    pub fn render_rgba(&self, scene: &EvalScene, size: [u32; 2]) -> Result<Vec<u8>> {
        let target = Target::new(&self.device, size);
        self.render_canvas(scene, &target);
        self.read(&target)
    }

    pub fn read(&self, target: &Target) -> Result<Vec<u8>> {
        let mut rb = Readback::new(&self.device, target.size);
        rb.start(self, target);
        let mut out = Vec::new();
        rb.finish(self, &mut out)?;
        Ok(out)
    }

    fn bind_view(&self, view: &wgpu::TextureView) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.image_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        })
    }

    /// A stream of export frames of `size` px; see [`FrameStream`].
    pub fn frame_stream(&self, size: [u32; 2]) -> FrameStream {
        let slot = || {
            let target = Target::new(&self.device, size);
            let bind = self.bind_view(&target.view);
            StreamSlot { readback: Readback::new(&self.device, target.size), target, bind }
        };
        let accum = self
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("accum"),
                size: wgpu::Extent3d { width: size[0].max(1), height: size[1].max(1), depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: HDR,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());
        let accum_bind = self.bind_view(&accum);
        FrameStream { slots: [slot(), slot()], accum, accum_bind, next: 0, in_flight: None }
    }
}

/// A mappable copy of a target's pixels.
struct Readback {
    buffer: wgpu::Buffer,
    size: [u32; 2],
    padded: u32,
    submitted: Option<wgpu::SubmissionIndex>,
}

impl Readback {
    fn new(device: &wgpu::Device, size: [u32; 2]) -> Self {
        let padded = (size[0] * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (padded * size[1]) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Self { buffer, size, padded, submitted: None }
    }

    /// Queues the copy of `target` and the buffer's mapping; returns at once.
    fn start(&mut self, r: &Renderer, target: &Target) {
        let [w, h] = self.size;
        let mut enc = r.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            target.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &self.buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(self.padded), rows_per_image: None },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.submitted = Some(r.queue.submit([enc.finish()]));
        self.buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    }

    /// Waits for that copy only (later GPU work keeps running), then writes
    /// the tightly packed rows into `out` and unmaps the buffer for reuse.
    fn finish(&mut self, r: &Renderer, out: &mut Vec<u8>) -> Result<()> {
        r.device.poll(wgpu::PollType::Wait { submission_index: self.submitted.take(), timeout: None })?;
        let [w, h] = self.size;
        let row = (w * 4) as usize;
        out.clear();
        out.reserve(row * h as usize);
        {
            let data = self.buffer.slice(..).get_mapped_range()?;
            for y in 0..h as usize {
                let start = y * self.padded as usize;
                out.extend_from_slice(&data[start..start + row]);
            }
        }
        self.buffer.unmap();
        Ok(())
    }
}

struct StreamSlot {
    target: Target,
    bind: wgpu::BindGroup,
    readback: Readback,
}

/// Export frames of one size. Motion blur averages the sub-frames on the GPU
/// in linear light (one readback per frame, not per sub-frame), and readback
/// is double-buffered: frame n+1 renders while frame n is copied out. The
/// renderer is passed per call so callers can release it between frames.
pub struct FrameStream {
    slots: [StreamSlot; 2],
    accum: wgpu::TextureView,
    accum_bind: wgpu::BindGroup,
    next: usize,
    in_flight: Option<usize>,
}

impl FrameStream {
    /// Queues one frame: the average of `samples` (one scene: no blur). When
    /// the previous frame is ready, writes its sRGB RGBA8 pixels into `out`
    /// and returns true.
    pub fn push(&mut self, r: &Renderer, samples: &[EvalScene], out: &mut Vec<u8>) -> Result<bool> {
        let i = self.next;
        let slot = &self.slots[i];
        match samples {
            [] => anyhow::bail!("a frame needs at least one sample"),
            [scene] => r.render_canvas(scene, &slot.target),
            many => {
                let w = 1.0 / many.len() as f64;
                for (k, scene) in many.iter().enumerate() {
                    r.render_canvas(scene, &slot.target);
                    let load = if k == 0 { wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT) } else { wgpu::LoadOp::Load };
                    let mut enc = r.device.create_command_encoder(&Default::default());
                    {
                        let mut pass = fullscreen_pass(&mut enc, &self.accum, load);
                        pass.set_pipeline(&r.accum_pipeline);
                        pass.set_blend_constant(wgpu::Color { r: w, g: w, b: w, a: w });
                        pass.set_bind_group(0, &slot.bind, &[]);
                        pass.draw(0..3, 0..1);
                    }
                    r.queue.submit([enc.finish()]);
                }
                let mut enc = r.device.create_command_encoder(&Default::default());
                {
                    let mut pass = fullscreen_pass(&mut enc, &slot.target.view, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
                    pass.set_pipeline(&r.copy_pipeline);
                    pass.set_bind_group(0, &self.accum_bind, &[]);
                    pass.draw(0..3, 0..1);
                }
                r.queue.submit([enc.finish()]);
            }
        }
        self.slots[i].readback.start(r, &self.slots[i].target);
        self.next = 1 - i;
        match self.in_flight.replace(i) {
            Some(j) => self.slots[j].readback.finish(r, out).map(|_| true),
            None => Ok(false),
        }
    }

    /// The last queued frame, if any.
    pub fn finish(&mut self, r: &Renderer, out: &mut Vec<u8>) -> Result<bool> {
        match self.in_flight.take() {
            Some(j) => self.slots[j].readback.finish(r, out).map(|_| true),
            None => Ok(false),
        }
    }
}

fn fullscreen_pass<'e>(enc: &'e mut wgpu::CommandEncoder, view: &wgpu::TextureView, load: wgpu::LoadOp<wgpu::Color>) -> wgpu::RenderPass<'e> {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

/// Encodes RGBA8 pixels as PNG.
pub fn encode_png(rgba: &[u8], size: [u32; 2]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, size[0], size[1]);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(rgba)?;
    Ok(out)
}

/// Lays frames out left-to-right, top-to-bottom in one image (one image costs
/// far fewer agent tokens than several). Frames are separated by 4px gaps.
pub fn contact_sheet(frames: &[Vec<u8>], size: [u32; 2], cols: u32) -> (Vec<u8>, [u32; 2]) {
    let cols = cols.clamp(1, frames.len().max(1) as u32);
    let rows = (frames.len() as u32).div_ceil(cols);
    let gap = 4;
    let sheet = [cols * size[0] + (cols - 1) * gap, rows * size[1] + (rows - 1) * gap];
    let mut out = vec![0u8; (sheet[0] * sheet[1] * 4) as usize];
    for (i, f) in frames.iter().enumerate() {
        let (cx, cy) = (i as u32 % cols, i as u32 / cols);
        let (ox, oy) = (cx * (size[0] + gap), cy * (size[1] + gap));
        for y in 0..size[1] {
            let src = (y * size[0] * 4) as usize;
            let dst = (((oy + y) * sheet[0] + ox) * 4) as usize;
            out[dst..dst + (size[0] * 4) as usize].copy_from_slice(&f[src..src + (size[0] * 4) as usize]);
        }
    }
    (out, sheet)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed_core::{Session, Source};

    /// The cinematic path (shadow cube, HDR env, depth of field, haze, bloom)
    /// builds its pipelines and draws. Set ED_RENDER_OUT=<file.png> to look.
    #[test]
    fn renders_lens_shadow_env_and_bloom() {
        let Some(r) = Renderer::headless() else { return eprintln!("no GPU adapter; skipped") };
        let dir = std::env::temp_dir().join("ed_render_test");
        std::fs::create_dir_all(&dir).unwrap();
        // A small sky: bright warm sun patch on a dim blue dome, as Radiance HDR.
        let (w, h) = (64u32, 32u32);
        let sky = image::Rgb32FImage::from_fn(w, h, |x, y| {
            let sun = ((x as i32 - 20).pow(2) + (y as i32 - 8).pow(2)) < 6;
            if sun { image::Rgb([40.0, 34.0, 28.0]) } else if y < h / 2 { image::Rgb([0.2, 0.25, 0.35]) } else { image::Rgb([0.05, 0.04, 0.03]) }
        });
        image::DynamicImage::ImageRgb32F(sky).save(dir.join("sky.hdr")).unwrap();
        r.set_root(dir.clone());

        let mut s = Session::new();
        let script = std::env::var("ED_RENDER_SCRIPT").ok();
        s.run(
            script.as_deref().unwrap_or("set scene env=sky.hdr bloom=0.1 ambient=0.6\n\
             add plane floor scale=20,1,20 color=#808080 rough=0.6\n\
             add cube c loc=0,0.5,0 rough=0.3 rep=5,1,1 spacing=2.5,0,0\n\
             add sphere hot loc=0,0.4,2 scale=0.8,0.8,0.8 metal=1 rough=0.05\n\
             add light key loc=2,5,3 energy=60 shadow=0.3\n\
             add camera cam loc=0,2.5,8 rot=-14,0,0 fov=35 focus=6 aperture=0.4 haze=0.02 color=#202838\n\
             add view v camera=cam"),
            Source::Agent,
        )
        .unwrap();
        let scene = ed_core::eval::evaluate(s.doc(), 0.0);
        let px = r.render_rgba(&scene, [480, 270]).unwrap();
        if let Ok(out) = std::env::var("ED_RENDER_OUT") {
            std::fs::write(out, encode_png(&px, [480, 270]).unwrap()).unwrap();
        }
        // Lit, not blank: some bright and some dark pixels.
        let lum: Vec<u32> = px.chunks(4).map(|p| p[0] as u32 + p[1] as u32 + p[2] as u32).collect();
        assert!(lum.iter().any(|&l| l > 300) && lum.iter().any(|&l| l < 200));
    }

    /// A clip shows its composition in the clip's box: a red comp clipped
    /// into the right half of a blue film, and a clip of a clip inside it.
    #[test]
    fn clips_show_their_compositions() {
        let Some(r) = Renderer::headless() else { return eprintln!("no GPU adapter; skipped") };
        let mut s = Session::new();
        s.run(
            "set scene res=200,100 bg=#0000ff\n\
             add comp dot scale=50,50 color=#00ff00\n\
             add comp red scale=100,100 color=#ff0000\n\
             add clip d src=dot loc=75,75 scale=50,50 comp=red\n\
             add clip c src=red loc=150,50 scale=100,100",
            Source::Agent,
        )
        .unwrap();
        let px = r.render_rgba(&ed_core::eval::evaluate(s.doc(), 0.0), [200, 100]).unwrap();
        let at = |x: usize, y: usize| <[u8; 3]>::try_from(&px[(y * 200 + x) * 4..][..3]).unwrap();
        assert_eq!(at(20, 50), [0, 0, 255], "film background");
        assert_eq!(at(120, 20), [255, 0, 0], "the comp's background, in the clip's box");
        assert_eq!(at(180, 80), [0, 255, 0], "a clip inside the comp shows its own comp");
    }

    /// Export frames come out in order, and a blurred frame is the average of
    /// its sub-frames in linear light (as if the shutter stayed open).
    #[test]
    fn frame_stream_averages_subframes_in_linear_light() {
        let Some(r) = Renderer::headless() else { return eprintln!("no GPU adapter; skipped") };
        let mut s = Session::new();
        s.run("set scene res=64,32 bg=#000000\nadd rect r loc=16,16 scale=32,32 color=#ffffff\nkey r.loc.x 16 @0 linear\nkey r.loc.x 48 @4", Source::Agent)
            .unwrap();
        let at = |f: f32| ed_core::eval::evaluate(s.doc(), f);
        let mut stream = r.frame_stream([64, 32]);
        let (mut out, mut frames) = (Vec::new(), Vec::new());
        // Frame 0 blurred over 4 sub-frames, then frame 4 sharp.
        for samples in [vec![at(0.0), at(1.0), at(2.0), at(3.0)], vec![at(4.0)]] {
            if stream.push(&r, &samples, &mut out).unwrap() {
                frames.push(out.clone());
            }
        }
        assert!(stream.finish(&r, &mut out).unwrap());
        frames.push(out.clone());
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1], r.render_rgba(&at(4.0), [64, 32]).unwrap(), "the sharp frame comes second, untouched");

        let lin = |b: u8| {
            let c = b as f32 / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        let enc = |c: f32| ((if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }) * 255.0).round() as i32;
        let subs: Vec<Vec<u8>> = (0..4).map(|f| r.render_rgba(&at(f as f32), [64, 32]).unwrap()).collect();
        for i in (0..64 * 32 * 4).step_by(4) {
            let want = enc(subs.iter().map(|p| lin(p[i])).sum::<f32>() / 4.0);
            assert!((frames[0][i] as i32 - want).abs() <= 2, "pixel {}: {} vs {want}", i / 4, frames[0][i]);
        }
    }

    /// Shader plugins written while running: a layer draws, an effect
    /// rewrites what is beneath it, an edit to the file takes effect on the
    /// next render, and a broken one is skipped with its error kept.
    #[test]
    fn shader_plugins_draw_and_reload() {
        let Some(r) = Renderer::headless() else { return eprintln!("no GPU adapter; skipped") };
        let dir = std::env::temp_dir().join(format!("ed_plugins_{}", std::process::id()));
        let write = |name: &str, src: &str| {
            std::fs::create_dir_all(dir.join("plugins").join(name)).unwrap();
            std::fs::write(dir.join("plugins").join(name).join("shader.wgsl"), src).unwrap();
        };
        write("fill", "fn shade(uv: vec2f, px: vec2f) -> vec4f { return vec4f(layer.params.xyz, 1.0); }");
        write("swap", "fn effect(px: vec2f, src: vec4f) -> vec4f { return src.bgra; }");
        r.set_root(dir.clone());
        let mut s = Session::new();
        s.run("set scene res=40,20 bg=#000000\nadd shader a src=fill params=1,0,0 scale=20,20 loc=10,10", Source::Agent).unwrap();
        let px = |s: &Session| r.render_rgba(&ed_core::eval::evaluate(s.doc(), 0.0), [40, 20]).unwrap();
        let at = |p: &[u8], x: usize| <[u8; 3]>::try_from(&p[(10 * 40 + x) * 4..][..3]).unwrap();
        let a = px(&s);
        assert_eq!((at(&a, 5), at(&a, 30)), ([255, 0, 0], [0, 0, 0]), "the layer fills its box with params");

        s.run("add fx f fx=swap z=1", Source::Agent).unwrap();
        assert_eq!(at(&px(&s), 5), [0, 0, 255], "the effect swaps red and blue beneath it");

        std::thread::sleep(std::time::Duration::from_millis(20));
        write("swap", "fn effect(px: vec2f, src: vec4f) -> vec4f { return vec4f(0.0, 1.0, 0.0, 1.0); }");
        assert_eq!(at(&px(&s), 30), [0, 255, 0], "an edited plugin is recompiled");

        write("swap", "fn effect(px: vec2f, src: vec4f) -> vec4f { return nope; }");
        assert_eq!(at(&px(&s), 5), [255, 0, 0], "a broken effect passes the canvas through");
        assert!(r.plugin_error("swap").is_some_and(|e| e.contains("nope")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
