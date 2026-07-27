//! Build the ordered steps + assertions for a machine, then evaluate (drift) or
//! apply (install) them. Live step primitives: `copy`, `block`, `setkey(json)`,
//! `exec`. Assertions are drift-only.
//!
//! Flows: `install` applies every step (with a `dry_run` preview); `update` /
//! `ensure` / `adopt` land later.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Result};

use crate::journal::Journal;
use crate::manifest::{self, expand_tilde, Assert, Ignore, Machine, Step};
use crate::primitives::{self, CopyOpts, ExecOpts, FileState};
use crate::{drift, packages, providers};

fn os_gated(step_os: &Option<String>, machine: &Machine) -> bool {
    match step_os {
        Some(os) => os != &machine.os,
        None => false,
    }
}

/// Skip a step whose declared role doesn't match the machine's role. If either
/// side is unset, don't gate (lenient — the machine may not declare a role).
fn role_gated(step_role: &Option<String>, machine: &Machine) -> bool {
    match (step_role, &machine.role) {
        (Some(r), Some(mr)) => r != mr,
        _ => false,
    }
}

/// Everything a machine composes, OS-gated to this host.
pub struct Resolved {
    pub steps: Vec<(String, Step)>,     // (app, step)
    pub asserts: Vec<(String, Assert)>, // (app, assert)
}

pub fn resolve(home: &Path, machine: &Machine) -> Result<Resolved> {
    let mut steps = Vec::new();
    let mut asserts = Vec::new();
    for app in &machine.apps {
        let bundle = manifest::load_bundle(home, app)?;
        for step in bundle.step {
            if !os_gated(&step.os, machine) && !role_gated(&step.role, machine) {
                steps.push((app.clone(), step));
            }
        }
        for assert in bundle.assert {
            if !os_gated(&assert.os, machine) {
                asserts.push((app.clone(), assert));
            }
        }
    }
    Ok(Resolved { steps, asserts })
}

fn copy_opts<'a>(step: &'a Step, vars: &'a BTreeMap<String, String>) -> CopyOpts<'a> {
    CopyOpts {
        template: step.template,
        seed: step.seed,
        mode: step.mode.as_deref(),
        vars,
    }
}

fn exec_opts<'a>(home: &'a Path, machine: &'a Machine, step: &'a Step) -> ExecOpts<'a> {
    ExecOpts {
        secrets: &step.secrets,
        home,
        machine: &machine.name,
        os: &machine.os,
    }
}

fn is_step(step: &Step) -> bool {
    step.copy.is_some()
        || step.block.is_some()
        || step.setkey.is_some()
        || step.exec.is_some()
        || step.profile.is_some()
}

/// One drift finding across any primitive or assertion.
pub struct Finding {
    pub app: String,
    pub kind: &'static str,
    pub target: String,
    pub ok: bool,
    pub status: String,
}

impl Finding {
    fn state(app: &str, kind: &'static str, target: String, state: FileState) -> Finding {
        Finding {
            app: app.to_string(),
            kind,
            target,
            ok: state.is_ok(),
            status: state.label().to_string(),
        }
    }
}

/// Drift a single step, if it's one we evaluate.
fn step_finding(
    home: &Path,
    machine: &Machine,
    app: &str,
    step: &Step,
    vars: &BTreeMap<String, String>,
) -> Result<Option<Finding>> {
    if let (Some(copy), Some(to)) = (&step.copy, &step.to) {
        let state =
            primitives::copy_state(&home.join(copy), &expand_tilde(to), &copy_opts(step, vars))?;
        return Ok(Some(Finding::state(app, "copy", to.clone(), state)));
    }
    if let (Some(block), Some(in_file)) = (&step.block, &step.in_file) {
        let marker = step.marker.as_deref().unwrap_or("block");
        let state = primitives::block_state(&home.join(block), &expand_tilde(in_file), marker)?;
        return Ok(Some(Finding::state(app, "block", in_file.clone(), state)));
    }
    if let Some(sk) = &step.setkey {
        let state = primitives::setkey_state(sk)?;
        let target = format!("{}:{}", sk.file.as_deref().unwrap_or(&sk.backend), sk.key);
        return Ok(Some(Finding::state(app, "setkey", target, state)));
    }
    if let Some(exec) = &step.exec {
        let opts = exec_opts(home, machine, step);
        let check = step.check.as_ref().map(|c| home.join(c));
        let (ok, status) = primitives::exec_state(check.as_deref(), &opts)?;
        return Ok(Some(Finding {
            app: app.to_string(),
            kind: "exec",
            target: exec.clone(),
            ok,
            status,
        }));
    }
    if let Some(profile) = &step.profile {
        // Not verifiable without MDM — reported status-only, never "drifted".
        return Ok(Some(Finding {
            app: app.to_string(),
            kind: "profile",
            target: profile.clone(),
            ok: true,
            status: "manual".into(),
        }));
    }
    Ok(None)
}

