//! Build the ordered steps + assertions for a machine, then evaluate (drift) or
//! apply (install/update) them, with presence-gating (`when`/`needs`) and a
//! both-direction remediation summary. Step primitives: `copy`, `block`,
//! `setkey` (all backends), `exec`, `profile`, `sysfile`; assertions are
//! drift-only. Flows `install` / `update` / `prune` / `backup` / `restore` /
//! `adopt` / `reconcile` are all live.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Result};

use crate::journal::Journal;
use crate::manifest::{self, expand_tilde, Assert, Ignore, Machine, Step};
use crate::primitives::{self, CopyOpts, ExecOpts, FileState};
use crate::{drift, packages, probe, providers};

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
            if !manifest::gated(&step.os, &step.role, machine) {
                steps.push((app.clone(), step));
            }
        }
        for assert in bundle.assert {
            if !manifest::gated(&assert.os, &assert.role, machine) {
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
        || step.sysfile.is_some()
}

/// `sysfile` options from a step.
fn sysfile_opts(step: &Step) -> primitives::SysfileOpts<'_> {
    primitives::SysfileOpts {
        mode: step.mode.as_deref(),
        owner: step.owner.as_deref(),
        group: step.group.as_deref(),
    }
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

    /// An `ok` finding that isn't actually enforced-in-sync — reported for
    /// visibility but not repairable/converged: a `manual` step, a backend whose
    /// tool is absent (`unavailable`), an `exec` with no drift hook, or a
    /// `profile` (GUI apply). The drift renderer surfaces these separately so
    /// they neither read as green "in sync" nor as red drift.
    pub fn status_only(&self) -> bool {
        self.kind == "profile"
            || self.kind == "when"
            || self.status.starts_with("unavailable") // incl. "unavailable — secret …"
            || self.status == "no drift-check"
            || self.status.starts_with("manual")
    }
}

/// The outcome of a step's presence gate (`when`/`needs`).
enum Gate {
    /// Apply/evaluate the step normally.
    Apply,
    /// `when` failed — skip the step (loudly). Carries the probe description.
    Skip(String),
    /// `needs` failed — a hard requirement is absent. Carries the description.
    Require(String),
}

/// Evaluate a step's `needs` (hard) then `when` (soft) presence gate.
fn gate_step(home: &Path, step: &Step) -> Gate {
    if let Some(n) = &step.needs {
        if !probe::passes(home, n) {
            return Gate::Require(probe::describe(n));
        }
    }
    if let Some(w) = &step.when {
        if !probe::passes(home, w) {
            return Gate::Skip(probe::describe(w));
        }
    }
    Gate::Apply
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
        let state = primitives::setkey_state(sk, vars)?;
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
    if let (Some(sysfile), Some(to)) = (&step.sysfile, &step.to) {
        let state =
            primitives::sysfile_state(&home.join(sysfile), &expand_tilde(to), &sysfile_opts(step))?;
        return Ok(Some(Finding::state(app, "sysfile", to.clone(), state)));
    }
    Ok(None)
}

/// A short `kind target` label for a step, for the progress region and the `✓`
/// lines. Pure and cheap on purpose — unlike `step_finding` it probes nothing, so
/// naming a step costs nothing before we know whether it will change anything.
fn step_label(step: &Step) -> String {
    if let (Some(_), Some(to)) = (&step.copy, &step.to) {
        return format!("copy {to}");
    }
    if let (Some(_), Some(in_file)) = (&step.block, &step.in_file) {
        return format!("block {in_file}");
    }
    if let Some(sk) = &step.setkey {
        return format!(
            "setkey {}:{}",
            sk.file.as_deref().unwrap_or(&sk.backend),
            sk.key
        );
    }
    if let Some(exec) = &step.exec {
        return format!("exec {exec}");
    }
    if let Some(profile) = &step.profile {
        return format!("profile {profile}");
    }
    if let (Some(_), Some(to)) = (&step.sysfile, &step.to) {
        return format!("sysfile {to}");
    }
    "step".into()
}

