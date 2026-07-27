//! Parse and validate the manifest: `temper.toml` (machines, template vars) and
//! `apps/<name>.toml` bundles (ordered steps + drift-only assertions). See
//! ../../SPEC.md.
//!
//! Live step primitives: `copy` (verbatim/template/seed/mode), `block`,
//! `setkey` (json backend). `[[assert]]` covers drift-only checks.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct TemperToml {
    #[serde(default)]
    pub machine: Vec<Machine>,
    /// Declared template variables, referenced as `{{ var "NAME" }}`.
    #[serde(default)]
    pub vars: std::collections::BTreeMap<String, String>,
    /// Packages installed but not declared that drift/prune must NOT flag as
    /// extras (OS-preinstalled baseline, e.g. Bazzite's default flatpaks).
    #[serde(default)]
    pub ignore: Ignore,
}

/// Per-manager ignore lists (by the same short name drift matches on).
#[derive(Debug, Default, Deserialize)]
pub struct Ignore {
    #[serde(default)]
    pub brew: Vec<String>,
    #[serde(default)]
    pub cask: Vec<String>,
    #[serde(default)]
    pub flatpak: Vec<String>,
    #[serde(default)]
    pub mas: Vec<String>,
    #[serde(default)]
    pub vscode: Vec<String>,
    #[serde(default)]
    pub tap: Vec<String>,
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
    /// Per-machine loose packages that belong to no app-bundle.
    #[serde(default)]
    pub packages: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Bundle {
    /// Packages this bundle needs (Brewfile line grammar), aggregated into the
    /// machine's effective set. `_mac`/`_linux` variants are OS-scoped.
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub packages_mac: Vec<String>,
    #[serde(default)]
    pub packages_linux: Vec<String>,
    #[serde(default)]
    pub step: Vec<Step>,
    /// Drift-only assertions (no converge action).
    #[serde(default)]
    pub assert: Vec<Assert>,
}

/// One primitive step. Exactly one primitive is set.
#[derive(Debug, Deserialize, Clone)]
pub struct Step {
    // --- copy ---
    #[serde(default)]
    pub copy: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    /// `copy`: substitute `{{ … }}` before deploy.
    #[serde(default)]
    pub template: bool,
    /// `copy`: create-once if absent, then hands-off; excluded from drift.
    #[serde(default)]
    pub seed: bool,
    /// `copy`: octal file mode enforced on the target (e.g. "0600").
    #[serde(default)]
    pub mode: Option<String>,

    // --- block: ensure a marker-delimited region is present in a user file ---
    #[serde(default)]
    pub block: Option<String>,
    #[serde(default, rename = "in")]
    pub in_file: Option<String>,
    #[serde(default)]
    pub marker: Option<String>,

    // --- setkey: set a key in a structured file, preserving siblings ---
    #[serde(default)]
    pub setkey: Option<SetKey>,

    // --- exec: run a user script (the escape hatch) ---
    #[serde(default)]
    pub exec: Option<String>,
    /// Companion drift-hook: exit 0 = in sync. Also gates whether `exec` re-runs.
    #[serde(default)]
    pub check: Option<String>,
    /// Run the exec/check under `sudo`.
    #[serde(default)]
    pub sudo: bool,
    /// Env var names that must be present and are passed through to the script.
    #[serde(default)]
    pub secrets: Vec<String>,

    /// Lifecycle: "always" | "install" | "ensure" | "manual". Defaults by
    /// primitive when unset (copy/setkey/block → always; exec → install).
    #[serde(default)]
    pub run: Option<String>,

    /// Skip this step unless the machine's OS matches ("mac" | "linux").
    #[serde(default)]
    pub os: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SetKey {
    /// "json" (live) | "toml"/"ini"/"dconf"/"defaults" (later).
    pub backend: String,
    /// Target file for file backends.
    #[serde(default)]
    pub file: Option<String>,
    /// Dotted key path.
    pub key: String,
    /// Scalar (or array) value to set.
    pub value: toml::Value,
    /// List-union append into an array-valued key.
    #[serde(default)]
    pub append: bool,
}

/// A drift-only assertion. Exactly one check field is set.
#[derive(Debug, Deserialize, Clone)]
pub struct Assert {
    /// Path that must NOT exist.
    #[serde(default)]
    pub absent: Option<String>,
    /// A file that must contain a given line.
    #[serde(default)]
    pub contains_line: Option<ContainsLine>,
    /// A path that must have a given octal mode.
    #[serde(default)]
    pub mode: Option<ModeCheck>,
    /// A command that must resolve on PATH.
    #[serde(default)]
    pub executable_resolves: Option<String>,
    #[serde(default)]
    pub os: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ContainsLine {
    pub file: String,
    pub line: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ModeCheck {
    pub path: String,
    pub mode: String,
}

pub fn load_fleet(home: &Path) -> Result<TemperToml> {
    let p = home.join("temper.toml");
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
