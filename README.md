<p align="center">
  <img src="assets/eigen-icon.png" width="128" alt="Eigen">
</p>

<h1 align="center">Eigen</h1>

<p align="center">An AI-first film editor. The agent builds the film, you direct.</p>

---

Eigen is a native macOS film editor: a 2D canvas with a 3D world on it, a Resolve-style media pool and timeline, and an AI agent working inside it. You give direction, point at things, and review. The agent builds through the same operations the UI uses, so whatever you can do, it can do too.

- **One document, two hands.** Every change, from a UI drag or the agent, is an op-language line run through one session with undo and a log.
- **Director and maker agents** ([omp](https://github.com/can1357/oh-my-pi)): the director plans in passes (a rough cut first, then your notes), and the maker builds. Both run in a macOS sandbox that confines writes to the project, so they need no approval prompts.
- **Pointing language.** Region and object refs (`@R`), review notes (`@N`), storyboard panels (`@P`), and a binding moodboard.
- **Pages.** Edit, Storyboard, Moodboard, References, and Plugins & Skills. tmux-style keys: `Ctrl+A`, then `1`–`5`, `hjkl`, `z`, `?`.
- **Renderer.** wgpu on Metal: a 3D world (PBR, glTF/OBJ, HDRI lighting, soft shadows, depth of field) plus a canvas compositor, with GPU motion blur on export.
- **Grows itself.** The agent writes plugins at runtime: WGSL effects and layers, and sandboxed tool programs (e.g. local text-to-speech on MLX), with no rebuild.
- **Assets.** Poly Haven textures, HDRIs and models; Openverse images; open music catalogs with beat analysis; a catalog of video skills.

## Requirements

macOS on Apple silicon, Rust (stable), `ffmpeg`, and [`omp`](https://github.com/can1357/oh-my-pi) for the agent.

## Run

```sh
cargo run -p ed-app --bin eigen -- <project-dir>   # or no argument: Project Manager
cargo test --workspace
```

The agent connects over MCP on `127.0.0.1:4777` (`ED_PORT` overrides it).

## Layout

| crate | role |
|---|---|
| `ed-core` | document, property paths, keyframes, op language, evaluation, session/undo/log |
| `ed-text` | fonts, shaping, glyph tessellation |
| `ed-audio` | decode, beat/tempo analysis, music search, playback |
| `ed-media` | probing, video decode, thumbnails, project library, Poly Haven / Openverse |
| `ed-render` | wgpu renderer: 3D world pass + canvas compositor |
| `ed-mcp` | the agent's tools (MCP), plugins, sandbox, export |
| `ed-app` | the egui app |

The timeline derives from [FilmCraft](https://github.com/storytold/filmcraft); see [ATTRIBUTION.md](ATTRIBUTION.md).