/// A suggested next command to resolve drift — the "what to run next" hand-off
/// RIS emits at the moment of detection. Each is a human label + the exact,
/// copy-pasteable invocation.
pub struct Remediation {
    pub label: String,
    pub command: String,
}

/// Both-direction remediations for a drift report (RIS's four-branch package
/// fork + a config line). Machine→spec: `install-missing` / `prune` /
/// re-`install`. Spec←machine: `reconcile` / `backup`. Plus `undo` to revert.
/// Empty when nothing is out of sync.
///
/// Commands are **bare** (no machine name): every verb defaults to this host by
/// hostname, and you run these on the machine they apply to — so the name is
/// noise (and passing it would trip the not-this-host confirm).
pub fn remediations(items: &[Finding]) -> Vec<Remediation> {
    let drifted = |kinds: &[&str]| items.iter().any(|f| !f.ok && kinds.contains(&f.kind));
    let missing_pkg = drifted(&["package", "extension", "rpm"]);
    let extra_pkg = drifted(&["package-extra"]);
    let trust_gap = drifted(&["trust"]);
    let trust_extra = drifted(&["trust-extra"]);
    let config_drift = items.iter().any(|f| {
        !f.ok
            && ![
                "package",
                "package-extra",
                "extension",
                "rpm",
                "trust",
                "trust-extra",
            ]
            .contains(&f.kind)
    });

    let mut out = Vec::new();
    let push = |out: &mut Vec<Remediation>, label: &str, command: &str| {
        out.push(Remediation {
            label: label.to_string(),
            command: command.to_string(),
        })
    };
    // Machine → spec (converge the machine toward the declared state).
    if missing_pkg {
        push(
            &mut out,
            "install declared packages that are missing",
            "temper install --packages-only",
        );
    }
    if extra_pkg || trust_extra {
        let label = if extra_pkg && trust_extra {
            "uninstall packages / untrust taps not in the spec (asks first)"
        } else if trust_extra {
            "untrust taps not in the spec (asks first)"
        } else {
            "uninstall packages not in the spec (asks first)"
        };
        push(&mut out, label, "temper prune");
    }
    if trust_gap {
        push(
            &mut out,
            "trust declared taps so brew loads their formulae",
            "temper install --packages-only",
        );
    }
    // Spec ← machine (absorb the machine's state into the spec).
    if missing_pkg || extra_pkg || trust_extra || trust_gap {
        push(
            &mut out,
            "interactively add extras / drop missing entries (packages + tap-trust)",
            "temper reconcile",
        );
    }
    if missing_pkg || extra_pkg {
        push(
            &mut out,
            "overwrite the machine Brewfile with live state",
            "temper backup",
        );
    }
    // Config drift: re-apply, or revert the last run.
    if config_drift {
        push(
            &mut out,
            "re-apply configuration to fix the drift above",
            "temper install",
        );
        push(
            &mut out,
            "revert the most recent run instead",
            "temper undo",
        );
    }
    out
}