pub fn run_drift(
    home: &Path,
    machine: &Machine,
    vars: &BTreeMap<String, String>,
    ignore: &Ignore,
) -> Result<Vec<Finding>> {
    let resolved = resolve(home, machine)?;
    let mut findings = Vec::new();
    for (app, step) in &resolved.steps {
        if let Some(mut f) = step_finding(home, machine, app, step, vars)? {
            // A `manual` step is never applied by install/update, so don't
            // report it as permanent, unfixable drift — mark it status-only.
            if lifecycle(step) == "manual" && !f.ok {
                f.status = format!("manual — {}", f.status);
                f.ok = true;
            }
            findings.push(f);
        }
    }
    for (app, assert) in &resolved.asserts {
        let (ok, status) = drift::eval(home, assert)?;
        findings.push(Finding {
            app: app.clone(),
            kind: drift::kind(assert),
            target: drift::target(assert),
            ok,
            status,
        });
    }

    // Package drift (machine scope): inert when no packages are declared.
    let effective = packages::effective_set(home, machine)?;
    if !effective.is_empty() {
        let installed = providers::probe(&effective)?;
        for p in packages::missing(&effective, &installed) {
            findings.push(Finding {
                app: "packages".into(),
                kind: "package",
                target: format!("{} {}", p.manager.as_str(), p.name),
                ok: false,
                status: "missing".into(),
            });
        }
        for (m, name) in packages::extras(&effective, &installed, ignore) {
            // brew-family extras are computed dependency-aware below (a naive
            // set-diff wrongly flags every installed transitive dependency).
            if matches!(
                m,
                packages::Manager::Brew | packages::Manager::Cask | packages::Manager::Tap
            ) {
                continue;
            }
            findings.push(Finding {
                app: "packages".into(),
                kind: "package-extra",
                target: format!("{} {}", m.as_str(), name),
                ok: false,
                status: "extra".into(),
            });
        }
        for name in providers::brew_extras(&effective, ignore)? {
            findings.push(Finding {
                app: "packages".into(),
                kind: "package-extra",
                target: format!("brew {name}"),
                ok: false,
                status: "extra".into(),
            });
        }
    }

    // GNOME extensions + rpm-ostree (Linux; inert where their CLIs are absent).
    for uuid in providers::gext_missing(&providers::effective_extensions(home, machine)?) {
        findings.push(Finding {
            app: "extensions".into(),
            kind: "extension",
            target: uuid,
            ok: false,
            status: "missing".into(),
        });
    }
    for pkg in providers::rpm_missing(&providers::effective_rpm(home, machine)?) {
        findings.push(Finding {
            app: "rpm".into(),
            kind: "rpm",
            target: pkg,
            ok: false,
            status: "missing".into(),
        });
    }

    Ok(findings)
}

/// Apply a single step. Returns whether it changed anything.
fn apply_step(
    home: &Path,
    machine: &Machine,
    step: &Step,
    vars: &BTreeMap<String, String>,
    journal: &mut Journal,
) -> Result<bool> {
    if let (Some(copy), Some(to)) = (&step.copy, &step.to) {
        return primitives::copy_apply(
            &home.join(copy),
            &expand_tilde(to),
            &copy_opts(step, vars),
            journal,
        );
    }
    if let (Some(block), Some(in_file)) = (&step.block, &step.in_file) {
        let marker = step.marker.as_deref().unwrap_or("block");
        return primitives::block_apply(&home.join(block), &expand_tilde(in_file), marker, journal);
    }
    if let Some(sk) = &step.setkey {
        return primitives::setkey_apply(sk, journal);
    }
    if let Some(exec) = &step.exec {
        let opts = exec_opts(home, machine, step);
        let check = step.check.as_ref().map(|c| home.join(c));
        return primitives::exec_apply(&home.join(exec), check.as_deref(), &opts);
    }
    if let Some(profile) = &step.profile {
        return primitives::profile_apply(&home.join(profile));
    }
    bail!("step names no known primitive (copy / block / setkey / exec / profile)")
}

/// A step's effective lifecycle. Defaults by primitive: exec & seed are
/// install-only; copy/setkey/block are re-applied every update ("always").
fn lifecycle(step: &Step) -> &str {
    if let Some(r) = &step.run {
        return r;
    }
    if step.exec.is_some() || step.seed {
        "install"
    } else {
        "always"
    }
}

/// Would this step change anything? (dry-run preview — never runs an `exec`.)
fn step_would_change(
    home: &Path,
    machine: &Machine,
    step: &Step,
    vars: &BTreeMap<String, String>,
) -> Result<bool> {
    if step.exec.is_some() {
        // Never run the script during a preview. Use the check hook if present;
        // otherwise we can't tell, so assume it would run.
        return match &step.check {
            Some(check) => {
                let opts = exec_opts(home, machine, step);
                Ok(!primitives::exec_check(&home.join(check), &opts)?)
            }
            None => Ok(true),
        };
    }
    Ok(step_finding(home, machine, "", step, vars)?.map_or(false, |f| !f.ok))
}

/// Outcome of an install run.
pub struct InstallReport {
    /// Declared packages considered by the converge phase.
    pub packages: usize,
    pub steps_changed: usize,
    pub steps_total: usize,
    /// A layered rpm was added and the machine needs a reboot.
    pub reboot: bool,
}

