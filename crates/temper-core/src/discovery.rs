//! Locate the temper-home folder — the directory holding `temper.toml`.
//!
//! Resolution order: `$TEMPER_DIR` (explicit) → walk up from the cwd (you're
//! inside a folder) → a saved pointer (`temper use` writes it) → an auto-scan of
//! common locations. temper is delivery-agnostic: it never runs git or a sync
//! client — it only needs a path that contains a manifest.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

fn has_manifest(dir: &Path) -> bool {
    dir.join("temper.toml").is_file()
}

pub fn find_home() -> Result<PathBuf> {
    // 1. Explicit override.
    if let Ok(d) = std::env::var("TEMPER_DIR") {
        let p = PathBuf::from(d);
        if has_manifest(&p) {
            return Ok(p);
        }
        bail!("TEMPER_DIR={} has no temper.toml", p.display());
    }
    // 2. Walk up from the cwd (you're inside a temper folder).
    let mut dir = std::env::current_dir()?;
    loop {
        if has_manifest(&dir) {
            return Ok(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    // 3. A saved pointer (your default home; `temper use <dir>` writes it).
    if let Some(p) = saved_pointer() {
        if has_manifest(&p) {
            return Ok(p);
        }
    }
    // 4. Auto-scan common locations (git checkout / cloud folder / USB).
    if let Some(p) = auto_scan() {
        return Ok(p);
    }
    bail!(
        "no temper.toml found — set TEMPER_DIR, run inside your temper folder, \
         or `temper use <dir>` to save its location"
    )
}

/// The saved-pointer file (`$XDG_CONFIG_HOME/temper/home`, else `~/.config/…`).
fn pointer_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("temper").join("home"))
}

/// Read the saved home pointer, if it points at an existing directory.
fn saved_pointer() -> Option<PathBuf> {
    let s = fs::read_to_string(pointer_path()?).ok()?;
    let path = PathBuf::from(s.trim());
    path.is_dir().then_some(path)
}

/// Persist `dir` as the default temper-home. Returns the pointer file written.
pub fn save_pointer(dir: &Path) -> Result<PathBuf> {
    if !has_manifest(dir) {
        bail!("{} has no temper.toml — not saving it as the temper home", dir.display());
    }
    let p = pointer_path().context("cannot resolve a config dir (no HOME/XDG_CONFIG_HOME)")?;
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    let abs = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    fs::write(&p, abs.display().to_string()).with_context(|| format!("writing {}", p.display()))?;
    Ok(p)
}

/// First common location that contains a `temper.toml`. Covers a git checkout, a
/// synced cloud folder, or a mounted disk — the ways a folder tends to arrive.
fn auto_scan() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let names = ["steel", "temper-home", ".temper"];
    let roots = [
        home.clone(),
        home.join("Developer"),
        home.join("Nextcloud"),
        home.join("Dropbox"),
        home.join("Library/CloudStorage"),
        PathBuf::from("/media"),
        PathBuf::from("/run/media").join(std::env::var("USER").unwrap_or_default()),
    ];
    for root in roots {
        for name in names {
            let cand = root.join(name);
            if has_manifest(&cand) {
                return Some(cand);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_round_trip() {
        let xdg = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        fs::write(home.path().join("temper.toml"), "").unwrap();
        // Point XDG at a temp dir so we never touch the real config.
        std::env::set_var("XDG_CONFIG_HOME", xdg.path());
        let written = save_pointer(home.path()).unwrap();
        assert!(written.exists());
        let read = saved_pointer().unwrap();
        assert!(has_manifest(&read));
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn save_refuses_dir_without_manifest() {
        let empty = tempfile::TempDir::new().unwrap();
        assert!(save_pointer(empty.path()).is_err());
    }
}
