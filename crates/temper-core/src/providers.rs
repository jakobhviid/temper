//! Package managers as (probe, converge) shell-outs. The pure set logic lives
//! in `packages`; this layer talks to the real tools.
//!
//! Guarded throughout: a manager is only probed/converged if its CLI is present
//! AND the effective set actually contains one of its packages — so on a
//! machine without brew, or with no declared packages, this is a clean no-op.
//! The real converge (`brew bundle`, `flatpak install`) is VM-verified; it is
//! never exercised by the sandboxed tests (which declare no packages).
//!
//! `gext` (GNOME extensions) and `rpm-ostree` (layered rpms) are modeled at the
//! bottom — they don't use Brewfile grammar, so they have their own bundle
//! fields (`extensions`, `rpm`). Both are Linux/VM-only and guarded on their CLI.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::manifest::{self, Machine};
use crate::packages::{Installed, Manager, Pkg};
use crate::primitives::which;

fn have(cmd: &str) -> bool {
    which(cmd).is_some()
}

/// Run a command and return its non-empty output lines (trimmed). A non-zero
/// exit yields an empty list rather than an error — probing is best-effort.
fn run_lines(cmd: &str, args: &[&str]) -> Result<Vec<String>> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("running {cmd} {args:?}"))?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// Snapshot what's installed, but only for managers that (a) appear in the
/// effective set and (b) have their CLI present. Absent managers stay unprobed
/// (so nothing is reported as missing/extra for them).
pub fn probe(effective: &[Pkg]) -> Result<Installed> {
    let managers: HashSet<Manager> = effective.iter().map(|p| p.manager).collect();
    let mut inst = Installed::default();

    if have("brew") {
        if managers.contains(&Manager::Brew) {
            inst.set(Manager::Brew, run_lines("brew", &["list", "--formula"])?);
        }
        if managers.contains(&Manager::Cask) {
            inst.set(Manager::Cask, run_lines("brew", &["list", "--cask"])?);
        }
        if managers.contains(&Manager::Tap) {
            inst.set(Manager::Tap, run_lines("brew", &["tap"])?);
        }
    }
    if managers.contains(&Manager::Flatpak) && have("flatpak") {
        inst.set(
            Manager::Flatpak,
            run_lines("flatpak", &["list", "--app", "--columns=application"])?,
        );
    }
    if managers.contains(&Manager::Mas) && have("mas") {
        // `mas list` rows look like: "497799835  Xcode (14.0)" → first token.
        let ids = run_lines("mas", &["list"])?
            .into_iter()
            .filter_map(|l| l.split_whitespace().next().map(String::from));
        inst.set(Manager::Mas, ids.collect::<Vec<_>>());
    }
    if managers.contains(&Manager::Vscode) && have("code") {
        let exts = run_lines("code", &["--list-extensions"])?
            .into_iter()
            .map(|s| s.to_lowercase());
        inst.set(Manager::Vscode, exts.collect::<Vec<_>>());
    }
    Ok(inst)
}

/// Converge the effective set (install-missing; never removes). brew-family
/// packages go through one materialized Brewfile + `brew bundle`; flatpaks are
/// installed by id. `dry_run` performs no mutation. Returns the number of
/// declared packages considered.
pub fn converge(effective: &[Pkg], dry_run: bool) -> Result<usize> {
    let brewish: Vec<&Pkg> = effective
        .iter()
        .filter(|p| {
            matches!(
                p.manager,
                Manager::Brew | Manager::Cask | Manager::Tap | Manager::Mas | Manager::Vscode
            )
        })
        .collect();

    if !brewish.is_empty() && have("brew") && !dry_run {
        let body: String = brewish.iter().map(|p| format!("{}\n", p.raw)).collect();
        let tmp = std::env::temp_dir().join(format!("temper-Brewfile-{}", std::process::id()));
        std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
        let status = Command::new("brew")
            .args(["bundle", "--file"])
            .arg(&tmp)
            .status()
            .context("running brew bundle")?;
        let _ = std::fs::remove_file(&tmp);
        if !status.success() {
            bail!("brew bundle failed");
        }
    }

    let flatpaks: Vec<&str> = effective
        .iter()
        .filter(|p| p.manager == Manager::Flatpak)
        .map(|p| p.name.as_str())
        .collect();
    if !flatpaks.is_empty() && have("flatpak") && !dry_run {
        let mut cmd = Command::new("flatpak");
        cmd.args(["install", "-y", "--noninteractive"]);
        for f in &flatpaks {
            cmd.arg(f);
        }
        // best-effort: a missing remote or app shouldn't abort the whole run
        let _ = cmd.status();
    }

    Ok(effective.len())
}