/// Install flow: phase 1 converges packages (whole-machine), phase 2 applies
/// config steps. With `dry_run`, nothing is written, journaled, converged, or
/// exec'd — it only reports what would change.
pub fn run_install(
    home: &Path,
    machine: &Machine,
    vars: &BTreeMap<String, String>,
    brew_trust: &[String],
    dry_run: bool,
) -> Result<InstallReport> {
    // Never apply one machine's config to a different-OS host (drift/dry-run
    // from anywhere is fine — only a live converge is refused).
    if !dry_run && machine.os != crate::machine::current_os() {
        bail!(
            "refusing to install '{}' (os={}) on a {} host — run it on the target machine, \
             or use --dry-run to preview",
            machine.name,
            machine.os,
            crate::machine::current_os()
        );
    }

    // Phase 1 — packages (aggregate converge; inert without declared packages).
    let effective = packages::effective_set(home, machine)?;
    if !dry_run {
        providers::trust_taps(brew_trust)?;
    }
    let packages = providers::converge(&effective, dry_run)?;
    providers::gext_converge(&providers::effective_extensions(home, machine)?, dry_run)?;
    let reboot = providers::rpm_converge(&providers::effective_rpm(home, machine)?, dry_run)?;

    // Phase 2 — config steps.
    let resolved = resolve(home, machine)?;
    let mut journal = Journal::begin();
    let (mut changed, mut total) = (0usize, 0usize);
    for (_app, step) in &resolved.steps {
        // `manual` steps are never run by an automated flow (e.g. speaker-eq's
        // interactive picker) — only when explicitly invoked.
        if !is_step(step) || lifecycle(step) == "manual" {
            continue;
        }
        total += 1;
        if dry_run {
            if step_would_change(home, machine, step, vars)? {
                changed += 1;
            }
        } else if apply_step(home, machine, step, vars, &mut journal)? {
            changed += 1;
        }
    }
    if !dry_run {
        journal.commit()?;
    }
    Ok(InstallReport {
        packages,
        steps_changed: changed,
        steps_total: total,
        reboot,
    })
}

/// Prune installed-but-not-declared packages. Returns the extras (computed by
/// the unit-tested set logic); with `dry_run` it only lists, otherwise it also
/// removes them (VM-verified shell-out).
pub fn run_prune(
    home: &Path,
    machine: &Machine,
    ignore: &Ignore,
    dry_run: bool,
) -> Result<Vec<(packages::Manager, String)>> {
    let effective = packages::effective_set(home, machine)?;
    if effective.is_empty() {
        return Ok(Vec::new());
    }
    let installed = providers::probe(&effective)?;
    let extras = packages::extras(&effective, &installed, ignore);
    if !dry_run && !extras.is_empty() {
        providers::prune_apply(&effective, &extras)?;
    }
    Ok(extras)
}

/// Backup: dump live package state into `machines/<name>/Brewfile`. Returns the
/// written path. (dconf snapshot lands with the Linux slice.)
pub fn run_backup(home: &Path, machine: &Machine) -> Result<std::path::PathBuf> {
    providers::dump(home, &machine.name)
}

/// Adopt (advisory v1): report the installed extras so they can be added to a
/// bundle, the machine loose list, or `[ignore]`. Non-mutating; interactive
/// folder-authoring is a later refinement.
pub fn run_adopt(
    home: &Path,
    machine: &Machine,
    ignore: &Ignore,
) -> Result<Vec<(packages::Manager, String)>> {
    let effective = packages::effective_set(home, machine)?;
    if effective.is_empty() {
        return Ok(Vec::new());
    }
    let installed = providers::probe(&effective)?;
    Ok(packages::extras(&effective, &installed, ignore))
}

/// Update flow: upgrade declared packages (only if any are declared — so a
/// machine with no packages never triggers a global `brew upgrade`), then
/// re-apply the `always`/`ensure` config steps. Skips install-only steps
/// (seed, one-time exec) so it stays the "safe and boring" flow.
pub fn run_update(
    home: &Path,
    machine: &Machine,
    vars: &BTreeMap<String, String>,
    brew_trust: &[String],
) -> Result<InstallReport> {
    let effective = packages::effective_set(home, machine)?;
    if !effective.is_empty() {
        providers::trust_taps(brew_trust)?;
        providers::upgrade()?;
    }

    let resolved = resolve(home, machine)?;
    let mut journal = Journal::begin();
    let (mut changed, mut total) = (0usize, 0usize);
    for (_app, step) in &resolved.steps {
        if !is_step(step) {
            continue;
        }
        let lc = lifecycle(step);
        if lc != "always" && lc != "ensure" {
            continue;
        }
        total += 1;
        if apply_step(home, machine, step, vars, &mut journal)? {
            changed += 1;
        }
    }
    journal.commit()?;
    Ok(InstallReport {
        packages: effective.len(),
        steps_changed: changed,
        steps_total: total,
        reboot: false,
    })
}
