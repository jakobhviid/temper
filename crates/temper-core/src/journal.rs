//! Content-addressed, after-hash-guarded undo — lifted from amdl's model.
//!
//! A mutating run writes `runs/<id>/manifest.json` + a content-addressed
//! `objects/` store under the state dir (`$TEMPER_STATE_DIR`, else the platform
//! state dir). Each entry is a minimal inverse:
//!   - `Create`  — temper created the file; undo deletes it if it still hashes
//!                 to what temper wrote.
//!   - `Restore` — temper overwrote an existing file; undo restores the prior
//!                 bytes if the file still hashes to what temper left.
//! Every revert is guarded by an after-hash check: if the file changed since,
//! the entry is skipped, never clobbered.
//!
//! Slice 1 reverts the newest run; run selection / listing / GC land later.

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
        if best.as_ref().map_or(true, |(t, _)| mtime > *t) {
            best = Some((mtime, p));
        }
    }
    best.map(|(_, p)| p).ok_or_else(|| anyhow!("nothing to undo"))
}

/// Revert the newest run. Returns (reverted, skipped) counts. `dry_run` reports
/// without touching anything.
pub fn undo(dry_run: bool) -> Result<(usize, usize)> {
    let root = state_root();
    let run_dir = newest_run(&root.join("runs"))?;
    let run: RunFile = serde_json::from_slice(&fs::read(run_dir.join("manifest.json"))?)?;

    let (mut reverted, mut skipped) = (0usize, 0usize);
    for entry in run.entries.iter().rev() {
        match entry {
            Entry::Create { path, hash: h } => {
                let p = PathBuf::from(path);
                if p.is_file() && &hash(&fs::read(&p)?) == h {
                    if !dry_run {
                        fs::remove_file(&p)?;
                    }
                    reverted += 1;
                } else {
                    skipped += 1;
                }
            }
            Entry::Restore { path, before, after } => {
                let p = PathBuf::from(path);
                if p.is_file() && &hash(&fs::read(&p)?) == after {
                    if !dry_run {
                        let bytes = fs::read(root.join("objects").join(before))?;
                        fs::write(&p, bytes)?;
                    }
                    reverted += 1;
                } else {
                    skipped += 1;
                }
            }
        }
    }
    if !dry_run {
        fs::remove_dir_all(&run_dir).ok();
    }
    Ok((reverted, skipped))
}