/// Upgrade installed packages (brew + flatpak). Best-effort; VM-verified. The
/// caller only invokes this when packages are actually declared, so a machine
/// with an empty set never triggers a global upgrade.
pub fn upgrade() -> Result<()> {
    if have("brew") {
        let _ = Command::new("brew").arg("upgrade").status();
    }
    if have("flatpak") {
        let _ = Command::new("flatpak")
            .args(["update", "-y", "--noninteractive"])
            .status();
    }
    Ok(())
}

/// Remove installed-but-not-declared packages. brew-family goes through
/// dependency-aware `brew bundle cleanup --force` against the effective
/// Brewfile (so a kept package's transitive deps aren't removed); flatpak
/// extras are uninstalled by id. VM-verified.
pub fn prune_apply(effective: &[Pkg], extras: &[(Manager, String)]) -> Result<()> {
    let has_brewish = effective.iter().any(|p| {
        matches!(
            p.manager,
            Manager::Brew | Manager::Cask | Manager::Tap | Manager::Mas | Manager::Vscode
        )
    });
    if has_brewish && have("brew") {
        let body: String = effective
            .iter()
            .filter(|p| {
                matches!(
                    p.manager,
                    Manager::Brew | Manager::Cask | Manager::Tap | Manager::Mas | Manager::Vscode
                )
            })
            .map(|p| format!("{}\n", p.raw))
            .collect();
        let tmp = std::env::temp_dir().join(format!("temper-Brewfile-prune-{}", std::process::id()));
        fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
        let status = Command::new("brew")
            .args(["bundle", "cleanup", "--force", "--file"])
            .arg(&tmp)
            .status()
            .context("running brew bundle cleanup")?;
        let _ = fs::remove_file(&tmp);
        if !status.success() {
            bail!("brew bundle cleanup failed");
        }
    }

    let flatpaks: Vec<&str> = extras
        .iter()
        .filter(|(m, _)| *m == Manager::Flatpak)
        .map(|(_, n)| n.as_str())
        .collect();
    if !flatpaks.is_empty() && have("flatpak") {
        let mut cmd = Command::new("flatpak");
        cmd.args(["uninstall", "-y", "--noninteractive"]);
        for f in &flatpaks {
            cmd.arg(f);
        }
        let _ = cmd.status();
    }
    Ok(())
}

/// Dump live package state into the folder at `machines/<name>/Brewfile` via
/// `brew bundle dump`. Returns the written path. VM-verified.
pub fn dump(home: &Path, machine: &str) -> Result<PathBuf> {
    if !have("brew") {
        bail!("brew not found — cannot dump package state");
    }
    let dir = home.join("machines").join(machine);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let bf = dir.join("Brewfile");
    let status = Command::new("brew")
        .args(["bundle", "dump", "--force", "--no-vscode", "--file"])
        .arg(&bf)
        .status()
        .context("running brew bundle dump")?;
    if !status.success() {
        bail!("brew bundle dump failed");
    }
    Ok(bf)
}

// --- gext: GNOME extensions (Linux desktop) -----------------------------------

/// Union of a machine's composed apps' `extensions`, de-duplicated.
pub fn effective_extensions(home: &Path, machine: &Machine) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for app in &machine.apps {
        for uuid in manifest::load_bundle(home, app)?.extensions {
            if seen.insert(uuid.clone()) {
                out.push(uuid);
            }
        }
    }
    Ok(out)
}

fn gext_installed() -> Vec<String> {
    // `gnome-extensions list` prints one UUID per line.
    if have("gnome-extensions") {
        run_lines("gnome-extensions", &["list"]).unwrap_or_default()
    } else {
        Vec::new()
    }
}