pub fn run_drift(
    home: &Path,
    machine: &Machine,
    vars: &BTreeMap<String, String>,
    ignore: &Ignore,
    brew_trust: &[String],
) -> Result<Vec<Finding>> {
    let resolved = resolve(home, machine)?;
    let mut findings = Vec::new();
    for (app, step) in &resolved.steps {
        // Presence gate: a `when`-skipped step is status-only (its app isn't
        // here); a failed `needs` is real drift (a hard dep is missing).
        match gate_step(home, step) {
            Gate::Skip(desc) => {
                findings.push(Finding {
                    app: app.clone(),
                    kind: "when",
                    target: desc.clone(),
                    ok: true,
                    status: format!("skipped: {desc} absent"),
                });
                continue;
            }
            Gate::Require(desc) => {
                findings.push(Finding {
                    app: app.clone(),
                    kind: "needs",
                    target: desc.clone(),
                    ok: false,
                    status: format!("required {desc} is absent"),
                });
                continue;
            }
            Gate::Apply => {}
        }
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
        let extras = packages::extras(&effective, &installed, ignore);
        // mas extras come back as bare numeric ids (that's what a mas probe
        // yields). Resolve them to app names so a drifted App Store item is
        // legible — only shelling out to `mas list` when there's one to name.
        let mas_names = if extras.iter().any(|(m, _)| *m == packages::Manager::Mas) {
            providers::mas_names()
        } else {
            std::collections::BTreeMap::new()
        };
        for (m, name) in extras {
            // brew-family extras are computed dependency-aware below (a naive
            // set-diff wrongly flags every installed transitive dependency).
            if matches!(
                m,
                packages::Manager::Brew | packages::Manager::Cask | packages::Manager::Tap
            ) {
                continue;
            }
            let target = match m {
                packages::Manager::Mas => match mas_names.get(&name) {
                    Some(app) => format!("mas \"{app}\" (id {name})"),
                    None => format!("mas {name}"), // not in `mas list` — id is all we have
                },
                _ => format!("{} {}", m.as_str(), name),
            };
            findings.push(Finding {
                app: "packages".into(),
                kind: "package-extra",
                target,
                ok: false,
                status: "extra".into(),
            });
        }
        for (m, name) in providers::brew_extras(&effective, ignore)? {
            findings.push(Finding {
                app: "packages".into(),
                kind: "package-extra",
                target: format!("{} {}", m.as_str(), name),
                ok: false,
                status: "extra".into(),
            });
        }
    }

    // brew tap-trust drift — both directions, mirroring packages. Skipped
    // entirely when brew is absent (`trusted_taps` → None), so a declared
    // `[brew].trust` on a non-brew host doesn't read as "all untrusted".
    if let Some(trusted) = providers::trusted_taps()? {
        // Declared but not trusted → a gap (brew silently skips the tap's
        // formulae). Fixed by `install`/`update`, which run `brew trust`.
        for tap in brew_trust {
            if !trusted.iter().any(|t| t == tap) {
                findings.push(Finding {
                    app: "trust".into(),
                    kind: "trust",
                    target: format!("tap {tap}"),
                    ok: false,
                    status: "untrusted".into(),
                });
            }
        }
        // Trusted but not declared → an extra. Honors `[ignore].tap` so a known
        // baseline tap isn't nagged. Absorbed into `[brew].trust` by `reconcile`.
        for tap in &trusted {
            if !brew_trust.iter().any(|t| t == tap) && !ignore.tap.iter().any(|t| t == tap) {
                findings.push(Finding {
                    app: "trust".into(),
                    kind: "trust-extra",
                    target: format!("tap {tap}"),
                    ok: false,
                    status: "trusted-extra".into(),
                });
            }
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
    verbose: bool,
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
        return primitives::setkey_apply(sk, vars, journal);
    }
    if let Some(exec) = &step.exec {
        let opts = exec_opts(home, machine, step);
        let check = step.check.as_ref().map(|c| home.join(c));
        return primitives::exec_apply(&home.join(exec), check.as_deref(), &opts, verbose);
    }
    if let Some(profile) = &step.profile {
        return primitives::profile_apply(&home.join(profile));
    }
    if let (Some(sysfile), Some(to)) = (&step.sysfile, &step.to) {
        return primitives::sysfile_apply(
            &home.join(sysfile),
            &expand_tilde(to),
            &sysfile_opts(step),
        );
    }
    bail!("step names no known primitive (copy / block / setkey / exec / profile / sysfile)")
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
        let opts = exec_opts(home, machine, step);
        // A missing secret means the hook can't run — a read-only preview must
        // degrade (report no change), not abort.
        if primitives::exec_missing_secret(&opts).is_some() {
            return Ok(false);
        }
        return match &step.check {
            Some(check) => Ok(!primitives::exec_check(&home.join(check), &opts)?),
            None => Ok(true),
        };
    }
    Ok(step_finding(home, machine, "", step, vars)?.is_some_and(|f| !f.ok))
}

