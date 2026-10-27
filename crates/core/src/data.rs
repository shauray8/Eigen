//! DNA: the plain data of a document. No behaviour lives here.

use std::collections::BTreeMap;

use glam::Vec3;
use indexmap::IndexMap;

use crate::anim::FCurve;

#[derive(Clone, Debug, Default)]
pub struct Document {
    pub scene: Scene,
    /// Objects by name. Names are the stable IDs the agent and the UI both use.
    pub objects: IndexMap<String, Object>,
}

#[derive(Clone, Debug)]
pub struct Scene {
    pub fps: f32,
    pub start: i32,
    pub end: i32,
    /// Output resolution. Also the coordinate space of 2D layers.
    pub res: [u32; 2],
    pub camera: Option<String>,
    pub bg: [f32; 4],
    pub ambient: f32,
    /// Audio object whose beat grid `@bN` frame references use.
    pub music: Option<String>,
    /// Equirect HDR image (project path) lighting and reflecting on meshes; scaled by `ambient`.
    pub env: String,
    /// Environment yaw in degrees.
    pub envrot: f32,
    /// Bloom/halation mix (0 = off).
    pub bloom: f32,
    /// Export motion blur: shutter open for this fraction of a frame (0.5 = 180 degrees; 0 = off).
    pub shutter: f32,
    /// Film grain on 3D views (0-1).
    pub grain: f32,
    /// Lens vignette on 3D views (0-1).
    pub vignette: f32,
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            fps: 24.0,
            start: 0,
            end: 120,
            res: [1920, 1080],
            camera: None,
            bg: [0.05, 0.05, 0.06, 1.0],
            ambient: 0.15,
            music: None,
            env: String::new(),
            envrot: 0.0,
            bloom: 0.0,
            shutter: 0.0,
            grain: 0.0,
            vignette: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    // 3D
    Cube,
    Sphere,
    Plane,
    Cylinder,
    /// Triangle mesh from an OBJ, glTF or GLB file (`src`), drawn like the primitives.
    Model,
    Camera,
    Light,
    Empty,
    /// Point cloud from a file (`src`): round dots of `cell` px.
    Points,
    // Canvas layers, in scene-resolution pixels
    Rect,
    Ellipse,
    Text,
    /// Shows the 3D world through a camera, as a layer on the canvas.
    View,
    /// Adjustment layer: applies an effect to everything beneath it.
    Fx,
    /// A sound on the timeline (not drawn).
    Audio,
    /// A picture file on the canvas.
    Image,
    /// A video file on the canvas (its sound plays too).
    Video,
    /// A composition: its own canvas (`scale` = w,h px, 0 = scene res; `color`
    /// = background, default clear; `out` = length in frames; `camera`) holding
    /// the objects whose `comp` names it. The media pool's built pieces.
    Comp,
    /// A canvas layer playing a composition (`src` = comp name), like a video.
    Clip,
    /// A canvas layer drawn by a shader plugin (`src` = plugin name, `params`).
    Shader,
}

impl Kind {
    pub const ALL: [Kind; 20] = [
        Kind::Cube,
        Kind::Sphere,
        Kind::Plane,
        Kind::Cylinder,
        Kind::Model,
        Kind::Camera,
        Kind::Light,
        Kind::Empty,
        Kind::Points,
        Kind::Rect,
        Kind::Ellipse,
        Kind::Text,
        Kind::View,
        Kind::Fx,
        Kind::Audio,
        Kind::Image,
        Kind::Video,
        Kind::Comp,
        Kind::Clip,
        Kind::Shader,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Cube => "cube",
            Kind::Sphere => "sphere",
            Kind::Plane => "plane",
            Kind::Cylinder => "cylinder",
            Kind::Model => "model",
            Kind::Camera => "camera",
            Kind::Light => "light",
            Kind::Empty => "empty",
            Kind::Points => "points",
            Kind::Rect => "rect",
            Kind::Ellipse => "ellipse",
            Kind::Text => "text",
            Kind::View => "view",
            Kind::Fx => "fx",
            Kind::Audio => "audio",
            Kind::Image => "image",
            Kind::Video => "video",
            Kind::Comp => "comp",
            Kind::Clip => "clip",
            Kind::Shader => "shader",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.name() == s)
    }

    pub fn is_mesh(self) -> bool {
        matches!(self, Kind::Cube | Kind::Sphere | Kind::Plane | Kind::Cylinder | Kind::Model)
    }

    pub fn is_2d(self) -> bool {
        matches!(
            self,
            Kind::Rect | Kind::Ellipse | Kind::Text | Kind::View | Kind::Fx | Kind::Image | Kind::Video | Kind::Clip | Kind::Shader
        )
    }
}

