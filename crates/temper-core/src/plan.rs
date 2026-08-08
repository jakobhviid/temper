//! Build the ordered steps + assertions for a machine, then evaluate (drift) or
//! apply (install/update) them, with presence-gating (`when`/`needs`) and a
//! both-direction remediation summary. Step primitives: `copy`, `block`,
//! `setkey` (all backends), `exec`, `profile`, `sysfile`; assertions are
//! drift-only. Flows `install` / `update` / `prune` / `snapshot` / `restore` /
//! `adopt` / `reconcile` are all live.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Result};

use crate::journal::Journal;
use crate::manifest::{self, expand_tilde, Assert, Ignore, Machine, Step};
use crate::primitives::{self, Applied, CopyOpts, ExecOpts, FileState};
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
    /// What actually disagreed, when the check can say. A drifted `setkey`
    /// carries `want X, have Y` here — a bare "drifted" is what made a dconf
    /// formatting bug take a hand-audit to find.
    pub detail: Option<String>,
}

impl Finding {
    fn state(app: &str, kind: &'static str, target: String, state: FileState) -> Finding {
        Finding {
            app: app.to_string(),
            kind,
            target,
            ok: state.is_ok(),
            status: state.label().to_string(),
            detail: None,
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
            || self.status.starts_with("notice")
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
        let ks = primitives::setkey_state(sk, vars)?;
        let target = format!("{}:{}", sk.file.as_deref().unwrap_or(&sk.backend), sk.key);
        let mut f = Finding::state(app, "setkey", target, ks.state);
        f.detail = ks.values.map(|(want, have)| format!("want {want}, have {have}"));
        return Ok(Some(f));
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
            detail: None,
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
            detail: None,
        }));
    }
    if let (Some(sysfile), Some(to)) = (&step.sysfile, &step.to) {
        let state =
            primitives::sysfile_state(&home.join(sysfile), &expand_tilde(to), &sysfile_opts(step))?;
        return Ok(Some(Finding::state(app, "sysfile", to.clone(), state)));
    }
    Ok(None)
}

/// The root work this run will really do, split by **who** escalates.
///
/// `.0` — temper's own escalations (`sysfile`: temper shells out to `sudo install`).
/// These are direct children of temper, so they can spend a credential temper
/// acquired even where sudo keys its timestamp to the parent process.
///
/// `.1` — scripts that escalate for themselves (`exec` with `sudo = true`). Their
/// `sudo` has the script's shell as its parent, so under parent-keyed timestamps
/// they authenticate again no matter what temper did — see
/// [`crate::sudo::reusable_by_children`].
///
/// Both lists consult **reality, not the declaration**: a `sysfile` already in sync
/// and an `exec` whose `check` hook passes will not run, so they must not cost a
/// password. Asking for work that provably won't happen is the same defect as any
/// other count of intentions in this codebase.
fn root_steps(
    home: &Path,
    machine: &Machine,
    vars: &BTreeMap<String, String>,
    resolved: &Resolved,
    update: bool,
) -> Result<(Vec<String>, Vec<String>)> {
    let (mut own, mut scripts) = (Vec::new(), Vec::new());
    for (_, step) in &resolved.steps {
        if !is_step(step) {
            continue;
        }
        if update {
            match lifecycle(step) {
                "always" => {}
                "ensure" => {
                    if !ensure_should_apply(home, machine, step, vars)? {
                        continue;
                    }
                }
                _ => continue,
            }
        } else if lifecycle(step) == "manual" {
            continue;
        }
        if !matches!(gate_step(home, step), Gate::Apply) {
            continue;
        }

        if let (Some(sysfile), Some(to)) = (&step.sysfile, &step.to) {
            // In sync → nothing to write → no root needed. `sysfile_state` answers
            // this without privilege when the destination is readable, and degrades
            // to `Unavailable` when it isn't — which we treat as "might need root",
            // since not being able to look is not evidence of being in sync.
            let state = primitives::sysfile_state(
                &home.join(sysfile),
                &expand_tilde(to),
                &sysfile_opts(step),
            )?;
            if state != FileState::InSync {
                own.push(to.clone());
            }
            continue;
        }

        if step.exec.is_some() && step.sudo {
            // A passing `check` means the phase will skip the script entirely, so
            // its `sudo` never happens. Evaluated here and again in the phase: a
            // check is contractually read-only and instant, and a needless password
            // prompt costs the user more than a second probe costs the machine. If
            // the check can't be evaluated (a missing secret), assume root *is*
            // needed rather than promise a quiet run we can't deliver.
            if let Some(check) = &step.check {
                let opts = exec_opts(home, machine, step);
                if primitives::exec_missing_secret(&opts).is_none()
                    && primitives::exec_check(&home.join(check), &opts)?
                {
                    continue;
                }
            }
            scripts.push(step_parts(step).1);
        }
    }
    Ok((own, scripts))
}

