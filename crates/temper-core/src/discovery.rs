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
    // 4. Auto-scan common locations (git checkout / cloud folder / USB). One
    //    match → use it; several → refuse and let the user choose (a fleet may
    //    have more than one library — never silently guess).
    let candidates = scan_candidates();
    match candidates.len() {
        0 => bail!(
            "no temper.toml found — run `temper setup`, set TEMPER_DIR, or run \
             inside your temper folder"
        ),
        1 => Ok(candidates.into_iter().next().expect("len==1")),
        _ => {
            let list = candidates
                .iter()
                .map(|p| format!("  {}", p.display()))
                .collect::<Vec<_>>()
                .join("\n");
            bail!(
                "found several temper-homes — run `temper setup` to choose one \
                 (or set TEMPER_DIR):\n{list}"
            )
        }
    }
}

/// The config base (`$XDG_CONFIG_HOME`, else `~/.config`) that holds temper's
/// saved-home pointer. `None` when neither is set.
fn config_base() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}

/// The saved-pointer file under a config base (`<base>/temper/home`). Base is a
/// param so the round-trip is testable without mutating process-global env.
fn pointer_in(base: &Path) -> PathBuf {
    base.join("temper").join("home")
}

/// Read the saved home pointer under `base`, if it points at an existing dir.
fn saved_pointer_in(base: &Path) -> Option<PathBuf> {
    let s = fs::read_to_string(pointer_in(base)).ok()?;
    let path = PathBuf::from(s.trim());
    path.is_dir().then_some(path)
}

/// Read the saved home pointer, if it points at an existing directory.
fn saved_pointer() -> Option<PathBuf> {
    saved_pointer_in(&config_base()?)
}

/// Persist `dir` as the default temper-home under `base`. Returns the pointer
/// file written.
fn save_pointer_in(base: &Path, dir: &Path) -> Result<PathBuf> {
    if !has_manifest(dir) {
        bail!(
            "{} has no temper.toml — not saving it as the temper home",
            dir.display()
        );
    }
    let p = pointer_in(base);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    let abs = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    fs::write(&p, abs.display().to_string()).with_context(|| format!("writing {}", p.display()))?;
    Ok(p)
}

/// Persist `dir` as the default temper-home. Returns the pointer file written.
pub fn save_pointer(dir: &Path) -> Result<PathBuf> {
    let base = config_base().context("cannot resolve a config dir (no HOME/XDG_CONFIG_HOME)")?;
    save_pointer_in(&base, dir)
}

/// Every common location that contains a `temper.toml` (a git checkout, a synced
/// cloud folder, a mounted disk — the ways a folder tends to arrive), in a stable
/// order and de-duplicated. `setup` builds its picker from this; `find_home` uses
/// it to auto-resolve a lone match and refuse an ambiguous several.
pub fn scan_candidates() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
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
    let mut out = Vec::new();
    for root in roots {
        for name in names {
            let cand = root.join(name);
            if has_manifest(&cand) && !out.contains(&cand) {
                out.push(cand);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_round_trip() {
        // Pass the config base as a param — no `set_var("XDG_CONFIG_HOME")`,
        // which is process-global (racy under parallel tests) and `unsafe` in
        // Rust 2024. A temp base means this never touches the real config.
        let base = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        fs::write(home.path().join("temper.toml"), "").unwrap();
        let written = save_pointer_in(base.path(), home.path()).unwrap();
        assert!(written.exists());
        let read = saved_pointer_in(base.path()).unwrap();
        assert!(has_manifest(&read));
    }

    #[test]
    fn save_refuses_dir_without_manifest() {
        let empty = tempfile::TempDir::new().unwrap();
        assert!(save_pointer(empty.path()).is_err());
    }
}
