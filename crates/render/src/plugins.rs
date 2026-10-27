//! Shader plugins: WGSL the agent (or anyone) writes while the editor runs,
//! in `<project>/plugins/<name>/shader.wgsl` or the shared library
//! `~/.editor/plugins/<name>/shader.wgsl`. Compiled on first use and again
//! whenever the file changes; a broken shader is skipped and its error kept.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const FX_PRELUDE: &str = include_str!("plugin_fx.wgsl");
const LAYER_PRELUDE: &str = include_str!("plugin_layer.wgsl");

/// How a shader plugin is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Use {
    /// Adjustment layer (`add fx x fx=<name>`): defines `effect(px, src)`.
    Fx,
    /// Its own layer (`add shader x src=<name>`): defines `shade(uv, px)`.
    Layer,
}

impl Use {
    fn prelude(self) -> &'static str {
        match self {
            Use::Fx => FX_PRELUDE,
            Use::Layer => LAYER_PRELUDE,
        }
    }
}

/// The shared plugin library, used by every project.
pub fn library_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".editor/plugins"))
}

/// Folder of plugin `name`: the project's own first, then the library.
pub fn dir_of(project: &Path, name: &str) -> Option<PathBuf> {
    [Some(project.join("plugins")), library_dir()].into_iter().flatten().map(|d| d.join(name)).find(|d| d.is_dir())
}

/// The whole module the GPU compiles. The plugin's code comes first so the
/// line numbers in errors are the plugin's own (WGSL declarations may appear in any order).
pub fn module_source(usage: Use, src: &str) -> String {
    format!("{src}\n\n{}", usage.prelude())
}

/// Compiles plugin source for `usage` without a GPU; the error names lines
/// of the plugin's own file.
pub fn check(usage: Use, src: &str) -> Result<(), String> {
    let full = module_source(usage, src);
    let module = naga::front::wgsl::parse_str(&full).map_err(|e| e.emit_to_string(&full))?;
    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::default())
        .validate(&module)
        .map_err(|e| e.emit_to_string(&full))?;
    Ok(())
}

pub(crate) struct Compiled {
    pub pipeline: Option<wgpu::RenderPipeline>,
    pub error: Option<String>,
    stamp: Option<SystemTime>,
    path: PathBuf,
}

/// Compiled pipelines by (plugin, use), rebuilt when their file changes.
#[derive(Default)]
pub(crate) struct Cache {
    entries: HashMap<(String, Use), Compiled>,
}

impl Cache {
    /// The pipeline for `name`, compiling (or recompiling) as needed. None if
    /// the plugin is missing or broken (see [`Cache::error`]).
    pub fn get(&mut self, project: &Path, name: &str, usage: Use, build: impl FnOnce(&str) -> wgpu::RenderPipeline) -> Option<&wgpu::RenderPipeline> {
        let path = dir_of(project, name).map(|d| d.join("shader.wgsl"));
        let stamp = path.as_ref().and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
        let key = (name.to_string(), usage);
        let fresh = self.entries.get(&key).is_some_and(|c| c.stamp == stamp && Some(&c.path) == path.as_ref());
        if !fresh {
            let compiled = match path.as_ref().map(std::fs::read_to_string) {
                None | Some(Err(_)) => Compiled { pipeline: None, error: Some(format!("no plugin `{name}` (plugins/{name}/shader.wgsl)")), stamp, path: path.clone().unwrap_or_default() },
                Some(Ok(src)) => match check(usage, &src) {
                    Ok(()) => Compiled { pipeline: Some(build(&module_source(usage, &src))), error: None, stamp, path: path.clone().unwrap_or_default() },
                    Err(e) => Compiled { pipeline: None, error: Some(e), stamp, path: path.clone().unwrap_or_default() },
                },
            };
            self.entries.insert(key.clone(), compiled);
        }
        self.entries[&key].pipeline.as_ref()
    }

    /// Why plugin `name` did not draw, if it did not.
    pub fn error(&self, name: &str) -> Option<String> {
        self.entries.iter().find(|((n, _), c)| n == name && c.error.is_some()).and_then(|(_, c)| c.error.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_name_the_plugins_own_lines() {
        assert!(check(Use::Layer, "fn shade(uv: vec2f, px: vec2f) -> vec4f { return vec4f(uv, layer.params.x, 1.0); }").is_ok());
        let err = check(Use::Fx, "fn effect(px: vec2f, src: vec4f) -> vec4f {\n  return src * nope;\n}").unwrap_err();
        assert!(err.contains("nope") && err.contains(":2:"), "{err}");
        assert!(check(Use::Fx, "fn shade(uv: vec2f, px: vec2f) -> vec4f { return vec4f(1.0); }").is_err(), "an fx plugin must define effect()");
    }
}