/// Ask for root **once**, up front, for everything in this run that needs it — and
/// say only what this machine can actually deliver.
///
/// The keyboard is here now; it may not be in twenty minutes, and a prompt that
/// arrives mid-run has nowhere good to land. But the promise "nothing will stop to
/// prompt" is only true when the credential outlives temper's own process tree, so
/// it is no longer printed as a guarantee: temper states what it needs the password
/// for, and if a script won't be able to reuse it, says that plainly with the
/// remedy. Silent when nothing needs root, which is the common case.
fn acquire_root_once(casks: &[String], own: &[String], scripts: &[String]) {
    if casks.is_empty() && own.is_empty() && scripts.is_empty() {
        return;
    }
    // Who can actually spend a credential temper acquires? Only temper's own
    // escalations (`own`). A `sudo = true` script's sudo has the script's shell as
    // its parent — and Homebrew's has brew's Ruby process — so under parent-keyed
    // timestamps neither can, which makes `casks` no safer than `scripts` here.
    let beyond_temper: Vec<String> = casks.iter().chain(scripts).cloned().collect();

    // If a credential already exists we can learn the scope *before* spending a
    // prompt — and skip asking altogether when the only reason would be work that
    // cannot reuse it anyway.
    if own.is_empty() && crate::sudo::cached() && !crate::sudo::reusable_by_children() {
        warn_parent_scoped(&beyond_temper);
        return;
    }

    let mut what = Vec::new();
    if !casks.is_empty() {
        what.push(format!("{} package installer(s): {}", casks.len(), casks.join(", ")));
    }
    if !own.is_empty() {
        what.push(format!("{} file(s) written as root: {}", own.len(), own.join(", ")));
    }
    if !scripts.is_empty() {
        what.push(format!("{} script(s) that escalate: {}", scripts.len(), scripts.join(", ")));
    }
    // `what` is a bare description of the work; `sudo::acquire` frames it, because
    // only it knows which sentence applies — asking, or explaining why it can't.
    let got = crate::sudo::acquire(&what.join(" · "));
    // Now that a credential exists, find out whether anything outside temper's own
    // process tree could ever use it.
    if got && !beyond_temper.is_empty() && !crate::sudo::reusable_by_children() {
        warn_parent_scoped(&beyond_temper);
    }
}

/// Say — once, plainly, with the remedy — that this machine cannot deliver the one
/// prompt temper would otherwise imply. Covers both a script's own `sudo` and
/// Homebrew's: each runs sudo from *its* process, not temper's.
fn warn_parent_scoped(beyond_temper: &[String]) {
    eprintln!(
        "{} this machine's sudo keeps credentials per parent process, so temper's \
         password cannot be reused by {} — {} will ask again when reached. \
         `Defaults timestamp_type=tty` in sudoers is what makes one prompt possible.",
        crate::ui::yellow("⚠"),
        if beyond_temper.len() == 1 { "it" } else { "them" },
        beyond_temper.join(", ")
    );
}

/// Apply one step, giving an `exec` the terminal to itself.
///
/// Every other primitive is a file/key write that cannot talk to the user, so it
/// runs under the live region. `exec` is the escape hatch — arbitrary code that may
/// invoke `sudo`, polkit or PAM, all of which prompt on `/dev/tty` where the region
/// cannot see (or protect) them. So the region is cleared for its duration: the
/// prompt gets a clean line, stays on screen, and leaves no fused progress line
/// behind. See `ui::Checklist::suspend`.
fn apply_one(
    home: &Path,
    machine: &Machine,
    step: &Step,
    vars: &BTreeMap<String, String>,
    journal: &mut Journal,
    verbose: bool,
    cl: &crate::ui::Checklist,
) -> Result<Applied> {
    if step.exec.is_some() {
        return cl.suspend(|| {
            // The script, not the whole aligned row: this is a subordinate detail
            // line, not a second entry in the results list.
            let _notice = crate::ui::WaitNotice::new(&step_parts(step).1);
            apply_step(home, machine, step, vars, journal, verbose)
        });
    }
    apply_step(home, machine, step, vars, journal, verbose)
}

/// A step's `(kind, target)` — the two cells the progress region and the `✓` lines
/// render beside its app. Pure and cheap on purpose: unlike `step_finding` it
/// probes nothing, so naming a step costs nothing before we know whether it will
/// change anything. Kinds match `Finding::kind`, so the step phase and `drift`
/// speak of the same step by the same name.
fn step_parts(step: &Step) -> (&'static str, String) {
    if let (Some(_), Some(to)) = (&step.copy, &step.to) {
        return ("copy", to.clone());
    }
    if let (Some(_), Some(in_file)) = (&step.block, &step.in_file) {
        return ("block", in_file.clone());
    }
    if let Some(sk) = &step.setkey {
        return (
            "setkey",
            format!("{}:{}", sk.file.as_deref().unwrap_or(&sk.backend), sk.key),
        );
    }
    if let Some(exec) = &step.exec {
        return ("exec", exec.clone());
    }
    if let Some(profile) = &step.profile {
        return ("profile", profile.clone());
    }
    if let (Some(_), Some(to)) = (&step.sysfile, &step.to) {
        return ("sysfile", to.clone());
    }
    ("step", String::new())
}