/// Outcome of an install run.
pub struct InstallReport {
    /// Declared packages considered by the converge phase.
    pub packages: usize,
    /// Packages actually **brought up to date** by an `update` — measured by
    /// diffing installed versions across the upgrade, so a failed or partial
    /// upgrade reports what really landed rather than what was attempted.
    /// `None` on flows that don't upgrade (`install`, `install-missing`).
    pub upgraded: Option<usize>,
    pub steps_changed: usize,
    pub steps_total: usize,
    /// A layered rpm was added and the machine needs a reboot.
    pub reboot: bool,
    /// Steps skipped because their `when` probe failed (app not present) —
    /// announced loudly (Principle #6). Each entry is a probe description.
    pub skipped: Vec<String>,
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
    packages_only: bool,
    verbose: bool,
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
    // One password for the whole run. Homebrew needs root once per pkg-based cask
    // and prompts for each — minutes apart, since sudo's timestamp expires during
    // the multi-GB downloads in between. So: find out up front whether root is
    // needed at all, ask once here (at the keyboard, before anything downloads),
    // and hold the timestamp open for the rest of the run.
    let _sudo = (!dry_run).then(|| {
        let root = providers::casks_needing_root(&effective);
        if !root.is_empty() {
            crate::sudo::acquire(&format!(
                "{} package(s) install with a system installer and need your password: {}",
                root.len(),
                root.join(", ")
            ));
        }
        crate::sudo::keep_alive()
    });
    if !dry_run {
        providers::trust_taps(brew_trust, verbose)?;
    }
    let packages = providers::converge(&effective, dry_run, verbose)?;
    providers::gext_converge(
        &providers::effective_extensions(home, machine)?,
        dry_run,
        verbose,
    )?;
    let reboot = providers::rpm_converge(
        &providers::effective_rpm(home, machine)?,
        dry_run,
        verbose,
    )?;

    // `install-missing`: packages only — skip the config-step phase entirely.
    if packages_only {
        return Ok(InstallReport {
            packages,
            upgraded: None, // `install-missing` adds, never upgrades
            steps_changed: 0,
            steps_total: 0,
            reboot,
            skipped: Vec::new(),
        });
    }

    // Phase 2 — config steps.
    let resolved = resolve(home, machine)?;
    let mut journal = Journal::begin();
    let (mut changed, mut total) = (0usize, 0usize);
    let mut skipped = Vec::new();
    // Candidates are known before any of them runs, so the phase has an honest
    // denominator. A dry-run reports rather than applies — no live region for it.
    let candidates = resolved
        .steps
        .iter()
        .filter(|(_, s)| is_step(s) && lifecycle(s) != "manual")
        .count();
    let cl = crate::ui::Checklist::new(if dry_run { 0 } else { candidates }, "config", verbose);
    for (app, step) in &resolved.steps {
        // `manual` steps are never run by an automated flow (e.g. speaker-eq's
        // interactive picker) — only when explicitly invoked.
        if !is_step(step) || lifecycle(step) == "manual" {
            continue;
        }
        let label = format!("{app} · {}", step_label(step));
        cl.start(&label);
        // Presence gate — skip loudly when absent, error on a failed `needs`.
        match gate_step(home, step) {
            Gate::Skip(desc) => {
                cl.skipped(&label, &desc);
                skipped.push(desc);
                continue;
            }
            Gate::Require(desc) => bail!("step in `{app}` needs {desc}, which is absent"),
            Gate::Apply => {}
        }
        total += 1;
        if dry_run {
            if step_would_change(home, machine, step, vars)? {
                changed += 1;
            }
        } else if apply_step(home, machine, step, vars, &mut journal, verbose)? {
            changed += 1;
            cl.done(&label);
        } else {
            cl.unchanged();
        }
    }
    cl.finish();
    if !dry_run {
        journal.commit()?;
    }
    Ok(InstallReport {
        packages,
        upgraded: None, // `install` converges the declared set; `update` upgrades
        steps_changed: changed,
        steps_total: total,
        reboot,
        skipped,
    })
}

