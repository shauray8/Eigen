//! The agent harness: omp (oh-my-pi) running in the embedded terminal,
//! connected to the editor's MCP endpoint. The session the human talks to is
//! the director (the user's default omp model); it delegates building to the
//! `maker` subagent (Opus 5.5 unless omp role `maker` is set).
//! Everything lives in the project's `.omp/` and is rewritten on every launch.
//!
//! The agent runs inside a macOS sandbox (`ed_mcp::sandbox::agent_profile`):
//! it may write only the project, the editor's library, omp's state, temp and
//! caches. With the OS holding that line it needs no approvals (yolo).

use std::path::Path;

use anyhow::Result;

/// MCP server name; omp registers its tools as `mcp__editor_<tool>`.
const SERVER: &str = "editor";
const DIRECTOR_PROMPT: &str = include_str!("agent_system.md");
const MAKER: &str = include_str!("agent_maker.md");
const SYSTEM_FILE: &str = ".omp/editor-system.md";
const CONFIG_FILE: &str = ".omp/editor.yml";
const SANDBOX_FILE: &str = ".omp/sandbox.sb";

/// Writes the project's omp config: the MCP endpoint, the director's prompt,
/// the maker agent, and the approval overlay (none inside the sandbox; without
/// one, everything but the editor's tools and delegation asks the human).
pub fn setup(dir: &Path, port: u16) -> Result<()> {
    std::fs::create_dir_all(dir.join(".omp/agents"))?;
    let mcp = serde_json::json!({ "mcpServers": { SERVER: { "type": "http", "url": ed_mcp::url(port) } } });
    std::fs::write(dir.join(".omp/mcp.json"), serde_json::to_string_pretty(&mcp)?)?;
    std::fs::write(dir.join(SYSTEM_FILE), DIRECTOR_PROMPT)?;
    std::fs::write(dir.join(".omp/agents/maker.md"), MAKER)?;

    let mode = if ed_mcp::sandbox::available() { "yolo" } else { "always-ask" };
    let mut yml = format!("# Written by the editor on every launch.\ntools:\n  approvalMode: {mode}\n  approval:\n    task: allow\n");
    for name in ed_mcp::tool_names() {
        yml += &format!("    mcp__{SERVER}_{name}: allow\n");
    }
    // A project inside the editor's repo would otherwise inherit the repo's
    // developer notes (build commands), inviting the agent to patch the editor.
    yml += "disabledExtensions:\n  - context-file:project:CLAUDE.md\n";
    std::fs::write(dir.join(CONFIG_FILE), yml)?;
    Ok(())
}

/// Shell command that starts the director in the project directory `dir`,
/// sandboxed where the OS can. The profile is rewritten on every start, so
/// nothing the agent wrote into the project can widen it.
pub fn command(dir: &Path) -> String {
    let omp = format!("omp --config {CONFIG_FILE} --system-prompt {SYSTEM_FILE}");
    if ed_mcp::sandbox::available() && std::fs::write(dir.join(SANDBOX_FILE), ed_mcp::sandbox::agent_profile(dir)).is_ok() {
        format!("exec {} -f {SANDBOX_FILE} {omp}", ed_mcp::sandbox::SANDBOX_EXEC)
    } else {
        // No sandbox: the human approves everything but the editor's tools.
        format!("exec {omp} --approval-mode always-ask")
    }
}
