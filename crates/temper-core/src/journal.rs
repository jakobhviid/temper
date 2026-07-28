//! Content-addressed, after-hash-guarded undo — lifted from amdl's model.
//!
//! A mutating run writes `runs/<id>/manifest.json` + a content-addressed
//! `objects/` store under the state dir (`$TEMPER_STATE_DIR`, else the platform
//! state dir). Each entry is a minimal inverse:
//!
//! - `Create` — temper created the file; undo deletes it if it still hashes to
//!   what temper wrote.
//! - `Restore` — temper overwrote an existing file; undo restores the prior
//!   bytes if the file still hashes to what temper left.
//! - `DconfKey` — a `setkey(dconf)` write; undo restores the prior value (or
//!   resets a previously-unset key), guarded on the live value.
//!
//! Every revert is guarded by an after check: if the target changed since, the
//! entry is skipped, never clobbered.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};

pub fn state_root() -> PathBuf {
    if let Ok(d) = std::env::var("TEMPER_STATE_DIR") {
        return PathBuf::from(d);
    }
    if cfg!(target_os = "macos") {
        if let Ok(h) = std::env::var("HOME") {
            return PathBuf::from(h).join("Library/Application Support/temper");
        }
    }
    if let Ok(d) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(d).join("temper");
    }
    if let Ok(h) = std::env::var("HOME") {
        return PathBuf::from(h).join(".local/state/temper");
    }
    PathBuf::from(".temper-state")
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op")]
enum Entry {
    Create { path: String, hash: String },
    Restore { path: String, before: String, after: String },
    /// A `setkey(dconf)` write. `before` = prior `dconf read` (None if the key
    /// was unset), `after` = the value temper wrote (the revert guard). Undo
    /// re-writes `before`, or resets the key when it was previously unset.
    DconfKey {
        key: String,
        before: Option<String>,
        after: String,
    },
}

fn dconf_read(key: &str) -> Option<String> {
    let out = std::process::Command::new("dconf").args(["read", key]).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}