/// What a prune would remove: installed-but-undeclared packages, plus taps
/// trusted on the machine but not in `[brew].trust` (nor `[ignore].tap`). Both
/// are the machine→spec convergence — the mirror of `reconcile`'s absorb.
#[derive(Debug, Default)]
pub struct PrunePlan {
    pub packages: Vec<(packages::Manager, String)>,
    /// Taps to `brew untrust` — the prune counterpart of a `trust-extra`.
    pub untrust: Vec<String>,
}

impl PrunePlan {
    pub fn is_empty(&self) -> bool {
        self.packages.is_empty() && self.untrust.is_empty()
    }
    pub fn len(&self) -> usize {
        self.packages.len() + self.untrust.len()
    }
}

/// Prune installed-but-not-declared packages **and** trusted-but-undeclared
/// taps. Returns the plan; the caller previews and confirms before
/// `commit_prune` applies it.
///
/// Mirrors `run_drift`: brew-family (brew/cask/tap) extras are computed
/// dependency-aware via `providers::brew_extras` (a naive set-diff wrongly flags
/// every installed transitive dependency); only non-brew managers use the naive
/// `packages::extras`. Tap-trust extras are the untrust side of drift's
/// `trusted-extra`. Inert when the machine declares no packages (so an
/// unconfigured machine never proposes nuking everything).
pub fn run_prune(
    home: &Path,
    machine: &Machine,
    ignore: &Ignore,
    brew_trust: &[String],
) -> Result<PrunePlan> {
    let effective = packages::effective_set(home, machine)?;
    if effective.is_empty() {
        return Ok(PrunePlan::default());
    }
    let installed = providers::probe(&effective)?;
    let mut packages: Vec<(packages::Manager, String)> =
        packages::extras(&effective, &installed, ignore)
            .into_iter()
            .filter(|(m, _)| {
                !matches!(
                    m,
                    packages::Manager::Brew | packages::Manager::Cask | packages::Manager::Tap
                )
            })
            .collect();
    packages.extend(providers::brew_extras(&effective, ignore)?);

    // Trusted-but-undeclared taps → untrust (honors `[ignore].tap`). Skipped
    // without brew (`trusted_taps` → None).
    let mut untrust = Vec::new();
    if let Some(trusted) = providers::trusted_taps()? {
        for tap in &trusted {
            if !brew_trust.iter().any(|t| t == tap) && !ignore.tap.iter().any(|t| t == tap) {
                untrust.push(tap.clone());
            }
        }
    }
    untrust.sort();
    Ok(PrunePlan { packages, untrust })
}

/// Apply a prune: uninstall the packages and `brew untrust` the taps.
/// Destructive — the caller previews and confirms first (`run_prune` computes
/// the plan without touching anything). Recomputes the effective set so `brew
/// bundle cleanup` keeps a declared package's transitive deps. A no-op on an
/// empty plan.
pub fn commit_prune(home: &Path, machine: &Machine, plan: &PrunePlan) -> Result<()> {
    // Uninstalling a pkg-based cask needs root per cask, same as installing one.
    // No `acquire` here: the plan is already confirmed and brew asks in its own
    // words on the first one — this just stops it asking again for the rest.
    let _sudo = crate::sudo::keep_alive();
    if !plan.packages.is_empty() {
        let effective = packages::effective_set(home, machine)?;
        providers::prune_apply(&effective, &plan.packages)?;
    }
    if !plan.untrust.is_empty() {
        providers::untrust_taps(&plan.untrust)?;
    }
    Ok(())
}

