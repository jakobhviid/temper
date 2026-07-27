//! Locate the fleet-home folder — the directory holding `fleet.toml`.
//!
//! Slice 1: `$FLEET_DIR` → walk up from the current directory. The fuller
//! dotsync-style auto-scan of cloud folders + first-run prompt lands later.
//!
//! fleet is delivery-agnostic: it never runs git or a sync client — it only
//! needs a path that contains a manifest.

use std::path::PathBuf;

use anyhow::{bail, Result};

pub fn find_home() -> Result<PathBuf> {
    if let Ok(d) = std::env::var("FLEET_DIR") {
        let p = PathBuf::from(d);
        if p.join("fleet.toml").is_file() {
            return Ok(p);
        }
        bail!("FLEET_DIR={} has no fleet.toml", p.display());
    }
    let mut dir = std::env::current_dir()?;
    loop {
        if dir.join("fleet.toml").is_file() {
            return Ok(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    bail!("no fleet.toml found (set FLEET_DIR or run inside your fleet folder)")
}
