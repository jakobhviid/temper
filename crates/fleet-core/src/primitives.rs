//! The closed set of config primitives (app-scope). Each will implement the
//! shared plan → apply → drift → undo contract. Slice 1 = `copy` (verbatim);
//! template/seed/mode, `block`, `setkey`, `profile`, and `exec` land next.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::journal::Journal;

/// The sync state of a deployed file relative to its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    /// Target absent.
    Missing,
    /// Target matches source.
    InSync,
    /// Target present but differs.
    Drifted,
}

impl FileState {
    pub fn label(self) -> &'static str {
        match self {
            FileState::Missing => "missing",
            FileState::InSync => "in sync",
            FileState::Drifted => "drifted",
        }
    }

    pub fn is_ok(self) -> bool {
        matches!(self, FileState::InSync)
    }
}

/// `copy` (verbatim) drift: compare source bytes against the target.
pub fn copy_state(src: &Path, target: &Path) -> Result<FileState> {
    let want = fs::read(src).with_context(|| format!("reading source {}", src.display()))?;
    if !target.exists() {
        return Ok(FileState::Missing);
    }
    let have = fs::read(target).with_context(|| format!("reading {}", target.display()))?;
    Ok(if have == want {
        FileState::InSync
    } else {
        FileState::Drifted
    })
}

/// `copy` (verbatim) apply: deploy source → target if it differs, recording the
/// inverse in the journal. Returns whether it changed the file.
pub fn copy_apply(src: &Path, target: &Path, journal: &mut Journal) -> Result<bool> {
    let want = fs::read(src).with_context(|| format!("reading source {}", src.display()))?;
    let before = if target.exists() {
        Some(fs::read(target).with_context(|| format!("reading {}", target.display()))?)
    } else {
        None
    };
    if before.as_deref() == Some(want.as_slice()) {
        return Ok(false);
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    journal.record_write(target, before.as_deref(), &want)?;
    fs::write(target, &want).with_context(|| format!("writing {}", target.display()))?;
    Ok(true)
}