/// What a `backup` wrote: the dumped Brewfile + any filtered dconf snapshots.
pub struct BackupReport {
    pub brewfile: std::path::PathBuf,
    pub dconf: Vec<std::path::PathBuf>,
}

/// Backup: dump live package state into `machines/<name>/Brewfile`, plus each
/// declared dconf snapshot (filtered) into its file. dconf is a no-op where the
/// tool is absent (a Mac) or the machine declares none.
pub fn run_backup(home: &Path, machine: &Machine) -> Result<BackupReport> {
    // Dump into the machine's OWN brewfile (the file it actually reads), so a
    // backup feeds back into the spec; fall back to machines/<name>/Brewfile if
    // the machine declares no brewfile.
    let bf_rel = machine
        .brewfile
        .clone()
        .unwrap_or_else(|| format!("machines/{}/Brewfile", machine.name));
    let dest = home.join(&bf_rel);
    let before = std::fs::read(&dest).ok();
    providers::dump_to(&dest)?;
    let after = std::fs::read(&dest)
        .map_err(|e| anyhow::anyhow!("reading dumped {}: {e}", dest.display()))?;

    // Journal the writes so `undo` reverts a backup.
    let mut journal = Journal::begin();
    journal.record_write(&dest, before.as_deref(), &after)?;
    let dconf = crate::dconf::backup(home, machine, &mut journal)?;
    journal.commit()?;
    Ok(BackupReport {
        brewfile: dest,
        dconf,
    })
}