#[derive(Clone, Debug)]
pub struct Object {
    pub kind: Kind,
    pub parent: Option<String>,
    /// Composition the object belongs to (None: the film itself).
    pub comp: Option<String>,
    pub loc: Vec3,
    /// Euler XYZ in degrees.
    pub rot: Vec3,
    pub scale: Vec3,
    /// Base color, linear RGBA.
    pub color: [f32; 4],
    pub visible: bool,
    /// Multiplies the layer's alpha.
    pub opacity: f32,
    /// First frame the object exists (inclusive). None: from the start.
    pub t_in: Option<f32>,
    /// Frame the object stops existing (exclusive). None: to the end.
    pub t_out: Option<f32>,
    /// Camera a view layer looks through. None: the scene camera.
    pub camera: Option<String>,
    /// Camera vertical field of view in degrees.
    pub fov: f32,
    /// Light power.
    pub energy: f32,
    /// Camera focus distance (scene units) for depth of field.
    pub focus: f32,
    /// Camera lens aperture diameter in scene units (0 = everything sharp).
    pub aperture: f32,
    /// Camera atmospheric haze density per scene unit; haze color is the camera's `color`.
    pub haze: f32,
    /// Light shadow softness (light radius in scene units; 0 = no shadow).
    pub shadow: f32,
    /// Points: how much scene lights shade the dots (0 = flat color).
    pub lit: f32,
    /// 2D corner radius in pixels.
    pub radius: f32,
    /// Draw order for 2D layers (higher on top).
    pub z: f32,
    /// Text layer data (used when `kind` is `Text`).
    pub text: TextData,
    pub material: Material,
    pub repeat: Repeat,
    pub fx: FxData,
    pub media: MediaData,
    /// Animation: paths path (component level, e.g. `loc.z`) -> curve.
    pub anim: BTreeMap<String, FCurve>,
    /// Expressions by property path (`loc` or `loc.y`), run after the keys
    /// every frame (see `expr`).
    pub expr: BTreeMap<String, String>,
}

impl Object {
    /// Whether the object exists at `frame` (its in/out range).
    pub fn alive(&self, frame: f32) -> bool {
        self.t_in.is_none_or(|t| frame >= t) && self.t_out.is_none_or(|t| frame < t)
    }

    pub fn new(kind: Kind) -> Self {
        let (scale, color) = match kind {
            Kind::Text => (Vec3::ONE, [1.0, 1.0, 1.0, 1.0]),
            // Scene resolution and a clear background until set.
            Kind::Comp => (Vec3::new(0.0, 0.0, 1.0), [0.0; 4]),
            k if k.is_2d() => (Vec3::new(200.0, 200.0, 1.0), [1.0, 1.0, 1.0, 1.0]),
            _ => (Vec3::ONE, [0.8, 0.8, 0.8, 1.0]),
        };
        let fx = if kind == Kind::Points { FxData { cell: 2.0, ..FxData::default() } } else { FxData::default() };
        Self {
            kind,
            parent: None,
            comp: None,
            loc: Vec3::ZERO,
            rot: Vec3::ZERO,
            scale,
            color,
            visible: true,
            opacity: 1.0,
            t_in: None,
            t_out: None,
            camera: None,
            fov: 40.0,
            energy: 10.0,
            focus: 10.0,
            aperture: 0.0,
            haze: 0.0,
            shadow: 0.0,
            lit: 0.0,
            radius: 0.0,
            z: 0.0,
            text: TextData::default(),
            material: Material::default(),
            repeat: Repeat::default(),
            fx,
            media: MediaData::default(),
            anim: BTreeMap::new(),
            expr: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Align {
    Left,
    #[default]
    Center,
    Right,
}

impl Align {
    pub const ALL: [Align; 3] = [Align::Left, Align::Center, Align::Right];

    pub fn name(self) -> &'static str {
        match self {
            Align::Left => "left",
            Align::Center => "center",
            Align::Right => "right",
        }
    }
}

/// Order in which letters start their staggered animation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Order {
    #[default]
    Forward,
    Reverse,
    Center,
    Edges,
    Random,
}

impl Order {
    pub const ALL: [Order; 5] = [Order::Forward, Order::Reverse, Order::Center, Order::Edges, Order::Random];