fn dconf_write(key: &str, value: &str) -> bool {
    std::process::Command::new("dconf")
        .args(["write", key, value])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn dconf_reset(key: &str) -> bool {
    std::process::Command::new("dconf")
        .args(["reset", key])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[derive(Serialize, Deserialize)]
struct RunFile {
    argv: Vec<String>,
    entries: Vec<Entry>,
}

pub struct Journal {
    root: PathBuf,
    id: String,
    entries: Vec<Entry>,
}

fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

impl Journal {
    pub fn begin() -> Journal {
        let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        Journal {
            root: state_root(),
            id: format!("{}-{:09}", d.as_secs(), d.subsec_nanos()),
            entries: Vec::new(),
        }
    }

    /// Record that `path` is being written: `before` = its prior bytes (None if
    /// it didn't exist), `after` = the bytes temper is writing.
    pub fn record_write(&mut self, path: &Path, before: Option<&[u8]>, after: &[u8]) -> Result<()> {
        let path = path.to_string_lossy().into_owned();
        match before {
            None => self.entries.push(Entry::Create {
                path,
                hash: hash(after),
            }),
            Some(bytes) => {
                let before = self.store_object(bytes)?;
                self.entries.push(Entry::Restore {
                    path,
                    before,
                    after: hash(after),
                });
            }
        }
        Ok(())
    }

    /// Record a `setkey(dconf)` write for undo (`before` = prior value or None).
    pub fn record_dconf(&mut self, key: &str, before: Option<String>, after: String) {
        self.entries.push(Entry::DconfKey {
            key: key.to_string(),
            before,
            after,
        });
    }

    fn store_object(&self, bytes: &[u8]) -> Result<String> {
        let h = hash(bytes);
        let dir = self.root.join("objects");
        fs::create_dir_all(&dir)?;
        let p = dir.join(&h);
        if !p.exists() {
            fs::write(&p, bytes)?;
        }
        Ok(h)
    }

    /// Write the manifest atomically (presence = committed). No-op if empty.
    pub fn commit(self) -> Result<()> {
        if self.entries.is_empty() {
            return Ok(());
        }
        let dir = self.root.join("runs").join(&self.id);
        fs::create_dir_all(&dir)?;
        let run = RunFile {
            argv: std::env::args().collect(),
            entries: self.entries,
        };
        let tmp = dir.join("manifest.json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(&run)?)?;
        fs::rename(&tmp, dir.join("manifest.json"))?;
        Ok(())
    }
}

fn newest_run(runs: &Path) -> Result<PathBuf> {
    if !runs.is_dir() {
        bail!("nothing to undo");
    }
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(runs)? {
        let entry = entry?;
        let p = entry.path();
        if !p.join("manifest.json").is_file() {
            continue;
        }
        let mtime = entry.metadata()?.modified()?;
        if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
            best = Some((mtime, p));
        }
    }
    best.map(|(_, p)| p).ok_or_else(|| anyhow!("nothing to undo"))
}

/// Revertible run ids, newest first.
pub fn list_runs() -> Result<Vec<String>> {
    let runs = state_root().join("runs");
    if !runs.is_dir() {
        return Ok(Vec::new());
    }
    let mut v: Vec<(SystemTime, String)> = Vec::new();
    for entry in fs::read_dir(&runs)? {
        let entry = entry?;
        if !entry.path().join("manifest.json").is_file() {
            continue;
        }
        v.push((
            entry.metadata()?.modified()?,
            entry.file_name().to_string_lossy().into_owned(),
        ));
    }
    v.sort_by_key(|b| std::cmp::Reverse(b.0));
    Ok(v.into_iter().map(|(_, id)| id).collect())
}

/// Revert a run — the one named by `run` (its id), else the newest. `dry_run`
/// reports without touching anything. Returns (reverted, skipped). A guard
/// check (does the file still hash to what temper left?) and a missing content
/// object both cause that entry to be *skipped and reported*, never clobbered
/// or aborted mid-run.
pub fn undo(run: Option<&str>, dry_run: bool) -> Result<(usize, usize)> {
    let root = state_root();
    let runs = root.join("runs");
    let run_dir = match run {
        Some(id) => {
            let d = runs.join(id);
            if !d.join("manifest.json").is_file() {
                bail!("no revertible run '{id}' (see `temper undo --list`)");
            }
            d
        }
        None => newest_run(&runs)?,
    };
    let rf: RunFile = serde_json::from_slice(&fs::read(run_dir.join("manifest.json"))?)?;

    let (mut reverted, mut skipped) = (0usize, 0usize);
    for entry in rf.entries.iter().rev() {
        // dconf key entries guard on the live value, not a file hash.
        if let Entry::DconfKey { key, before, after } = entry {
            if dconf_read(key).as_deref() != Some(after.as_str()) {
                skipped += 1; // changed since temper wrote it → don't clobber
                continue;
            }
            if dry_run {
                reverted += 1;
                continue;
            }
            let done = match before {
                Some(v) => dconf_write(key, v),
                None => dconf_reset(key),
            };
            if done {
                reverted += 1;
            } else {
                skipped += 1;
            }
            continue;
        }

        let (path, expect_after) = match entry {
            Entry::Create { path, hash } => (path, hash),
            Entry::Restore { path, after, .. } => (path, after),
            Entry::DconfKey { .. } => unreachable!(),
        };
        let p = PathBuf::from(path);
        let current = if p.is_file() { fs::read(&p).ok() } else { None };
        // Only revert if the file still hashes to what temper left it as.
        if !current.as_deref().is_some_and(|b| hash(b).as_str() == expect_after.as_str()) {
            skipped += 1;
            continue;
        }
        if dry_run {
            reverted += 1;
            continue;
        }
        let done = match entry {
            Entry::Create { .. } => fs::remove_file(&p).is_ok(),
            Entry::Restore { before, .. } => {
                // A missing object is a skip, not a fatal abort mid-run.
                match fs::read(root.join("objects").join(before)) {
                    Ok(bytes) => fs::write(&p, bytes).is_ok(),
                    Err(_) => false,
                }
            }
            Entry::DconfKey { .. } => unreachable!(),
        };
        if done {
            reverted += 1;
        } else {
            skipped += 1;
        }
    }
    // Keep the run dir if anything was skipped, so it can be inspected/retried.
    if !dry_run && skipped == 0 {
        fs::remove_dir_all(&run_dir).ok();
    }
    Ok((reverted, skipped))
}
