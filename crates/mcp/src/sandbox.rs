//! macOS `sandbox-exec` profiles: everything is allowed except writing
//! outside the listed folders and reading the listed secrets. Used for tool
//! plugins and for the agent itself, which then needs no approvals: the OS
//! keeps it inside the project.

use std::path::{Path, PathBuf};

pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Whether this machine can sandbox (macOS with `sandbox-exec`).
pub fn available() -> bool {
    cfg!(target_os = "macos") && Path::new(SANDBOX_EXEC).exists()
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

/// Folders every sandboxed process may write: temp, caches, devices.
pub fn scratch() -> Vec<PathBuf> {
    let h = home();
    vec![
        "/private/tmp".into(),
        "/private/var/folders".into(),
        "/dev".into(),
        h.join(".cache"),
        h.join("Library/Caches"),
        h.join(".npm"),
        h.join(".bun/install/cache"),
    ]
}

/// Credentials no sandboxed process may read.
pub fn secrets() -> Vec<PathBuf> {
    let h = home();
    [".ssh", ".aws", ".gnupg", ".config/gcloud", ".kube", ".docker", ".netrc", ".azure"].iter().map(|p| h.join(p)).collect()
}

/// `p` with symlinks resolved, also when it does not exist yet (the kernel
/// matches resolved paths: `/tmp` is `/private/tmp`).
fn resolve(p: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut at = p;
    loop {
        if let Ok(real) = at.canonicalize() {
            return rest.iter().rev().fold(real, |acc: PathBuf, part| acc.join(part));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                at = parent;
            }
            _ => return p.to_path_buf(),
        }
    }
}

/// The profile text. Later rules win, so `deny_write` carves holes in `write`.
pub fn profile(write: &[PathBuf], deny_write: &[PathBuf], deny_read: &[PathBuf]) -> String {
    let q = |p: &Path| format!("(subpath \"{}\")", resolve(p).to_string_lossy().replace('\\', "\\\\").replace('"', "\\\""));
    let list = |ps: &[PathBuf]| ps.iter().map(|p| q(p)).collect::<Vec<_>>().join(" ");
    let mut s = String::from("(version 1)\n(allow default)\n(deny file-write*)\n");
    s += &format!("(allow file-write* {})\n", list(write));
    if !deny_write.is_empty() {
        s += &format!("(deny file-write* {})\n", list(deny_write));
    }
    if !deny_read.is_empty() {
        s += &format!("(deny file-read* {})\n", list(deny_read));
    }
    s
}

/// Profile for the agent (omp) working on `project`: it writes the project,
/// the editor's library (`~/.editor`), omp's own state, temp and caches; it
/// cannot change omp's configuration (hooks, extensions, global agents and
/// skills would run unsandboxed in the human's other sessions) or read secrets.
pub fn agent_profile(project: &Path) -> String {
    let h = home();
    let omp = h.join(".omp");
    let mut write = vec![project.to_path_buf(), h.join(".editor"), omp.clone()];
    write.extend(scratch());
    let a = omp.join("agent");
    let mut deny_write: Vec<PathBuf> = ["config.yml", "config.yaml", "settings.json", "mcp.json", "AGENTS.md", "SYSTEM.md", "APPEND_SYSTEM.md"]
        .iter()
        .map(|f| a.join(f))
        .collect();
    for d in ["agents", "skills", "hooks", "extensions", "tools", "rules", "commands", "prompts", "plugins"] {
        deny_write.push(a.join(d));
    }
    deny_write.push(omp.join("plugins"));
    profile(&write, &deny_write, &secrets())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `sh -c script` under `profile`; true when it succeeded.
    fn run(profile: &str, script: &str) -> bool {
        std::process::Command::new(SANDBOX_EXEC).arg("-p").arg(profile).args(["/bin/sh", "-c", script]).status().unwrap().success()
    }

    #[test]
    fn agent_writes_only_inside_its_folders() {
        if !available() {
            return;
        }
        let root = std::env::temp_dir().join(format!("ed_sandbox_{}", std::process::id()));
        let project = root.join("project");
        let outside = root.join("outside");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        // The scratch folders include temp, so test "outside" with a profile
        // whose only writable folder is the project.
        let p = profile(&[project.clone()], &[project.join("locked")], &[outside.clone()]);
        std::fs::create_dir_all(project.join("locked")).unwrap();
        std::fs::write(outside.join("secret"), "x").unwrap();
        assert!(run(&p, &format!("echo hi > '{}'", project.join("a.txt").display())));
        assert!(!run(&p, &format!("echo hi > '{}'", outside.join("b.txt").display())));
        assert!(!run(&p, &format!("echo hi > '{}'", project.join("locked/c.txt").display())));
        assert!(!run(&p, &format!("cat '{}'", outside.join("secret").display())));
        std::fs::remove_dir_all(&root).ok();
    }
}