    pub fn name(self) -> &'static str {
        match self {
            Order::Forward => "forward",
            Order::Reverse => "reverse",
            Order::Center => "center",
            Order::Edges => "edges",
            Order::Random => "random",
        }
    }
}

/// Text layer. Like Blender's text curve data, plus a per-letter animator:
/// `gloc`/`grot`/`gscale`/`galpha` are offsets applied to every letter, each
/// letter reading its curves `stagger` frames after the previous one.
#[derive(Clone, Debug, PartialEq)]
pub struct TextData {
    pub body: String,
    pub font: String,
    pub weight: f32,
    /// Em size in scene pixels.
    pub size: f32,
    pub align: Align,
    /// Extra space between letters, in pixels.
    pub tracking: f32,
    /// Line height as a multiple of size.
    pub leading: f32,
    pub stagger: f32,
    pub order: Order,
    /// Per-letter offset in pixels.
    pub gloc: Vec3,
    /// Per-letter rotation in degrees (z).
    pub grot: f32,
    pub gscale: Vec3,
    pub galpha: f32,
    /// Per-letter random offset amplitude in pixels (each letter has its own direction).
    pub gspread: Vec3,
    /// Per-letter random rotation amplitude in degrees.
    pub gspin: f32,
}

impl Default for TextData {
    fn default() -> Self {
        Self {
            body: "Text".into(),
            font: "Helvetica Neue".into(),
            weight: 400.0,
            size: 120.0,
            align: Align::Center,
            tracking: 0.0,
            leading: 1.1,
            stagger: 0.0,
            order: Order::Forward,
            gloc: Vec3::ZERO,
            grot: 0.0,
            gscale: Vec3::ONE,
            galpha: 1.0,
            gspread: Vec3::ZERO,
            gspin: 0.0,
        }
    }
}

/// Surface of a 3D object. All 0..1; real-time approximations of a
/// principled surface (chrome = metal 1 rough 0.1, glass = glass 1).
#[derive(Clone, Debug, PartialEq)]
pub struct Material {
    pub metal: f32,
    pub rough: f32,
    pub glass: f32,
    /// Fur / fleece length as a fraction of the object's size.
    pub fuzz: f32,
    /// Texture set in `<project>/textures/<tex>/` (color, normal, rough maps).
    pub tex: String,
    /// Texture repeats across the object.
    pub uvscale: f32,
    /// Unlit: 1 shows the texture's own colors exactly (painted cards), 0 = lit.
    pub emit: f32,
    /// Soft edges: drawn blended, back to front, without writing depth (mist, smoke).
    pub soft: f32,
    /// Wind: amplitude (units), then the object-space heights where it fades
    /// in from nothing (x) to full (y). Leaves and grass bend; ground stays.
    pub sway: glam::Vec3,
}

impl Default for Material {
    fn default() -> Self {
        Self { metal: 0.0, rough: 0.45, glass: 0.0, fuzz: 0.0, tex: String::new(), uvscale: 1.0, emit: 0.0, soft: 0.0, sway: glam::Vec3::ZERO }
    }
}

/// Live array: the object is drawn `count` times on a grid, centered on it.
#[derive(Clone, Debug, PartialEq)]
pub struct Repeat {
    /// Copies along local x, y, z.
    pub count: Vec3,
    /// Distance between copies along local x, y, z.
    pub spacing: Vec3,
    /// Extra rotation (deg) per copy index.
    pub rot: Vec3,
    /// Random rotation amplitude (deg) per copy.
    pub jitter: Vec3,
    /// Random brightness variation 0..1 per copy.
    pub shade: f32,
    /// Copy i plays its animation `delay * i` frames later.
    pub delay: f32,
}

