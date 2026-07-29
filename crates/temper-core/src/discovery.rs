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
    scan_in(&home)
}

/// The scan over a given home root — split out so it's testable without touching
/// the process-global `$HOME` (which is racy under parallel tests).
fn scan_in(home: &Path) -> Vec<PathBuf> {
    let names = ["steel", "temper-home", ".temper"];
    // Parent dirs a checkout tends to live in. `Developer` is the macOS
    // convention; a Linux box just as often uses a lowercase `developer` or a
    // short `dev`/`src`/`code`/`projects`/`git`/`repos` — so detection doesn't
    // hinge on which name (or case) a given machine happened to pick. (This is
    // exactly what bit a lowercase `~/developer/steel` on some hosts.)
    let dev_parents = [
        "Developer",
        "developer",
        "dev",
        "src",
        "code",
        "Code",
        "projects",
        "Projects",
        "git",
        "repos",
    ];
    let mut roots = vec![home.to_path_buf()];
    roots.extend(dev_parents.iter().map(|d| home.join(d)));
    // Cloud-sync roots — the same set dotsync probes, so a folder that arrives
    // via any common sync client is found on both macOS and Linux:
    // macOS's unified `~/Library/CloudStorage/*` clients (OneDrive, GoogleDrive,
    // Dropbox, … each a subdir), iCloud Drive, and the classic home-root dirs.
    if let Ok(entries) = std::fs::read_dir(home.join("Library/CloudStorage")) {
        roots.extend(entries.flatten().map(|e| e.path()));
    }
    roots.push(home.join("Library/Mobile Documents/com~apple~CloudDocs")); // iCloud
    roots.extend(
        [
            "Nextcloud",
            "Dropbox",
            "OneDrive",
            "ProtonDrive",
            "Proton Drive",
            "Google Drive",
            "Sync",
        ]
        .iter()
        .map(|d| home.join(d)),
    );
    roots.extend([
        PathBuf::from("/media"),
        PathBuf::from("/run/media").join(std::env::var("USER").unwrap_or_default()),
    ]);
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

    #[test]
    fn scan_finds_lowercase_developer_and_other_dev_parents() {
        // The regression: a checkout under a lowercase `~/developer` (or `~/dev`,
        // `~/src`, …) must be found, not only the macOS-cased `~/Developer`.
        for parent in ["developer", "Developer", "dev", "src", "code", "projects"] {
            let home = tempfile::TempDir::new().unwrap();
            let steel = home.path().join(parent).join("steel");
            fs::create_dir_all(&steel).unwrap();
            fs::write(steel.join("temper.toml"), "").unwrap();
            let found = scan_in(home.path());
            assert!(
                found.contains(&steel),
                "scan missed {}: got {found:?}",
                steel.display()
            );
        }
    }

    #[test]
    fn scan_finds_cloud_sync_locations() {
        // A folder arriving via any common sync client must be found: a named
        // home-root client, iCloud Drive, and a macOS CloudStorage subdir.
        for rel in [
            "OneDrive/steel",
            "Proton Drive/steel",
            "Nextcloud/temper-home",
            "Library/Mobile Documents/com~apple~CloudDocs/steel",
            "Library/CloudStorage/OneDrive-Personal/steel",
        ] {
            let home = tempfile::TempDir::new().unwrap();
            let d = home.path().join(rel);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("temper.toml"), "").unwrap();
            let found = scan_in(home.path());
            assert!(found.contains(&d), "scan missed {rel}: got {found:?}");
        }
    }

    #[test]
    fn scan_finds_home_and_alt_repo_names() {
        let home = tempfile::TempDir::new().unwrap();
        // directly under ~ and under a named repo dir, both variants supported
        for rel in ["steel", "developer/temper-home", "dev/.temper"] {
            let d = home.path().join(rel);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("temper.toml"), "").unwrap();
        }
        let found = scan_in(home.path());
        assert_eq!(found.len(), 3, "expected all three, got {found:?}");
    }
}