/// Restore: load each declared dconf snapshot back into live dconf. The CLI
/// confirms first — this clobbers live desktop state (never run by `update`).
pub fn run_restore(home: &Path, machine: &Machine) -> Result<Vec<std::path::PathBuf>> {
    crate::dconf::restore(home, machine)
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

/// Whether an `ensure` step should apply on `update`. Semantics: install-if-
/// missing — create an absent target, but DON'T overwrite a present-but-drifted
/// one (that's what `always` is for). An `exec` `ensure` runs when its
/// drift-hook fails (not-yet-done); without a hook it's skipped on update.
fn ensure_should_apply(
    home: &Path,
    machine: &Machine,
    step: &Step,
    vars: &BTreeMap<String, String>,
) -> Result<bool> {
    Ok(match step_finding(home, machine, "", step, vars)? {
        Some(f) => !f.ok && (f.status == "missing" || step.exec.is_some()),
        None => false,
    })
}

/// Update flow: upgrade declared packages (only if any are declared — so a
/// machine with no packages never triggers a global `brew upgrade`), then
/// re-apply `always` config steps and create-if-missing `ensure` steps. Skips
/// install-only + manual steps so it stays the "safe and boring" flow.
pub fn run_update(
    home: &Path,
    machine: &Machine,
    vars: &BTreeMap<String, String>,
    brew_trust: &[String],
    verbose: bool,
) -> Result<InstallReport> {
    let effective = packages::effective_set(home, machine)?;
    // Same one-password-per-run deal as `install`: upgrading a pkg-based cask
    // needs root just like installing it, and `sysfile` steps below shell out to
    // `sudo install`. Ask once, up front, only if something actually needs it.
    let root = providers::casks_needing_root(&effective);
    if !root.is_empty() {
        crate::sudo::acquire(&format!(
            "{} package(s) upgrade with a system installer and need your password: {}",
            root.len(),
            root.join(", ")
        ));
    }
    let _sudo = crate::sudo::keep_alive();
    // Report the *effect*, not the invocation: snapshot installed versions either
    // side of the upgrade and count what actually moved. A failed or partly-applied
    // upgrade then reports what really landed (the failure has warned separately),
    // and a converged machine says so instead of reciting how many packages it
    // declares. Asking a package manager what it thinks is outdated would not do:
    // `brew outdated` was observed reporting nothing while `brew upgrade` upgraded
    // twelve packages — see `installed_versions`.
    let mut upgraded = None;
    if !effective.is_empty() {
        providers::trust_taps(brew_trust, verbose)?;
        let before = providers::installed_versions();
        providers::upgrade(verbose)?;
        let after = providers::installed_versions();
        upgraded = Some(providers::upgraded_between(&before, &after));
    }

    let resolved = resolve(home, machine)?;
    let mut journal = Journal::begin();
    let (mut changed, mut total) = (0usize, 0usize);
    let mut skipped = Vec::new();
    // `always` + `ensure` are what an update re-applies; `ensure` is filtered
    // again inside the loop (it needs a probe), so this is an upper bound — the
    // counter can finish short of its total, which beats a total that grows.
    let candidates = resolved
        .steps
        .iter()
        .filter(|(_, s)| is_step(s) && matches!(lifecycle(s), "always" | "ensure"))
        .count();
    let cl = crate::ui::Checklist::new(candidates, "config", verbose);
    for (app, step) in &resolved.steps {
        if !is_step(step) {
            continue;
        }
        match lifecycle(step) {
            "always" => {} // re-apply (fixes drift)
            "ensure" => {
                if !ensure_should_apply(home, machine, step, vars)? {
                    cl.unchanged();
                    continue; // present already → don't overwrite
                }
            }
            _ => continue, // install-only + manual are not applied on update
        }
        let label = format!("{app} · {}", step_label(step));
        cl.start(&label);
        // Presence gate — same as install.
        match gate_step(home, step) {
            Gate::Skip(desc) => {
                cl.skipped(&label, &desc);
                skipped.push(desc);
                continue;
            }
            Gate::Require(desc) => bail!("step in `{app}` needs {desc}, which is absent"),
            Gate::Apply => {}
        }
        total += 1;
        if apply_step(home, machine, step, vars, &mut journal, verbose)? {
            changed += 1;
            cl.done(&label);
        } else {
            cl.unchanged();
        }
    }
    cl.finish();
    journal.commit()?;
    Ok(InstallReport {
        packages: effective.len(),
        upgraded,
        steps_changed: changed,
        steps_total: total,
        reboot: false,
        skipped,
    })
}

#[cfg(test)]
mod remediation_tests {
    use super::*;

    fn f(kind: &'static str, ok: bool) -> Finding {
        Finding {
            app: "a".into(),
            kind,
            target: "t".into(),
            ok,
            status: "s".into(),
        }
    }

    #[test]
    fn missing_and_extra_offer_both_directions() {
        let items = vec![f("package", false), f("package-extra", false)];
        let cmds: Vec<String> = remediations(&items)
            .iter()
            .map(|r| r.command.clone())
            .collect();
        // Bare commands — no machine name (default resolves this host).
        assert!(cmds.contains(&"temper install --packages-only".to_string())); // add missing
        assert!(cmds.contains(&"temper prune".to_string())); // remove extras
        assert!(cmds.contains(&"temper reconcile".to_string())); // absorb (surgical)
        assert!(cmds.contains(&"temper backup".to_string())); // absorb (wholesale)
                                                              // never a machine name baked into a suggested command
        assert!(!cmds
            .iter()
            .any(|c| c.split_whitespace().count() > 3 && !c.contains("--")));
    }

    #[test]
    fn config_drift_offers_reapply_and_undo() {
        let items = vec![f("copy", false)];
        let cmds: Vec<String> = remediations(&items)
            .iter()
            .map(|r| r.command.clone())
            .collect();
        assert!(cmds.contains(&"temper install".to_string()));
        assert!(cmds.contains(&"temper undo".to_string()));
        // no package direction when only config drifted
        assert!(!cmds
            .iter()
            .any(|c| c.contains("prune") || c.contains("reconcile")));
    }

    #[test]
    fn all_in_sync_yields_no_remediation() {
        let items = vec![f("copy", true), f("package", true)];
        assert!(remediations(&items).is_empty());
    }
}
