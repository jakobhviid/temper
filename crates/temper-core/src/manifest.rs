//! Parse and validate the manifest: `temper.toml` (machines, template vars) and
//! `apps/<name>.toml` bundles (ordered steps + drift-only assertions). See
//! ../../SPEC.md.
//!
//! Step primitives: `copy` (verbatim/template/seed/mode), `block`, `setkey`,
//! `exec`, `profile`. `[[assert]]` covers drift-only checks.

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
    /// brew-specific settings.
    #[serde(default)]
    pub brew: BrewConfig,
}

/// `[brew]` settings.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrewConfig {
    /// Third-party taps to `brew trust` before any converge/upgrade (Homebrew
    /// 5.2+ gates untrusted taps, silently skipping their formulae otherwise).
    #[serde(default)]
    pub trust: Vec<String>,
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
#[serde(deny_unknown_fields)]
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
    /// A Brewfile (relative to the temper-home) whose lines are added to this
    /// machine's package set — the clean way to migrate an existing Brewfile.
    #[serde(default)]
    pub brewfile: Option<String>,
    /// Per-machine template vars, merged OVER the global `[vars]` (so a Linux
    /// box can override a Mac-valued `BREW_PREFIX`). Referenced as
    /// `{{ var "NAME" }}` like the globals.
    #[serde(default)]
    pub vars: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    /// Packages this bundle needs (Brewfile line grammar), aggregated into the
    /// machine's effective set. `_mac`/`_linux` variants are OS-scoped.
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub packages_mac: Vec<String>,
    #[serde(default)]
    pub packages_linux: Vec<String>,
    /// GNOME extension UUIDs to install via `gext` (Linux desktop).
    #[serde(default)]
    pub extensions: Vec<String>,
    /// rpm-ostree layered packages (Linux; can't be image-baked).
    #[serde(default)]
    pub rpm: Vec<String>,
    #[serde(default)]
    pub step: Vec<Step>,
    /// Drift-only assertions (no converge action).
    #[serde(default)]
    pub assert: Vec<Assert>,
}

/// One primitive step. Exactly one primitive is set.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
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

    // --- profile: install a macOS .mobileconfig (manual approval) ---
    #[serde(default)]
    pub profile: Option<String>,

    // --- exec: run a user script (the escape hatch) ---
    #[serde(default)]
    pub exec: Option<String>,
    /// Companion drift-hook: exit 0 = in sync. Also gates whether `exec` re-runs.
    #[serde(default)]
    pub check: Option<String>,
    /// Deprecated no-op: temper always runs exec as the user (chezmoi model);
    /// escalate inside the script with `sudo <cmd>` for specific ops. Kept so
    /// existing manifests parse.
    #[serde(default)]
    pub sudo: bool,
    /// Env var names that must be present and are passed through to the script.
    #[serde(default)]
    pub secrets: Vec<String>,

    /// Lifecycle: "always" (re-apply every update) | "install" (once) |
    /// "ensure" (install-if-missing on update; never overwrites a present
    /// target) | "manual" (only when explicitly invoked). Default by primitive:
    /// copy/setkey/block → always; exec/seed → install.
    #[serde(default)]
    pub run: Option<String>,

    /// Skip this step unless the machine's OS matches ("mac" | "linux").
    #[serde(default)]
    pub os: Option<String>,
    /// Skip this step unless the machine's role matches ("desktop" | "server").
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct SetKey {
    /// json | toml | ini | defaults (macOS) | dconf (Linux).
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
#[serde(deny_unknown_fields)]
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
    /// The current user must NOT belong to this group.
    #[serde(default)]
    pub not_member: Option<GroupCheck>,
    /// The current user's login shell must equal this path.
    #[serde(default)]
    pub shell: Option<String>,
    /// A deployed json file must be semantically equal to a reference.
    #[serde(default)]
    pub json_semantic: Option<JsonSemantic>,
    #[serde(default)]
    pub os: Option<String>,
    /// Skip this assertion unless the machine's role matches.
    #[serde(default)]
    pub role: Option<String>,
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

#[derive(Debug, Deserialize, Clone)]
pub struct GroupCheck {
    pub group: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct JsonSemantic {
    /// The deployed file to check (on the machine).
    pub file: String,
    /// The reference file (relative to the temper-home folder).
    pub against: String,
}

pub fn load_fleet(home: &Path) -> Result<TemperToml> {
    let p = home.join("temper.toml");
    let s = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    let ft: TemperToml =
        toml::from_str(&s).with_context(|| format!("parsing {}", p.display()))?;
    // Reject duplicate machine names — otherwise the second silently shadows.
    let mut seen = std::collections::HashSet::new();
    for m in &ft.machine {
        if !seen.insert(m.name.to_lowercase()) {
            anyhow::bail!("duplicate machine name '{}' in temper.toml", m.name);
        }
    }
    Ok(ft)
}

pub fn load_bundle(home: &Path, name: &str) -> Result<Bundle> {
    let p = home.join("apps").join(format!("{name}.toml"));
    let s = std::fs::read_to_string(&p)
        .with_context(|| format!("reading bundle {}", p.display()))?;
    toml::from_str(&s).with_context(|| format!("parsing {}", p.display()))
}

/// A machine's effective template vars: the global `[vars]` overlaid by the
/// machine's own `vars` (per-machine wins). Lets a Linux box override a
/// Mac-valued `BREW_PREFIX` without a second global table.
pub fn effective_vars(
    global: &std::collections::BTreeMap<String, String>,
    machine: &Machine,
) -> std::collections::BTreeMap<String, String> {
    let mut merged = global.clone();
    for (k, v) in &machine.vars {
        merged.insert(k.clone(), v.clone());
    }
    merged
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn machine_with_vars(vars: &[(&str, &str)]) -> Machine {
        Machine {
            name: "kira".into(),
            os: "linux".into(),
            role: None,
            apps: vec![],
            packages: vec![],
            brewfile: None,
            vars: vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    #[test]
    fn per_machine_vars_override_global() {
        let mut global = BTreeMap::new();
        global.insert("BREW_PREFIX".to_string(), "/opt/homebrew".to_string());
        global.insert("SHARED".to_string(), "keep".to_string());
        let m = machine_with_vars(&[("BREW_PREFIX", "/home/linuxbrew/.linuxbrew")]);
        let eff = effective_vars(&global, &m);
        assert_eq!(eff["BREW_PREFIX"], "/home/linuxbrew/.linuxbrew"); // machine wins
        assert_eq!(eff["SHARED"], "keep"); // global survives
    }

    #[test]
    fn no_machine_vars_leaves_global_untouched() {
        let mut global = BTreeMap::new();
        global.insert("A".to_string(), "1".to_string());
        let m = machine_with_vars(&[]);
        assert_eq!(effective_vars(&global, &m), global);
    }
}
