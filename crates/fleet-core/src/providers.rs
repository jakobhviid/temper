//! Package managers as (probe, converge) shell-outs. The pure set logic lives
//! in `packages`; this layer talks to the real tools.
//!
//! Guarded throughout: a manager is only probed/converged if its CLI is present
//! AND the effective set actually contains one of its packages — so on a
//! machine without brew, or with no declared packages, this is a clean no-op.
//! The real converge (`brew bundle`, `flatpak install`) is VM-verified; it is
//! never exercised by the sandboxed tests (which declare no packages).
//!
//! Not yet modeled here: `gext` (GNOME extensions) and `rpm-ostree` layered
//! rpms — they don't use Brewfile grammar and need their own manifest fields;
//! they land with the Linux/VM slice.

use std::collections::HashSet;
use std::process::Command;

use anyhow::{bail, Context, Result};

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
        let tmp = std::env::temp_dir().join(format!("fleet-Brewfile-{}", std::process::id()));
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