/// The widest app and kind among the steps a phase will render, so its rows line
/// up from the first one printed. `18` caps the app column: one long bundle name
/// must not shove every path to the right, but the cap is set above the realistic
/// names (`desktop-overrides` is 17) — eliding a word to save one column costs more
/// legibility than it buys. Prefix `4` is `"  ✓ "`; the target (column 2) is what
/// gives way on a narrow terminal.
fn step_columns(rows: &[(String, &'static str)]) -> crate::ui::Columns {
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|(app, kind)| vec![app.clone(), kind.to_string(), String::new()])
        .collect();
    crate::ui::Columns::measure(&cells, 4, &[18, 0, 0], 2)
}

/// What resolves a finding of a given `kind`.
///
/// This registry exists because the recurring defect in this tool has been
/// shipping a *report* with no way to act on it: gext extras were reported for a
/// release before `prune` could remove them, and drift kept naming `temper
/// snapshot` for a release after that verb was renamed. Both are the same
/// mistake — a cell in the (state × direction × verb) matrix that nobody
/// visited. Declaring the answer here makes the omission a failing test instead
/// of something a user discovers.
///
/// A kind with no entry fails `every_emitted_kind_is_registered`. Writing
/// `NoVerb` is deliberately awkward: if you find yourself reaching for it, ask
/// first whether the verb simply hasn't been built yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// A command that resolves it. Checked against the real CLI verb list.
    Verb(&'static str),
    /// Nothing temper runs resolves it, with the reason shown to the user.
    NoVerb(&'static str),
}

/// Every `Finding.kind` temper emits, and what answers it.
pub const KIND_ANSWERS: &[(&str, &[Answer])] = &[
    // App-scope config: re-applied by a converge.
    ("copy", &[Answer::Verb("temper install")]),
    ("block", &[Answer::Verb("temper install")]),
    ("setkey", &[Answer::Verb("temper install")]),
    ("sysfile", &[Answer::Verb("temper install")]),
    ("exec", &[Answer::Verb("temper install")]),
    ("profile", &[Answer::NoVerb("a macOS profile is a manual System-Settings install")]),
    // Presence gates: reported for visibility.
    ("when", &[Answer::NoVerb("the step's app is absent — status only")]),
    ("needs", &[Answer::NoVerb("install the hard dependency the step names")]),
    // Packages: the four-branch fork.
    ("package", &[Answer::Verb("temper install --packages-only")]),
    ("package-extra", &[Answer::Verb("temper prune"), Answer::Verb("temper reconcile")]),
    ("rpm", &[Answer::Verb("temper install --packages-only")]),
    ("trust", &[Answer::Verb("temper install --packages-only")]),
    ("trust-extra", &[Answer::Verb("temper prune"), Answer::Verb("temper reconcile")]),
    // GNOME extensions: both directions, since 3.2.
    ("extension", &[Answer::Verb("temper install --packages-only")]),
    ("extension-extra", &[Answer::Verb("temper prune"), Answer::Verb("temper reconcile")]),
    // Desktop dconf.
    ("dconf-key", &[Answer::Verb("temper restore-gnome"), Answer::Verb("temper reconcile")]),
    ("dconf-extra", &[Answer::Verb("temper reconcile"), Answer::Verb("temper snapshot-gnome")]),
    ("dconf-uncaptured", &[Answer::Verb("temper snapshot-gnome")]),
    // Assertions are drift-only by definition: they report a condition.
    ("absent", &[Answer::NoVerb("resolve the condition yourself")]),
    ("contains-line", &[Answer::NoVerb("resolve the condition yourself")]),
    ("mode", &[Answer::NoVerb("resolve the condition yourself")]),
    ("executable-resolves", &[Answer::NoVerb("resolve the condition yourself")]),
    ("not-member", &[Answer::NoVerb("resolve the condition yourself")]),
    ("shell", &[Answer::NoVerb("resolve the condition yourself")]),
    ("json-semantic", &[Answer::NoVerb("resolve the condition yourself")]),
    ("unknown", &[Answer::NoVerb("an unrecognised assertion — fix the bundle")]),
];

/// Every distinct command any answer names — the set a CLI-side test checks
/// really exists, so a verb rename can never leave drift teaching a dead name.
pub fn answer_commands() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = KIND_ANSWERS
        .iter()
        .flat_map(|(_, a)| a.iter())
        .filter_map(|a| match a {
            Answer::Verb(c) => Some(*c),
            Answer::NoVerb(_) => None,
        })
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// A suggested next command to resolve drift — the "what to run next" hand-off
/// RIS emits at the moment of detection. Each is a human label + the exact,
/// copy-pasteable invocation.
pub struct Remediation {
    pub label: String,
    pub command: String,
}

/// Both-direction remediations for a drift report (RIS's four-branch package
/// fork + a config line + the dconf pair). Machine→spec: `install-missing` /
/// `prune` / re-`install` / `restore`. Spec←machine: `reconcile` / `snapshot`.
/// Plus `undo` to revert. Empty when nothing is out of sync.
///
/// Wholesale absorption is deliberately **not** offered here as its own line:
/// `reconcile` already covers it (interactively, or with
/// `--current-state-wins`), so there is nothing to name separately.
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
    // dconf splits by direction: a `missing`/`changed` key can be pushed back
    // out with `restore`; an `extra` only ever moves spec←machine.
    let dconf_stale = drifted(&["dconf-key"]);
    let dconf_capture = drifted(&["dconf-key", "dconf-extra", "dconf-uncaptured"]);
    let config_drift = items.iter().any(|f| {
        !f.ok
            && ![
                "package",
                "package-extra",
                "extension",
                "rpm",
                "trust",
                "trust-extra",
                "dconf-key",
                "dconf-extra",
                "dconf-uncaptured",
                // No command re-applies this one — it is a hand edit to a shared
                // bundle, and the finding says so itself. Suggesting `install`
                // would be a lie.
                "extension-extra",
            ]
            .contains(&f.kind)
            // Assertions are drift-ONLY checks, never a converge action (see
            // `drift.rs`): `install` structurally cannot satisfy one. A staged
            // ostree deployment clears on reboot, a group membership by logging
            // out — offering `install` sent people to re-run a converge that was
            // never going to help.
            && !drift::is_assert_kind(f.kind)
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
    if drifted(&["extension-extra"]) {
        push(
            &mut out,
            "uninstall the GNOME extensions not in the spec (asks first)",
            "temper prune",
        );
        // The third branch, which only exists since a machine gained its own
        // `extensions` list: "yes, I want it — on this machine".
        push(
            &mut out,
            "declare them for this machine instead (per extension)",
            "temper reconcile",
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
    if dconf_stale {
        push(
            &mut out,
            "reload the desktop snapshot, clobbering live tweaks (asks first)",
            "temper restore-gnome",
        );
    }
    // Spec ← machine (absorb the machine's state into the spec).
    if missing_pkg || extra_pkg || trust_extra || trust_gap || dconf_capture {
        let label = if dconf_capture && (missing_pkg || extra_pkg || trust_extra || trust_gap) {
            "interactively add extras / drop missing entries (packages, tap-trust, desktop keys)"
        } else if dconf_capture {
            "interactively absorb changed desktop keys, per section"
        } else {
            "interactively add extras / drop missing entries (packages + tap-trust)"
        };
        push(&mut out, label, "temper reconcile");
    }
    if dconf_capture {
        push(
            &mut out,
            "capture the whole desktop subtree into the spec instead",
            "temper snapshot-gnome",
        );
    }
    // A failed assertion has no command: it reports a condition you resolve
    // yourself (reboot, log out, edit a file temper doesn't own). Saying so is
    // better than silence AND better than naming a verb that can't work.
    if items.iter().any(|f| !f.ok && drift::is_assert_kind(f.kind)) {
        push(
            &mut out,
            "an assertion failed — resolve it yourself (no verb applies); \
             re-run drift to confirm",
            "temper drift",
        );
    }
    // Config drift: re-apply, or revert the last run.
    if config_drift {
        push(
            &mut out,
            "re-apply the drifted config steps above (copy/block/setkey/sysfile/exec)",
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
                    detail: None,
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
                    detail: None,
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
        // A `notice` assertion reports a STATE, not a defect. A failing one is
        // still surfaced, but as information: it stays out of the out-of-sync
        // count and gets no remediation, because there is nothing to fix — a
        // staged system update is waiting for a reboot, not broken.
        let notice = assert.severity.as_deref() == Some("notice");
        let (ok, status) = match (notice, ok) {
            (true, false) => (
                true,
                format!(
                    "notice — {}",
                    assert.message.as_deref().unwrap_or(status.as_str())
                ),
            ),
            _ => (ok, status),
        };
        findings.push(Finding {
            app: app.clone(),
            kind: drift::kind(assert),
            target: drift::target(assert),
            ok,
            status,
            detail: None,
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
                detail: None,
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
                detail: None,
            });
        }
        for (m, name) in providers::brew_extras(&effective, ignore)? {
            findings.push(Finding {
                app: "packages".into(),
                kind: "package-extra",
                target: format!("{} {}", m.as_str(), name),
                ok: false,
                status: "extra".into(),
                detail: None,
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
                    detail: None,
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
                    detail: None,
                });
            }
        }
    }

    // GNOME extensions + rpm-ostree (Linux; inert where their CLIs are absent).
    let effective_ext = providers::effective_extensions(home, machine)?;
    for uuid in providers::gext_missing(&effective_ext) {
        findings.push(Finding {
            app: "extensions".into(),
            kind: "extension",
            target: uuid,
            ok: false,
            status: "missing".into(),
            detail: None,
        });
    }
    // The extras direction, which every other manager already reported. Carries
    // its remedy in the status: there is no single command for it, because
    // `extensions` lives in a shared bundle that only a human should edit.
    for uuid in providers::gext_extras(&effective_ext, ignore) {
        findings.push(Finding {
            app: "extensions".into(),
            kind: "extension-extra",
            target: uuid,
            ok: false,
            status: "extra — declare in a bundle or [ignore].gext".into(),
            detail: None,
        });
    }
    for pkg in providers::rpm_missing(&providers::effective_rpm(home, machine)?) {
        findings.push(Finding {
            app: "rpm".into(),
            kind: "rpm",
            target: pkg,
            ok: false,
            status: "missing".into(),
            detail: None,
        });
    }

    // Whole-desktop dconf snapshots (machine scope): the captured file versus a
    // live dump, both filtered through the same `strip`. Grouped per snapshot so
    // a narrow subtree (`…/shell/extensions/`) reads as its own section.
    // Degraded, not failed, on a host without dconf (a Mac).
    for snap in &machine.dconf {
        let group = format!("dconf/{}", snap.name());
        match crate::dconf::snapshot_state(home, snap)? {
            crate::dconf::SnapshotState::NoDconf => {}
            crate::dconf::SnapshotState::Uncaptured => findings.push(Finding {
                app: group,
                kind: "dconf-uncaptured",
                target: snap.file.clone(),
                ok: false,
                status: "never captured".into(),
                detail: None,
            }),
            crate::dconf::SnapshotState::Diffs(diffs) => {
                for d in diffs {
                    findings.push(Finding {
                        app: group.clone(),
                        kind: if d.status() == "extra" {
                            "dconf-extra"
                        } else {
                            "dconf-key"
                        },
                        target: d.id(),
                        ok: false,
                        status: d.status().into(),
                        detail: None,
                    });
                }
            }
        }
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
) -> Result<Applied> {
    if let (Some(copy), Some(to)) = (&step.copy, &step.to) {
        let changed = primitives::copy_apply(
            &home.join(copy),
            &expand_tilde(to),
            &copy_opts(step, vars),
            journal,
        )?;
        return Ok(Applied::from_changed(changed));
    }
    if let (Some(block), Some(in_file)) = (&step.block, &step.in_file) {
        let marker = step.marker.as_deref().unwrap_or("block");
        let changed =
            primitives::block_apply(&home.join(block), &expand_tilde(in_file), marker, journal)?;
        return Ok(Applied::from_changed(changed));
    }
    if let Some(sk) = &step.setkey {
        return Ok(Applied::from_changed(primitives::setkey_apply(
            sk, vars, journal,
        )?));
    }
    if let Some(exec) = &step.exec {
        let opts = exec_opts(home, machine, step);
        let check = step.check.as_ref().map(|c| home.join(c));
        return primitives::exec_apply(&home.join(exec), check.as_deref(), &opts, verbose);
    }
    if let Some(profile) = &step.profile {
        return Ok(Applied::from_changed(primitives::profile_apply(
            &home.join(profile),
        )?));
    }
    if let (Some(sysfile), Some(to)) = (&step.sysfile, &step.to) {
        let changed = primitives::sysfile_apply(
            &home.join(sysfile),
            &expand_tilde(to),
            &sysfile_opts(step),
        )?;
        return Ok(Applied::from_changed(changed));
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
    /// Steps that RAN but whose effect temper can't observe (a checkless
    /// `exec`). Reported apart from `steps_changed`, which is a measured claim.
    pub steps_ran: usize,
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
    // Resolved here rather than in phase 2, so a `sysfile`/`sudo = true` step's
    // password request joins the package one in a single ask before any work starts.
    let resolved = resolve(home, machine)?;
    let _sudo = if dry_run {
        None
    } else {
        let (own_root, script_root) = root_steps(home, machine, vars, &resolved, false)?;
        acquire_root_once(
            &providers::casks_needing_root(&effective),
            &own_root,
            &script_root,
        );
        Some(crate::sudo::keep_alive())
    };
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
            steps_ran: 0,
            steps_total: 0,
            reboot,
            skipped: Vec::new(),
        });
    }

    // Phase 2 — config steps (`resolved` was needed above, for the root ask).
    let mut journal = Journal::begin();
    let (mut changed, mut total, mut ran) = (0usize, 0usize, 0usize);
    let mut skipped = Vec::new();
    // Candidates are known before any of them runs, so the phase has an honest
    // denominator. A dry-run reports rather than applies — no live region for it.
    let planned: Vec<(String, &'static str)> = resolved
        .steps
        .iter()
        .filter(|(_, s)| is_step(s) && lifecycle(s) != "manual")
        .map(|(app, s)| (app.clone(), step_parts(s).0))
        .collect();
    let cols = step_columns(&planned);
    let cl = crate::ui::Checklist::new(
        if dry_run { 0 } else { planned.len() },
        "config",
        verbose,
    );
    for (app, step) in &resolved.steps {
        // `manual` steps are never run by an automated flow (e.g. speaker-eq's
        // interactive picker) — only when explicitly invoked.
        if !is_step(step) || lifecycle(step) == "manual" {
            continue;
        }
        let (kind, target) = step_parts(step);
        let label = cols.row(&[app, kind, &target]);
        cl.start(&label);
        // Presence gate — skip loudly when absent, error on a failed `needs`.
        match gate_step(home, step) {
            Gate::Skip(desc) => {
                cl.skipped(&label, &format!("{desc} absent"));
                skipped.push(desc);
                continue;
            }
            Gate::Require(desc) => bail!("step in `{app}` needs {desc}, which is absent"),
            Gate::Apply => {}
        }
        total += 1;
        if dry_run {
            // Name the steps behind the count: "would apply 4 of 26" never said
            // *which* four, so the one thing a dry run exists to tell you was the
            // one thing it withheld.
            if step_would_change(home, machine, step, vars)? {
                changed += 1;
                cl.noted(&format!("would apply {label}"));
            }
        } else {
            let started = std::time::Instant::now();
            let did = apply_one(
                home,
                machine,
                step,
                vars,
                &mut journal,
                verbose,
                &cl,
            )?;
            match did {
                Applied::Changed => {
                    changed += 1;
                    // The elapsed time accounts for a pause the reader just sat
                    // through (and is dropped for the quick ones).
                    cl.done_after(&label, started.elapsed());
                }
                // A checkless `exec` ran, and temper cannot tell whether it did
                // anything — say "ran", never "changed", and keep it out of the
                // changed count so a converged machine can report zero.
                Applied::Ran => {
                    ran += 1;
                    cl.done_after(&format!("{label}  (ran; no drift-check)"), started.elapsed());
                }
                Applied::Unchanged => cl.unchanged(),
            }
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
        steps_ran: ran,
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
    /// User-scope GNOME extensions to uninstall — the prune counterpart of an
    /// `extension-extra`. Without this, gext extras were the one drift no verb
    /// could clear: reported forever, with only a hand edit to answer them.
    pub extensions: Vec<String>,
}

impl PrunePlan {
    pub fn is_empty(&self) -> bool {
        self.packages.is_empty() && self.untrust.is_empty() && self.extensions.is_empty()
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
    // User-scope extensions no bundle declares — the same set drift reports as
    // `extension-extra`, so what drift names is exactly what prune offers.
    let extensions = providers::gext_extras(&providers::effective_extensions(home, machine)?, ignore);
    Ok(PrunePlan {
        packages,
        untrust,
        extensions,
    })
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
    if !plan.extensions.is_empty() {
        providers::gext_uninstall(&plan.extensions)?;
    }
    Ok(())
}

/// Snapshot: capture each declared `[[machine.dconf]]` subtree (filtered) into
/// its file — the spec←machine half of the capture/restore pair, and the
/// wholesale sibling of a per-key `reconcile`. A **recurring** verb: it is the
/// only way to update a snapshot wholesale.
///
/// Errors where `dconf` is absent rather than silently writing nothing.
/// Journaled, so `undo` reverts it.
pub fn run_snapshot(home: &Path, machine: &Machine) -> Result<Vec<std::path::PathBuf>> {
    if machine.dconf.is_empty() {
        return Ok(Vec::new());
    }
    if crate::primitives::which("dconf").is_none() {
        bail!("dconf not found — cannot capture a dconf snapshot on this host");
    }
    let mut journal = Journal::begin();
    let written = crate::dconf::capture(home, machine, &mut journal)?;
    journal.commit()?;
    Ok(written)
}

/// Restore: load each declared dconf snapshot back into live dconf. The CLI
/// confirms first — this clobbers live desktop state (never run by `update`).
/// Journaled per subtree, so `undo` reverts it.
pub fn run_restore(home: &Path, machine: &Machine, dry_run: bool) -> Result<Vec<std::path::PathBuf>> {
    crate::dconf::restore(home, machine, dry_run)
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
    let resolved = resolve(home, machine)?;
    let (own_root, script_root) = root_steps(home, machine, vars, &resolved, true)?;
    acquire_root_once(
        &providers::casks_needing_root(&effective),
        &own_root,
        &script_root,
    );
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

    let mut journal = Journal::begin();
    let (mut changed, mut total, mut ran) = (0usize, 0usize, 0usize);
    let mut skipped = Vec::new();
    // `always` + `ensure` are what an update re-applies; `ensure` is filtered
    // again inside the loop (it needs a probe), so this is an upper bound — the
    // counter can finish short of its total, which beats a total that grows.
    let planned: Vec<(String, &'static str)> = resolved
        .steps
        .iter()
        .filter(|(_, s)| is_step(s) && matches!(lifecycle(s), "always" | "ensure"))
        .map(|(app, s)| (app.clone(), step_parts(s).0))
        .collect();
    let cols = step_columns(&planned);
    let cl = crate::ui::Checklist::new(planned.len(), "config", verbose);
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
        let (kind, target) = step_parts(step);
        let label = cols.row(&[app, kind, &target]);
        cl.start(&label);
        // Presence gate — same as install.
        match gate_step(home, step) {
            Gate::Skip(desc) => {
                cl.skipped(&label, &format!("{desc} absent"));
                skipped.push(desc);
                continue;
            }
            Gate::Require(desc) => bail!("step in `{app}` needs {desc}, which is absent"),
            Gate::Apply => {}
        }
        total += 1;
        let started = std::time::Instant::now();
        let did = apply_one(
            home,
            machine,
            step,
            vars,
            &mut journal,
            verbose,
            &cl,
        )?;
        match did {
            Applied::Changed => {
                changed += 1;
                cl.done_after(&label, started.elapsed());
            }
            Applied::Ran => {
                ran += 1;
                cl.done_after(&format!("{label}  (ran; no drift-check)"), started.elapsed());
            }
            Applied::Unchanged => cl.unchanged(),
        }
    }
    cl.finish();
    journal.commit()?;
    Ok(InstallReport {
        packages: effective.len(),
        upgraded,
        steps_changed: changed,
        steps_ran: ran,
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
            detail: None,
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
                                                                 // never a machine name baked into a suggested command
        assert!(!cmds
            .iter()
            .any(|c| c.split_whitespace().count() > 3 && !c.contains("--")));
        // Seeding a machine is `init`'s job, never a drift remedy — offering it
        // here would tell you to re-seed a spec you already have.
        assert!(!cmds.iter().any(|c| c.contains("dump") || c.contains("init")));
    }

    /// The registry must cover every kind the source actually emits. This is the
    /// check that would have caught gext extras shipping with no way to act on
    /// them: adding a `kind` without an answer now fails here.
    #[test]
    fn every_emitted_kind_is_registered() {
        let plan_src = include_str!("plan.rs");
        let drift_src = include_str!("drift.rs");
        let mut emitted: Vec<String> = Vec::new();
        // `kind: "…"` literals and the `Finding::state(app, "…")` helper.
        for src in [plan_src, drift_src] {
            for pat in ["kind: \"", "Finding::state(app, \""] {
                // `match_indices` yields char-boundary byte offsets, and the
                // patterns are ASCII — slicing by hand tripped over a `…` in a
                // nearby string literal.
                for (i, _) in src.match_indices(pat) {
                    let tail = &src[i + pat.len()..];
                    if let Some(end) = tail.find('"') {
                        let k = &tail[..end];
                        // A real kind is lowercase-and-hyphens; this skips the
                        // prose in comments that describes the pattern itself.
                        if !k.is_empty()
                            && k.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                        {
                            emitted.push(k.to_string());
                        }
                    }
                }
            }
        }
        // Kinds the scrape structurally cannot see, listed explicitly rather than
        // by making the pattern-matching cleverer (which would rot):
        //   - assertion kinds come from `drift::kind`, which returns bare literals;
        //   - the dconf pair is chosen by a conditional, not a `kind: "…"` literal.
        for k in [
            "dconf-key",
            "dconf-extra",
            "absent",
            "contains-line",
            "mode",
            "executable-resolves",
            "not-member",
            "shell",
            "json-semantic",
            "unknown",
        ] {
            emitted.push(k.to_string());
        }
        emitted.sort();
        emitted.dedup();
        for k in &emitted {
            assert!(
                KIND_ANSWERS.iter().any(|(kind, _)| kind == k),
                "finding kind `{k}` has no entry in KIND_ANSWERS — say what resolves \
                 it (or that nothing does) before shipping the report"
            );
        }
        // …and the reverse: an entry for a kind nothing emits is dead config that
        // will quietly stop matching reality.
        for (kind, _) in KIND_ANSWERS {
            assert!(
                emitted.iter().any(|k| k == kind),
                "KIND_ANSWERS lists `{kind}`, which nothing emits any more — delete it"
            );
        }
        assert!(emitted.len() >= 15, "kind scrape found too few: {emitted:?}");
        // Honest limit: this reads the source for literal `kind: "…"`, so a kind
        // built from a variable or const is invisible to it. That has not
        // happened yet; if it does, the fix is to make `kind` a closed enum and
        // let the compiler enforce this instead of a scrape.
    }

    /// Every kind the registry answers with a verb must actually produce that
    /// command from `remediations`. Catches an answer that was declared but
    /// never wired, and one whose wiring drifted from its declaration.
    #[test]
    fn registered_verbs_are_actually_emitted() {
        for (kind, answers) in KIND_ANSWERS {
            let wanted: Vec<&str> = answers
                .iter()
                .filter_map(|a| match a {
                    Answer::Verb(c) => Some(*c),
                    Answer::NoVerb(_) => None,
                })
                .collect();
            if wanted.is_empty() {
                continue;
            }
            let items = vec![f(kind, false)];
            let got: Vec<String> = remediations(&items)
                .iter()
                .map(|r| r.command.clone())
                .collect();
            for w in wanted {
                assert!(
                    got.contains(&w.to_string()),
                    "kind `{kind}` claims `{w}` resolves it, but drift never offers \
                     that command (got {got:?})"
                );
            }
        }
    }

    #[test]
    fn dconf_drift_offers_both_directions() {
        let items = vec![f("dconf-key", false)];
        let cmds: Vec<String> = remediations(&items)
            .iter()
            .map(|r| r.command.clone())
            .collect();
        assert!(cmds.contains(&"temper restore-gnome".to_string())); // spec → machine
        assert!(cmds.contains(&"temper reconcile".to_string())); // spec ← machine, per key
        assert!(cmds.contains(&"temper snapshot-gnome".to_string())); // spec ← machine, wholesale
                                                                // dconf drift is not config drift — `install` never reloads a snapshot.
        assert!(!cmds.contains(&"temper install".to_string()));
    }

    #[test]
    fn a_live_only_dconf_key_never_offers_restore() {
        // An `extra` exists only on the machine; there is nothing in the spec to
        // push back out, so restore would silently drop it.
        let items = vec![f("dconf-extra", false)];
        let cmds: Vec<String> = remediations(&items)
            .iter()
            .map(|r| r.command.clone())
            .collect();
        assert!(!cmds.contains(&"temper restore-gnome".to_string()));
        assert!(cmds.contains(&"temper reconcile".to_string()));
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

#[cfg(test)]
mod root_step_tests {
    use super::*;

    /// A home with one bundle, so `resolve` can be driven from real TOML rather
    /// than hand-built structs (the parser is part of what's being tested).
    fn home_with(bundle: &str) -> (tempfile::TempDir, Machine) {
        let d = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(d.path().join("apps")).unwrap();
        std::fs::write(d.path().join("apps/a.toml"), bundle).unwrap();
        std::fs::write(
            d.path().join("temper.toml"),
            format!(
                "[[machine]]\nname = \"t\"\nos = \"{}\"\napps = [\"a\"]\n",
                crate::machine::current_os()
            ),
        )
        .unwrap();
        let ft = manifest::load_fleet(d.path()).unwrap();
        let m = crate::machine::resolve(&ft, Some("t")).unwrap();
        (d, m)
    }

    /// (temper's own escalations, scripts that escalate for themselves)
    fn root_targets(bundle: &str) -> (Vec<String>, Vec<String>) {
        let (d, m) = home_with(bundle);
        let resolved = resolve(d.path(), &m).unwrap();
        let vars = BTreeMap::new();
        root_steps(d.path(), &m, &vars, &resolved, false).unwrap()
    }

    #[test]
    fn sysfile_and_declared_sudo_are_split_by_who_escalates() {
        // `sysfile` is temper's own escalation (it can spend temper's credential);
        // a `sudo = true` exec escalates for itself (under parent-keyed timestamps
        // it cannot), so the two are tracked apart and reported differently.
        let (own, scripts) = root_targets(
            "[[step]]\nsysfile = \"assets/x\"\nto = \"/etc/x.conf\"\n\n\
             [[step]]\nexec = \"assets/needs-root.sh\"\nsudo = true\n",
        );
        assert_eq!(own, vec!["/etc/x.conf"]);
        assert_eq!(scripts, vec!["assets/needs-root.sh"]);
    }

    #[test]
    fn a_script_whose_check_passes_costs_no_password() {
        // A passing `check` means the phase skips the script, so its `sudo` never
        // happens — asking for it would be charging the user for work that provably
        // will not occur.
        let d = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(d.path().join("apps")).unwrap();
        std::fs::create_dir_all(d.path().join("assets")).unwrap();
        std::fs::write(d.path().join("assets/in-sync.sh"), "exit 0\n").unwrap();
        std::fs::write(
            d.path().join("apps/a.toml"),
            "[[step]]\nexec = \"assets/needs-root.sh\"\nsudo = true\n\
             check = \"assets/in-sync.sh\"\n",
        )
        .unwrap();
        std::fs::write(
            d.path().join("temper.toml"),
            format!(
                "[[machine]]\nname = \"t\"\nos = \"{}\"\napps = [\"a\"]\n",
                crate::machine::current_os()
            ),
        )
        .unwrap();
        let ft = manifest::load_fleet(d.path()).unwrap();
        let m = crate::machine::resolve(&ft, Some("t")).unwrap();
        let resolved = resolve(d.path(), &m).unwrap();
        let (own, scripts) = root_steps(d.path(), &m, &BTreeMap::new(), &resolved, false).unwrap();
        assert!(own.is_empty() && scripts.is_empty(), "{own:?} {scripts:?}");
    }

    #[test]
    fn a_sysfile_already_in_sync_costs_no_password() {
        // The converged case, and the whole point of consulting reality: temper can
        // see the file already matches — without privilege, since the destination is
        // readable — so there is nothing to write and nothing to ask for.
        let d = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(d.path().join("apps")).unwrap();
        std::fs::create_dir_all(d.path().join("assets")).unwrap();
        std::fs::write(d.path().join("assets/x"), "managed\n").unwrap();
        let dest = d.path().join("dest.conf");
        std::fs::write(&dest, "managed\n").unwrap();
        std::fs::write(
            d.path().join("apps/a.toml"),
            format!(
                "[[step]]\nsysfile = \"assets/x\"\nto = \"{}\"\n",
                dest.display()
            ),
        )
        .unwrap();
        std::fs::write(
            d.path().join("temper.toml"),
            format!(
                "[[machine]]\nname = \"t\"\nos = \"{}\"\napps = [\"a\"]\n",
                crate::machine::current_os()
            ),
        )
        .unwrap();
        let ft = manifest::load_fleet(d.path()).unwrap();
        let m = crate::machine::resolve(&ft, Some("t")).unwrap();
        let resolved = resolve(d.path(), &m).unwrap();
        let (own, scripts) = root_steps(d.path(), &m, &BTreeMap::new(), &resolved, false).unwrap();
        assert!(own.is_empty() && scripts.is_empty(), "{own:?} {scripts:?}");

        // …and it *is* asked for when the file really differs.
        std::fs::write(&dest, "drifted\n").unwrap();
        let (own, _) = root_steps(d.path(), &m, &BTreeMap::new(), &resolved, false).unwrap();
        assert_eq!(own.len(), 1, "{own:?}");
    }

    #[test]
    fn a_run_with_nothing_to_escalate_never_asks() {
        // The promise is no prompt at all on a run that needs no root — so a plain
        // `exec` and a `copy` must contribute nothing.
        let (own, scripts) = root_targets(
            "[[step]]\nexec = \"assets/plain.sh\"\n\n\
             [[step]]\ncopy = \"assets/f\"\nto = \"~/.f\"\n",
        );
        assert!(own.is_empty() && scripts.is_empty(), "{own:?} {scripts:?}");
    }

    #[test]
    fn a_step_that_will_be_skipped_costs_no_password() {
        // Gated out by `when`, and `manual` — neither will run, so neither may
        // trigger a prompt for root it never needs.
        let (own, scripts) = root_targets(
            "[[step]]\nexec = \"assets/gated.sh\"\nsudo = true\n\
             when = { binary = \"definitely-not-installed-anywhere\" }\n\n\
             [[step]]\nexec = \"assets/manual.sh\"\nsudo = true\nrun = \"manual\"\n",
        );
        assert!(own.is_empty() && scripts.is_empty(), "{own:?} {scripts:?}");
    }
}
