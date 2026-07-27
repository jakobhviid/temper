//! Parse and validate the manifest: `fleet.toml` (machines, per-machine app
//! composition) and `apps/<name>.toml` bundles (ordered steps). See ../../SPEC.md.
//!
//! Slice 1 supports the `copy` step; more fields land as primitives do.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct FleetToml {
    #[serde(default)]
    pub machine: Vec<Machine>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Machine {
    pub name: String,
    /// "mac" | "linux".
    pub os: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub apps: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Bundle {
    #[serde(default)]
    pub step: Vec<Step>,
}

/// One primitive step. Exactly one primitive field is set; slice 1 = `copy`.
#[derive(Debug, Deserialize, Clone)]
pub struct Step {
    #[serde(default)]
    pub copy: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    /// Skip this step unless the machine's OS matches ("mac" | "linux").
    #[serde(default)]
    pub os: Option<String>,
    /// File mode (e.g. "0600"). Stored now; enforced when `copy` grows perms.
    #[serde(default)]
    pub mode: Option<String>,
}

pub fn load_fleet(home: &Path) -> Result<FleetToml> {
    let p = home.join("fleet.toml");
    let s = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    toml::from_str(&s).with_context(|| format!("parsing {}", p.display()))
}

pub fn load_bundle(home: &Path, name: &str) -> Result<Bundle> {
    let p = home.join("apps").join(format!("{name}.toml"));
    let s = std::fs::read_to_string(&p)
        .with_context(|| format!("reading bundle {}", p.display()))?;
    toml::from_str(&s).with_context(|| format!("parsing {}", p.display()))
}

/// Expand a leading `~/` against `$HOME`. Everything else is returned as-is.
pub fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(p)
}
