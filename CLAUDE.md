# Eigen

AI-first film editor: a 2D canvas with 3D available on it, driven equally by a human and an agent (omp). Rust workspace. Logo: `assets/eigen.svg` (app icon `assets/eigen-icon.png`).

| crate | role |
|---|---|
| `ed-core` | document (`data`), property paths (`paths`), keyframes (`anim`), operators (`ops`), op language (`lang`), evaluation (`eval`), queries, session/undo/log |
| `ed-text` | system fonts (fontdb), shaping (rustybuzz), glyph outlines tessellated to triangles (lyon) |
| `ed-audio` | ffmpeg decode, aubio beat/tempo analysis, music search (Openverse, ccMixter, archive.org, local) + fetch, cpal playback |
| `ed-media` | ffprobe, images, streaming video decode, thumbnails, project library (`media/`, `audio/`), Openverse images, Poly Haven textures (`textures/<id>/`), HDRIs (`hdri/<id>.hdr`) and glTF models (`models/<id>/`) |
| `ed-render` | wgpu renderer (Metal on macOS): 3D world pass + canvas compositor; same path for viewport, agent renders and export (`FrameStream`: GPU motion blur, double-buffered readback); shader plugins (`plugins.rs`, hot-reloaded WGSL); models from OBJ (`prims.rs`) or glTF/GLB with their maps (`model.rs`) |
| `ed-mcp` | in-process MCP server: the agent's tools (`ops`, `q`, `render`, `export`, `undo`, `log`, `library`, `fetch`, `images`, `textures`, `music`, `track`, `ui`, `ref`, `notes`, `plugin`, `request`, `storyboard`, `moodboard`); refs (`refs.rs`); plugin runtime (`plugins.rs`, sandboxed tools); moodboards (`moodboard.rs`); ffmpeg export with audio mix to `<project>/exports/<project>-NNN.mp4` |
| `ed-app` | egui shell. Pages via the bottom status bar: Edit (docked viewer/source/timeline/pool/layers/properties/log), Storyboard, Moodboard, References, Plugins & Skills (`extensions.rs`, video skill catalog `skill_catalog.rs`). The agent terminal (`omp`; `agent.rs` writes `.omp/` on launch) is a persistent right pane; pointing or "Send to agent" types into its prompt (`Terminal::insert`) without sending. Timeline ported from FilmCraft (`timeline.rs`, see `ATTRIBUTION.md`). Native macOS menu (`menu.rs`, muda), Project Manager (`projects.rs`), theme (`theme.rs`), tmux keys (`tmux.rs`: Ctrl+A prefix, then 1-5 pages, arrows/hjkl focus, z zoom, ? help) |

Model: the canvas (scene res, px) is the film frame. Canvas layers (rect, ellipse, text, view) stack by `z`; a `view` shows the 3D world through a camera. Every object has `in`/`out` frames; hidden groups hide their children. `fx` layers are adjustment layers (effect everything beneath). Meshes and shapes can be live arrays (`rep`, `spacing`, ...). `audio` objects + `scene.music` give beat-addressed frames (`@b16`, `in=b32`).
Compositions: a `comp` object is its own canvas (`scale` = size, `out` = length, `color` = bg, `camera`); objects with `comp=<name>` live in it (`in <name>` in scripts), with their own frames and 3D world. A `clip` layer (`src=<comp>`) plays a comp like a video; clips nest, loops are rejected. Names stay global. `eval::evaluate_comp(doc, comp, frame)` evaluates one; the renderer draws clips into pooled offscreen targets before the views.
Growing at runtime: the agent writes plugins instead of asking for editor changes. `plugins/<name>/` (project) or `~/.editor/plugins/<name>/` (library) with `plugin.json`: `fx` (WGSL `effect()`, used as `fx=<name>`), `layer` (WGSL `shade()`, used by `shader` layers), `tool` (any program; JSON in, `{ops, text}` out; run under `sandbox-exec`, writes only in the project). Expressions (`expr obj.prop "..."`, Rhai, `expr.rs`) compute properties per frame after keys. Only what no plugin can do becomes an editor change: the agent's `request` tool files a `tt` task here.

Rules:
- The document only changes through `Session::run` (op-language text). UI widgets emit op lines; never mutate `data` directly. This is what keeps human and agent at parity.
- Refs (`@R<n>` object/region, `@N<n>` review note) are annotations, not film content: they live in `<project>/refs.json` beside the document, outside the op log and undo.
- New property: add it to `paths.rs` (get/set, `OBJECT_PROPS`, `props_for`); ops, queries, animation, the UI property editor and the file format pick it up.
- Evaluation is deterministic: `eval::evaluate(doc, frame)`.
- Op language reference shown to the agent lives in `crates/mcp/src/instructions.md`; keep it short (it costs tokens every session).
- Agents: the omp session the human talks to is the director (`crates/app/src/agent_system.md`); it delegates building to the `maker` subagent (`agent_maker.md`, Opus 5.5 unless omp role `maker` is set). Director works in passes: a fast rough draft of the whole film for the human to note, then refinement. Skills ticked for the project and an active moodboard are binding. The agent works in the editor's tools first (ops, expressions, plugin tools), not ad-hoc scripts. Film sessions must not patch the editor's source.
- Sandbox: omp runs under `sandbox-exec` with `ed_mcp::sandbox::agent_profile` (written to `.omp/sandbox.sb` on every start): writes only the project, `~/.editor`, omp's state (not its config, hooks, extensions, global agents/skills), temp and caches; no reads of `~/.ssh` etc. Inside it approvals are off (`yolo`); without `sandbox-exec` everything but the editor's tools asks. Tool plugins use the same module.
- Native menu accelerators and app shortcuts avoid single keys and Shift-combos that the agent's terminal needs; tmux keys go through the Ctrl+A prefix, which `tmux::Prefix` strips from the input before the terminal sees it.

Run: `cargo run -p ed-app --bin eigen -- <project-dir>` (MCP on 127.0.0.1:4777, override with `ED_PORT`).
Test: `cargo test --workspace`.