impl Default for Repeat {
    fn default() -> Self {
        Self { count: Vec3::ONE, spacing: Vec3::ONE, rot: Vec3::ZERO, jitter: Vec3::ZERO, shade: 0.0, delay: 0.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FxKind {
    #[default]
    Blur,
    Pixelate,
    Halftone,
    Chroma,
    Grain,
    Glitch,
    Invert,
    Grade,
    Stipple,
}

impl FxKind {
    pub const ALL: [FxKind; 9] = [
        FxKind::Blur,
        FxKind::Pixelate,
        FxKind::Halftone,
        FxKind::Chroma,
        FxKind::Grain,
        FxKind::Glitch,
        FxKind::Invert,
        FxKind::Grade,
        FxKind::Stipple,
    ];

    pub fn name(self) -> &'static str {
        match self {
            FxKind::Blur => "blur",
            FxKind::Pixelate => "pixelate",
            FxKind::Halftone => "halftone",
            FxKind::Chroma => "chroma",
            FxKind::Grain => "grain",
            FxKind::Glitch => "glitch",
            FxKind::Invert => "invert",
            FxKind::Grade => "grade",
            FxKind::Stipple => "stipple",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FxData {
    pub kind: FxKind,
    /// A shader plugin (`plugins/<name>/`) used instead of `kind` (fx layers),
    /// or the generator a `shader` layer draws. Empty: built-in.
    pub plugin: String,
    /// Mix 0..1 (strength).
    pub amount: f32,
    /// Size in pixels (blur radius, pixel/dot cell, split distance).
    pub cell: f32,
    /// Four free numbers a shader plugin reads (its manifest says what they mean).
    pub params: [f32; 4],
}

impl Default for FxData {
    fn default() -> Self {
        Self { kind: FxKind::Blur, plugin: String::new(), amount: 1.0, cell: 16.0, params: [0.0; 4] }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Fit {
    /// Fill the layer box, cropping the overflow.
    #[default]
    Cover,
    /// Fit inside the layer box, keeping the whole picture.
    Contain,
    Stretch,
}

impl Fit {
    pub const ALL: [Fit; 3] = [Fit::Cover, Fit::Contain, Fit::Stretch];

    pub fn name(self) -> &'static str {
        match self {
            Fit::Cover => "cover",
            Fit::Contain => "contain",
            Fit::Stretch => "stretch",
        }
    }
}

/// File-backed data of audio, image and video objects.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaData {
    /// File, relative to the project directory.
    pub src: String,
    /// How a picture fills the layer box.
    pub fit: Fit,
    /// Seconds skipped at the start of the file.
    pub trim: f32,
    pub volume: f32,
    /// Tempo for beat references (0: unknown).
    pub bpm: f32,
    /// Time of the first beat in the file, seconds.
    pub beat0: f32,
}

impl Default for MediaData {
    fn default() -> Self {
        Self { src: String::new(), fit: Fit::Cover, trim: 0.0, volume: 1.0, bpm: 0.0, beat0: 0.0 }
    }
}

impl Document {
    /// Canvas size (px) and length (frames) of composition `comp`; `None` or
    /// an unknown name is the film itself.
    pub fn comp_canvas(&self, comp: Option<&str>) -> ([u32; 2], f32) {
        let film = (self.scene.res, (self.scene.end + 1) as f32);
        match comp.and_then(|c| self.objects.get(c)).filter(|o| o.kind == Kind::Comp) {
            Some(o) => {
                let res = if o.scale.x >= 1.0 && o.scale.y >= 1.0 { [o.scale.x as u32, o.scale.y as u32] } else { film.0 };
                (res, o.t_out.unwrap_or(film.1))
            }
            None => film,
        }
    }

    /// Whether composition `from` shows `target`, directly or through clips
    /// of clips. Placing a clip of `from` inside `target` would loop.
    pub fn comp_shows(&self, from: &str, target: &str) -> bool {
        let mut stack = vec![from];
        let mut seen = std::collections::HashSet::new();
        while let Some(c) = stack.pop() {
            if c == target {
                return true;
            }
            if !seen.insert(c) {
                continue;
            }
            stack.extend(
                self.objects.values().filter(|o| o.kind == Kind::Clip && o.comp.as_deref() == Some(c)).map(|o| o.media.src.as_str()),
            );
        }
        false
    }

    /// Frame of beat `n` of the scene's music (`@bN`): beat 0 is the first
    /// downbeat inside the clip, so bars start at b0, b4, b8...
    pub fn beat_frame(&self, n: f32) -> Result<f32, String> {
        let name = self.scene.music.as_deref().ok_or("beat references need `set scene.music <audio>`")?;
        let o = self.objects.get(name).ok_or_else(|| format!("no audio `{name}`"))?;
        let a = &o.media;
        if a.bpm <= 0.0 {
            return Err(format!("`{name}` has no bpm (run the audio analysis)"));
        }
        // Beat 0 is the first downbeat at or after the clip's start in the file.
        let period = 60.0 / a.bpm;
        let first_bar = ((a.trim - a.beat0) / (4.0 * period) - 1e-3).ceil() * 4.0;
        let secs = a.beat0 - a.trim + (first_bar + n) * period;
        Ok((o.t_in.unwrap_or(0.0) + secs * self.scene.fps).round())
    }
}
