//! `eq-import` — pull calibrated speaker profiles from a public repo into the
//! folder (RIS's `eq-import`). This is **folder-authoring** (it writes into the
//! config folder, not a machine), the one deliberately-labelled exception to
//! Principle #9 — its result feeds the `speaker-eq` step.
//!
//! Shallow-clones the repo to a temp dir, copies each `<x>.calibrated.conf` to
//! `<dest>/<x>.conf`, and cleans up. The clone/copy logic shells out to `git`;
//! the pure name/scan helpers are unit-tested.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::manifest::EqImport;
use crate::primitives::which;

/// `<x>.calibrated.conf` → `<x>.conf`; `None` for any other name.
pub fn dest_name(filename: &str) -> Option<String> {
    filename
        .strip_suffix(".calibrated.conf")
        .map(|base| format!("{base}.conf"))
}

/// Recursively collect files whose name ends in `.calibrated.conf`.
pub fn find_calibrated(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(find_calibrated(&p));
            } else if p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".calibrated.conf"))
            {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Shallow-clone `cfg.repo`, copy each calibrated profile into `home/<dest>`,
/// and return the written destination paths.
pub fn run(home: &Path, cfg: &EqImport) -> Result<Vec<PathBuf>> {
    if which("git").is_none() {
        bail!("eq-import needs `git` on PATH");
    }
    let tmp = std::env::temp_dir().join(format!("temper-eq-import-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let status = Command::new("git")
        .args(["clone", "--depth", "1", &cfg.repo])
        .arg(&tmp)
        .status()
        .context("running git clone")?;
    if !status.success() {
        bail!("git clone {} failed", cfg.repo);
    }

    let dest_dir = home.join(&cfg.dest);
    fs::create_dir_all(&dest_dir).with_context(|| format!("creating {}", dest_dir.display()))?;

    let mut written = Vec::new();
    for src in find_calibrated(&tmp) {
        let fname = src.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if let Some(dest_name) = dest_name(fname) {
            let dest = dest_dir.join(&dest_name);
            fs::copy(&src, &dest)
                .with_context(|| format!("copying {} → {}", src.display(), dest.display()))?;
            written.push(dest);
        }
    }
    let _ = fs::remove_dir_all(&tmp);
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibrated_rename() {
        assert_eq!(dest_name("living-room.calibrated.conf").as_deref(), Some("living-room.conf"));
        assert_eq!(dest_name("readme.md"), None);
        assert_eq!(dest_name("plain.conf"), None);
    }

    #[test]
    fn finds_calibrated_recursively() {
        let dir = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("a.calibrated.conf"), "x").unwrap();
        fs::write(dir.path().join("sub/b.calibrated.conf"), "y").unwrap();
        fs::write(dir.path().join("sub/note.txt"), "z").unwrap();
        let found = find_calibrated(dir.path());
        assert_eq!(found.len(), 2, "found {found:?}");
    }
}
