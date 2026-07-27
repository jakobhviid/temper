//! Locate the temper-home folder — the directory holding `temper.toml`.
//!
//! Slice 1: `$TEMPER_DIR` → walk up from the current directory. The fuller
//! dotsync-style auto-scan of cloud folders + first-run prompt lands later.
//!
//! temper is delivery-agnostic: it never runs git or a sync client — it only
//! needs a path that contains a manifest.

use std::path::PathBuf;

use anyhow::{bail, Result};

pub fn find_home() -> Result<PathBuf> {
    if let Ok(d) = std::env::var("TEMPER_DIR") {
        let p = PathBuf::from(d);
        if p.join("temper.toml").is_file() {
            return Ok(p);
        }
        bail!("TEMPER_DIR={} has no temper.toml", p.display());
    }
    let mut dir = std::env::current_dir()?;
    loop {
        if dir.join("temper.toml").is_file() {
            return Ok(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    bail!("no temper.toml found (set TEMPER_DIR or run inside your temper folder)")
}