/// Declared extensions not installed. Empty (no-op) where GNOME isn't present.
pub fn gext_missing(effective: &[String]) -> Vec<String> {
    if effective.is_empty() || (!have("gext") && !have("gnome-extensions")) {
        return Vec::new();
    }
    let installed = gext_installed();
    effective
        .iter()
        .filter(|e| !installed.contains(e))
        .cloned()
        .collect()
}

/// Install missing extensions via `gext`. VM-verified.
pub fn gext_converge(effective: &[String], dry_run: bool) -> Result<()> {
    if dry_run || !have("gext") {
        return Ok(());
    }
    for uuid in gext_missing(effective) {
        let _ = Command::new("gext").args(["install", &uuid]).status();
    }
    Ok(())
}

// --- rpm-ostree: layered rpms that can't be image-baked (Linux) ---------------

pub fn effective_rpm(home: &Path, machine: &Machine) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for app in &machine.apps {
        for pkg in manifest::load_bundle(home, app)?.rpm {
            if seen.insert(pkg.clone()) {
                out.push(pkg);
            }
        }
    }
    Ok(out)
}

/// Declared rpms not installed (`rpm -q`). Empty where rpm isn't present.
pub fn rpm_missing(effective: &[String]) -> Vec<String> {
    if effective.is_empty() || !have("rpm") {
        return Vec::new();
    }
    effective
        .iter()
        .filter(|p| {
            !Command::new("rpm")
                .args(["-q", p])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

/// Layer missing rpms via `rpm-ostree install --idempotent`. Returns whether a
/// reboot is needed. VM-verified.
pub fn rpm_converge(effective: &[String], dry_run: bool) -> Result<bool> {
    let missing = rpm_missing(effective);
    if dry_run || missing.is_empty() || !have("rpm-ostree") {
        return Ok(false);
    }
    let mut cmd = Command::new("rpm-ostree");
    cmd.args(["install", "--idempotent"]);
    for p in &missing {
        cmd.arg(p);
    }
    let _ = cmd.status();
    Ok(true) // layered rpms require a reboot to take effect
}

// --- dependency-aware brew extras (read-only) ---------------------------------

/// Formulae/casks/taps installed but not needed by the declared set, per
/// `brew bundle cleanup` (no `--force`, so read-only). Dependency-aware: a kept
/// package's transitive deps are NOT reported — unlike a naive set-diff. Names
/// are returned as brew's short names; the machine's `[ignore]` is applied.
pub fn brew_extras(effective: &[Pkg], ignore: &manifest::Ignore) -> Result<Vec<String>> {
    if !have("brew") {
        return Ok(Vec::new());
    }
    let body: String = effective
        .iter()
        .filter(|p| {
            matches!(
                p.manager,
                Manager::Brew | Manager::Cask | Manager::Tap | Manager::Mas | Manager::Vscode
            )
        })
        .map(|p| format!("{}\n", p.raw))
        .collect();
    if body.is_empty() {
        return Ok(Vec::new());
    }
    let tmp = std::env::temp_dir().join(format!("temper-Brewfile-drift-{}", std::process::id()));
    fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    let out = Command::new("brew")
        .args(["bundle", "cleanup", "--formula", "--cask", "--tap", "--file"])
        .arg(&tmp)
        .output()
        .context("running brew bundle cleanup")?;
    let _ = fs::remove_file(&tmp);

    // Parse the "Would uninstall …" / "Would untap …" sections for bare names.
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let ignored: HashSet<&str> = ignore
        .brew
        .iter()
        .chain(&ignore.cask)
        .chain(&ignore.tap)
        .map(String::as_str)
        .collect();

    let mut in_section = false;
    let mut extras = Vec::new();
    for line in text.lines() {
        if line.starts_with("Would uninstall") || line.starts_with("Would untap") {
            in_section = true;
            continue;
        }
        if !in_section {
            continue;
        }
        let first = line.chars().next();
        let is_name = matches!(first, Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
            && line
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '/' | '-'));
        if is_name {
            let name = line.rsplit('/').next().unwrap_or(line);
            if !ignored.contains(name) && !ignored.contains(line) {
                extras.push(name.to_string());
            }
        } else if matches!(first, Some(c) if c.is_ascii_uppercase()) {
            in_section = false;
        }
    }
    Ok(extras)
}
