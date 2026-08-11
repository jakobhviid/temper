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
    /// tool is absent (`unavailable`), or an `exec` with no drift hook. The drift
    /// renderer surfaces these separately so they neither read as green "in sync"
    /// nor as red drift.
    ///
    /// `profile` is deliberately absent from this list even though its apply is a
    /// GUI step: its presence is checked, so an installed one has earned a green
    /// "in sync" and a removed one is real drift. The case temper cannot evaluate
    /// reaches here through the `unavailable` status instead, like any other
    /// backend whose tool is missing.
    pub fn status_only(&self) -> bool {
        self.kind == "when"
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
        // Presence is checkable without MDM or root — see `primitives::profile_state`.
        // Only *installing* needs the GUI, which is why `install` is the answer to a
        // profile finding: it opens System Settings.
        let st = primitives::profile_state(&home.join(profile))?;
        return Ok(Some(Finding {
            app: app.to_string(),
            kind: "profile",
            target: profile.clone(),
            ok: st.state.is_ok(),
            status: st.state.label().to_string(),
            detail: st.detail,
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
        crate::ui::yellow(crate::ui::g_warn()),
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
/// The paths this machine's spec currently deploys, with the bytes it intends
/// to leave there — `copy` and `sysfile` only.
///
/// `block` is included, but its residue is the *region* rather than the file:
/// the file belongs to the user — a `.zshrc` — so retiring one is an edit. That
/// is why an entry is keyed by `(file, marker)` and hashed over the region body,
/// and why two blocks in one file are two entries.
fn deployed_paths(home: &Path, machine: &Machine) -> Result<crate::ledger::Ledger> {
    let resolved = resolve(home, machine)?;
    let mut out = crate::ledger::Ledger::new();
    for (_, step) in &resolved.steps {
        let (kind, target) = step_parts(step);
        if !matches!(kind, "copy" | "sysfile" | "block") {
            continue;
        }
        let src = step
            .copy
            .as_ref()
            .or(step.sysfile.as_ref())
            .or(step.block.as_ref());
        let Some(src) = src else { continue };
        // The hash of what the spec WOULD leave there. Reading the source rather
        // than the target keeps this honest on a machine that has drifted: the
        // ledger records what temper deploys, not what happens to be there.
        //
        // An unreadable source (a renamed asset, a partial sync) still yields an
        // entry, with an empty hash. Skipping it dropped the path from the
        // DECLARED set, which is the same list `residue` diffs against — so a
        // file the spec still declares became residue and `prune` deleted it.
        // An empty hash can never match, so the entry is reportable and never
        // removable.
        let bytes = std::fs::read(home.join(src)).ok();
        // A block's identity is (file, marker) and its content is the region
        // body, so it is hashed and keyed differently from a whole-file deploy.
        let (hash, marker) = if kind == "block" {
            let Some(marker) = step.marker.clone() else {
                continue;
            };
            let hash = bytes.as_ref().map(|b| {
                // `trim_matches`, not `trim_end_matches`: the hash has to be of
                // the same bytes `block_removed` will extract when prune checks
                // it, and that trims BOTH ends. A source whose content began
                // with a blank line hashed differently from itself, so its
                // region was never removable and drift called it "edited since
                // temper deployed it" — a lie about untouched content.
                let body = String::from_utf8_lossy(b).trim_matches('\n').to_string();
                blake3::hash(body.as_bytes()).to_hex().to_string()
            });
            (hash, Some(marker))
        } else {
            (
                bytes
                    .as_ref()
                    .map(|b| blake3::hash(b).to_hex().to_string()),
                None,
            )
        };
        out.insert(
            crate::ledger::Deployed::key(&target, marker.as_deref()),
            crate::ledger::Deployed {
                hash: hash.unwrap_or_default(),
                kind: kind.to_string(),
                path: target,
                marker,
            },
        );
    }
    Ok(out)
}

/// What a package-only converge does that `undo` cannot take back.
///
/// The package installs themselves are journaled. These three are not, and each
/// is recorded as `revertible: No` in `interface.rs` — so reporting an empty
/// list here contradicted the tool's own capability table.
fn packages_only_unrevertible(
    brew_trust: &[String],
    home: &Path,
    machine: &Machine,
) -> Vec<String> {
    // Only what this run would actually CHANGE. Keying off "does the spec
    // declare any" warned about reverting work that is not going to happen —
    // a converged machine declaring three taps was told its tap-trust could not
    // be taken back, while the run was about to touch nothing. That is the same
    // defect as listing an in-sync `sysfile` as unrevertible, which the step
    // path is careful about and this one was not.
    //
    // The reasons come from `interface::PROVIDERS`, not from here: that table is
    // where a provider's revertibility is declared, and writing the same thing
    // twice is how a capability table and its code stop agreeing.
    let untrusted = crate::providers::trusted_taps()
        .ok()
        .flatten()
        .map(|t| brew_trust.iter().any(|d| !t.contains(d)))
        .unwrap_or(false);
    let remotes_to_add = !crate::providers::remotes_missing(
        &crate::providers::effective_remotes(home, machine).unwrap_or_default(),
    )
    .is_empty();
    let switches_to_flip = !crate::providers::gext_enable_drift(
        &crate::providers::effective_extension_specs(home, machine).unwrap_or_default(),
    )
    .is_empty();
    let touched: [(&str, bool); 3] = [
        ("brew-trust", untrusted),
        ("flatpak-remote", remotes_to_add),
        ("gnome-extensions", switches_to_flip),
    ];
    touched
        .iter()
        .filter(|(_, would_touch)| *would_touch)
        .filter_map(|(name, _)| {
            crate::interface::unrevertible_reason(name).map(|why| format!("{name} — {why}"))
        })
        .collect()
}

/// Why a step's effect cannot be reverted, if it cannot.
///
/// `undo` covers file and key writes, and (since installs are journaled) package
/// converges. What it does not cover is small and specific, and each case has a
/// real reason rather than an inherited one. The point of naming them is
/// AGENTS.md question 7: a run whose only changes were unrevertible currently
/// reverts nothing while reporting success, and the user finds out afterwards.
fn unrevertible_reason(step: &Step) -> Option<&'static str> {
    if step.exec.is_some() {
        return Some("an `exec` runs arbitrary code; temper cannot know what to undo");
    }
    if step.sysfile.is_some() {
        return Some("`sysfile` writes root-owned state outside the journal");
    }
    if let Some(sk) = &step.setkey {
        if sk.backend == "defaults" {
            return Some("`defaults read` loses the value's type, so an undo could not rewrite it faithfully");
        }
    }
    if step.profile.is_some() {
        return Some("a profile is approved in System Settings; temper cannot remove it");
    }
    None
}

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
/// Which side of the matrix a kind reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detects {
    /// Declared, not present.
    Missing,
    /// Present, not declared.
    Extra,
    /// Declared and present, but not equal.
    Differs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// A command that resolves it. Checked against the real CLI verb list.
    Verb(&'static str),
    /// No verb — so name the FILE a human edits, and why. This is what drift
    /// prints, and it is the difference between "you cannot fix this" and "the
    /// declaration is in apps/gnome.toml".
    Hand {
        file: &'static str,
        why: &'static str,
    },
    /// The cell is meaningless for this kind, and why.
    NA(&'static str),
}

/// One `Finding.kind`, and what answers it **in both directions**.
///
/// The two fields are the point. A finding always admits two resolutions —
/// change the machine, or change the spec — and the old registry was a flat bag
/// of verbs per kind, which cannot express that a direction is missing. A bag
/// has no shape to violate, so `extension → [install]` passed every test written
/// to catch exactly this, and `drift` went on advising `temper reconcile` for a
/// missing extension that reconcile had no code path to touch.
///
/// Writing `Hand` or `NA` is deliberately awkward: if you find yourself reaching
/// for one, ask first whether the verb simply hasn't been built yet.
#[derive(Debug, Clone, Copy)]
pub struct KindSpec {
    pub name: &'static str,
    pub detects: Detects,
    /// Change the machine to match the spec. Non-empty: a kind must say what
    /// this direction is, even if the answer is `NA`.
    pub converge: &'static [Answer],
    /// Change the spec to match the machine. Non-empty, same rule.
    ///
    /// A slice, not a single answer, because a direction can legitimately have
    /// more than one verb — a drifted desktop key is absorbed per-key by
    /// `reconcile` or wholesale by `snapshot-dconf`. What must never be empty is
    /// the *direction*; that is the omission this registry exists to make
    /// impossible.
    pub absorb: &'static [Answer],
}

const HAND_BUNDLE: &str = "apps/<bundle>.toml";

/// Every `Finding.kind` temper emits, and what answers each direction.
pub const KIND_ANSWERS: &[KindSpec] = &[
    // ---- App-scope config: converged by `install`. -------------------------
    // Absorbing an edited deployed file back into the folder is a verb temper
    // does not have; `adopt` is packages-only. Named here rather than left
    // blank, so the gap is visible instead of implied.
    KindSpec {
        name: "copy",
        detects: Detects::Differs,
        converge: &[Answer::Verb("temper install")],
        absorb: &[Answer::Hand {
            file: "the step's source file in the temper folder",
            why: "no verb captures an edited deployed file back into the folder",
        }],
    },
    KindSpec {
        name: "block",
        detects: Detects::Differs,
        converge: &[Answer::Verb("temper install")],
        absorb: &[Answer::Hand {
            file: HAND_BUNDLE,
            why: "the block's content is declared in the bundle",
        }],
    },
    KindSpec {
        name: "setkey",
        detects: Detects::Differs,
        converge: &[Answer::Verb("temper install")],
        absorb: &[Answer::Hand {
            file: HAND_BUNDLE,
            why: "a setkey is fixed policy with exactly one owner — retune it there, \
                  not in the app's own UI",
        }],
    },
    KindSpec {
        name: "sysfile",
        detects: Detects::Differs,
        converge: &[Answer::Verb("temper install")],
        absorb: &[Answer::Hand {
            file: "the step's source file in the temper folder",
            why: "no verb captures a root-owned /etc file back into the folder",
        }],
    },
    KindSpec {
        name: "exec",
        detects: Detects::Differs,
        converge: &[Answer::Verb("temper install")],
        absorb: &[Answer::NA("an exec declares a script, not a state to absorb")],
    },
    // `install` opens it in System Settings for the user to approve.
    KindSpec {
        name: "profile",
        detects: Detects::Missing,
        converge: &[Answer::Verb("temper install")],
        absorb: &[Answer::Hand {
            file: HAND_BUNDLE,
            why: "a .mobileconfig is declared in a shared bundle and nothing exports \
                  an installed profile back out",
        }],
    },
    // ---- Presence gates: reported for visibility. --------------------------
    KindSpec {
        name: "when",
        detects: Detects::Missing,
        converge: &[Answer::NA("the step's app is absent — status only")],
        absorb: &[Answer::NA("a gate reports reality; there is nothing to absorb")],
    },
    KindSpec {
        name: "needs",
        detects: Detects::Missing,
        converge: &[Answer::Hand {
            file: "the machine",
            why: "install the hard dependency the step names — `install` bails on it",
        }],
        absorb: &[Answer::Hand {
            file: HAND_BUNDLE,
            why: "drop the `needs` if the dependency is genuinely not wanted",
        }],
    },
    // ---- Packages: all four cells, per PROVIDER. ---------------------------
    //
    // One `package` kind for brew, flatpak and mas made a per-provider answer
    // inexpressible: `interface.rs` requires that a provider claiming `prune`
    // has a kind naming `temper prune`, so with the kind shared, every provider
    // inherited every other provider's answer. Three of them could satisfy the
    // cross-check on one provider's code — and giving one an honest, different
    // answer was impossible. Principle #13's "the family always gets a second
    // member" applied to a family that already had three.
    //
    // brew/cask/brew-tap are ONE provider (they converge through `brew bundle`
    // together), so they share a kind. vscode is deliberately not a provider —
    // see `interface::PROVIDERS` — but its findings still need a registered
    // kind, and having one makes the ownership it declines explicit.
    KindSpec {
        name: "brew-package",
        detects: Detects::Missing,
        converge: &[Answer::Verb("temper install --packages-only")],
        absorb: &[Answer::Verb("temper reconcile")],
    },
    KindSpec {
        name: "brew-package-extra",
        detects: Detects::Extra,
        converge: &[Answer::Verb("temper prune")],
        absorb: &[Answer::Verb("temper reconcile")],
    },
    KindSpec {
        name: "flatpak-package",
        detects: Detects::Missing,
        converge: &[Answer::Verb("temper install --packages-only")],
        absorb: &[Answer::Verb("temper reconcile")],
    },
    KindSpec {
        name: "flatpak-package-extra",
        detects: Detects::Extra,
        converge: &[Answer::Verb("temper prune")],
        absorb: &[Answer::Verb("temper reconcile")],
    },
    KindSpec {
        name: "mas-package",
        detects: Detects::Missing,
        converge: &[Answer::Verb("temper install --packages-only")],
        absorb: &[Answer::Verb("temper reconcile")],
    },
    KindSpec {
        name: "mas-package-extra",
        detects: Detects::Extra,
        converge: &[Answer::Verb("temper prune")],
        absorb: &[Answer::Verb("temper reconcile")],
    },
    KindSpec {
        name: "vscode-package",
        detects: Detects::Missing,
        converge: &[Answer::Verb("temper install --packages-only")],
        absorb: &[Answer::Verb("temper reconcile")],
    },
    KindSpec {
        name: "vscode-package-extra",
        detects: Detects::Extra,
        converge: &[Answer::Verb("temper prune")],
        absorb: &[Answer::Verb("temper reconcile")],
    },
    KindSpec {
        name: "retired-package",
        detects: Detects::Extra,
        converge: &[Answer::Verb("temper prune")],
        absorb: &[Answer::Hand {
            file: "the `retire_packages` list that named it",
            why: "drop the entry if you have decided you want this back",
        }],
    },
    KindSpec {
        name: "retired-present",
        detects: Detects::Extra,
        converge: &[Answer::Verb("temper prune")],
        // Absorbing means deciding you want it after all, which is deleting the
        // `retire` entry — an edit to the declaration, not a reconcile.
        absorb: &[Answer::Hand {
            file: "the `retire` list that named it",
            why: "drop the entry if you have decided you want this back",
        }],
    },
    KindSpec {
        name: "deployed-file-extra",
        detects: Detects::Extra,
        converge: &[Answer::Verb("temper prune")],
        // There is nothing to absorb: re-declaring it means writing the `copy`
        // step you deleted, which is authoring, not reconciling.
        absorb: &[Answer::Hand {
            file: HAND_BUNDLE,
            why: "re-declare the step you removed, if removing it was the mistake",
        }],
    },
    KindSpec {
        name: "flatpak-remote",
        detects: Detects::Missing,
        converge: &[Answer::Verb("temper install --packages-only")],
        absorb: &[Answer::Verb("temper reconcile")],
    },
    KindSpec {
        name: "flatpak-remote-extra",
        detects: Detects::Extra,
        converge: &[Answer::Verb("temper prune")],
        absorb: &[Answer::Verb("temper reconcile")],
    },
    KindSpec {
        name: "rpm-ostree-extra",
        detects: Detects::Extra,
        converge: &[Answer::Verb("temper prune")],
        absorb: &[Answer::Verb("temper reconcile")],
    },
    KindSpec {
        name: "rpm-ostree",
        detects: Detects::Missing,
        converge: &[Answer::Verb("temper install --packages-only")],
        absorb: &[Answer::Verb("temper reconcile")],
    },
    KindSpec {
        name: "brew-trust",
        detects: Detects::Missing,
        converge: &[Answer::Verb("temper install --packages-only")],
        absorb: &[Answer::Verb("temper reconcile")],
    },
    KindSpec {
        name: "brew-trust-extra",
        detects: Detects::Extra,
        converge: &[Answer::Verb("temper prune")],
        absorb: &[Answer::Verb("temper reconcile")],
    },
    // ---- GNOME extensions: all four cells. --------------------------------
    KindSpec {
        name: "gnome-extension",
        detects: Detects::Missing,
        converge: &[Answer::Verb("temper install --packages-only")],
        absorb: &[Answer::Verb("temper reconcile")],
    },
    KindSpec {
        name: "gnome-extension-enable",
        detects: Detects::Differs,
        converge: &[Answer::Verb("temper install --packages-only")],
        // Absorbing means writing `enabled = false` into the declaration that
        // names the extension — an edit to whichever scope declared it, which is
        // the fleet's file as often as this machine's.
        absorb: &[Answer::Hand {
            file: "the declaration that names it (a bundle, or [[machine]].gnome_extensions)",
            why: "set `enabled` on the extension to match what you actually want",
        }],
    },
    KindSpec {
        name: "gnome-extension-extra",
        detects: Detects::Extra,
        converge: &[Answer::Verb("temper prune")],
        absorb: &[Answer::Verb("temper reconcile")],
    },
    // ---- Desktop dconf. ---------------------------------------------------
    KindSpec {
        name: "dconf-key",
        detects: Detects::Differs,
        converge: &[Answer::Verb("temper restore-dconf")],
        // Two real answers: per-key, or capture the whole subtree.
        absorb: &[
            Answer::Verb("temper reconcile"),
            Answer::Verb("temper snapshot-dconf"),
        ],
    },
    KindSpec {
        name: "dconf-extra",
        detects: Detects::Extra,
        converge: &[Answer::NA(
            "a snapshot is not exhaustive — nothing deletes a live key it never mentioned",
        )],
        absorb: &[
            Answer::Verb("temper reconcile"),
            Answer::Verb("temper snapshot-dconf"),
        ],
    },
    KindSpec {
        name: "dconf-unavailable",
        detects: Detects::Differs,
        converge: &[Answer::NA("the store cannot be read here — nothing can act on it")],
        absorb: &[Answer::NA("the store cannot be read here — nothing can be absorbed from it")],
    },
    KindSpec {
        name: "dconf-uncaptured",
        detects: Detects::Missing,
        converge: &[Answer::NA("there is nothing captured to push back out")],
        absorb: &[Answer::Verb("temper snapshot-dconf")],
    },
    KindSpec {
        name: "package-unavailable",
        detects: Detects::Differs,
        converge: &[Answer::NA(
            "the package manager could not be read here, so nothing may act on it",
        )],
        absorb: &[Answer::NA(
            "a drop computed from a manager that could not answer is a spec deletion",
        )],
    },
    // ---- Assertions are drift-only: they report a condition. --------------
    // `install` structurally cannot satisfy one, so the converge cell is NA and
    // the spec cell is the honest answer: change what you asserted.
    KindSpec {
        name: "absent",
        detects: Detects::Differs,
        converge: &[Answer::NA("assertions report a condition; no converge satisfies one")],
        absorb: &[Answer::Hand {
            file: HAND_BUNDLE,
            why: "resolve the condition, or change the [[assert]] that declares it",
        }],
    },
    KindSpec {
        name: "contains-line",
        detects: Detects::Differs,
        converge: &[Answer::NA("assertions report a condition; no converge satisfies one")],
        absorb: &[Answer::Hand {
            file: HAND_BUNDLE,
            why: "resolve the condition, or change the [[assert]] that declares it",
        }],
    },
    KindSpec {
        name: "mode",
        detects: Detects::Differs,
        converge: &[Answer::NA("assertions report a condition; no converge satisfies one")],
        absorb: &[Answer::Hand {
            file: HAND_BUNDLE,
            why: "resolve the condition, or change the [[assert]] that declares it",
        }],
    },
    KindSpec {
        name: "executable-resolves",
        detects: Detects::Differs,
        converge: &[Answer::NA("assertions report a condition; no converge satisfies one")],
        absorb: &[Answer::Hand {
            file: HAND_BUNDLE,
            why: "resolve the condition, or change the [[assert]] that declares it",
        }],
    },
    KindSpec {
        name: "not-member",
        detects: Detects::Differs,
        converge: &[Answer::NA("assertions report a condition; no converge satisfies one")],
        absorb: &[Answer::Hand {
            file: HAND_BUNDLE,
            why: "resolve the condition, or change the [[assert]] that declares it",
        }],
    },
    KindSpec {
        name: "shell",
        detects: Detects::Differs,
        converge: &[Answer::NA("assertions report a condition; no converge satisfies one")],
        absorb: &[Answer::Hand {
            file: HAND_BUNDLE,
            why: "resolve the condition, or change the [[assert]] that declares it",
        }],
    },
    KindSpec {
        name: "json-semantic",
        detects: Detects::Differs,
        converge: &[Answer::NA("assertions report a condition; no converge satisfies one")],
        absorb: &[Answer::Hand {
            file: HAND_BUNDLE,
            why: "resolve the condition, or change the [[assert]] that declares it",
        }],
    },
    KindSpec {
        name: "unknown",
        detects: Detects::Differs,
        converge: &[Answer::NA("an unrecognised assertion — nothing can act on it")],
        absorb: &[Answer::Hand {
            file: HAND_BUNDLE,
            why: "an unrecognised assertion — fix the bundle",
        }],
    },
];

/// The registered spec for a `Finding.kind`, if it has one.
pub fn kind_spec(kind: &str) -> Option<&'static KindSpec> {
    KIND_ANSWERS.iter().find(|k| k.name == kind)
}

impl KindSpec {
    /// Commands that change the machine.
    pub fn converge_verbs(&self) -> Vec<&'static str> {
        verbs(self.converge)
    }
    /// Commands that change the spec.
    pub fn absorb_verbs(&self) -> Vec<&'static str> {
        verbs(self.absorb)
    }
}

fn verbs(answers: &'static [Answer]) -> Vec<&'static str> {
    answers
        .iter()
        .filter_map(|a| match a {
            Answer::Verb(c) => Some(*c),
            _ => None,
        })
        .collect()
}

/// Every distinct command any answer names — the set a CLI-side test checks
/// really exists, so a verb rename can never leave drift teaching a dead name.
pub fn answer_commands() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = KIND_ANSWERS
        .iter()
        .flat_map(|k| [k.converge, k.absorb])
        .flat_map(verbs)
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
    // Which commands the DRIFTED kinds actually name, read out of the registry
    // rather than re-derived here.
    //
    // Two independent encodings of the same knowledge had already drifted apart:
    // this function offered `temper reconcile` for `extension`, `rpm` and
    // `dconf-uncaptured` — none of which reconcile has a code path for — while
    // the registry said otherwise, and only the registry→here direction was
    // tested. Worse, the config-drift test was a *denylist*, so every kind added
    // in future defaulted into "run `temper install`"; that is how a failed
    // `needs` came to be answered by a verb that bails on a failed `needs`.
    let mut converge: std::collections::BTreeSet<&'static str> = Default::default();
    let mut absorb: std::collections::BTreeSet<&'static str> = Default::default();
    for f in items.iter().filter(|f| !f.ok) {
        if let Some(k) = kind_spec(f.kind) {
            converge.extend(k.converge_verbs());
            absorb.extend(k.absorb_verbs());
        }
    }
    let converges = |c: &str| converge.contains(c);
    let absorbs = |c: &str| absorb.contains(c);

    let drifted = |kinds: &[&str]| items.iter().any(|f| !f.ok && kinds.contains(&f.kind));
    let extra_pkg = drifted(&[
        "brew-package-extra",
        "flatpak-package-extra",
        "mas-package-extra",
        "vscode-package-extra",
    ]);
    let trust_extra = drifted(&["brew-trust-extra"]);
    let dconf_capture = drifted(&["dconf-key", "dconf-extra", "dconf-uncaptured"]);
    let pkg_capture = drifted(&[
        "brew-package",
        "brew-package-extra",
        "flatpak-package",
        "flatpak-package-extra",
        "mas-package",
        "mas-package-extra",
        "vscode-package",
        "vscode-package-extra",
        "brew-trust",
        "brew-trust-extra",
    ]);
    let ext_capture = drifted(&["gnome-extension", "gnome-extension-extra"]);

    let mut out = Vec::new();
    let push = |out: &mut Vec<Remediation>, label: &str, command: &str| {
        out.push(Remediation {
            label: label.to_string(),
            command: command.to_string(),
        })
    };
    // Machine → spec (converge the machine toward the declared state).
    if converges("temper install --packages-only") {
        push(
            &mut out,
            "install declared packages/extensions that are missing, and trust declared taps",
            "temper install --packages-only",
        );
    }
    if converges("temper prune") {
        let label = match (extra_pkg, trust_extra, drifted(&["gnome-extension-extra"])) {
            (_, _, true) if extra_pkg || trust_extra => {
                "uninstall packages / GNOME extensions and untrust taps not in the spec (asks first)"
            }
            (false, false, true) => "uninstall the GNOME extensions not in the spec (asks first)",
            (true, true, _) => "uninstall packages / untrust taps not in the spec (asks first)",
            (false, true, _) => "untrust taps not in the spec (asks first)",
            _ => "uninstall packages not in the spec (asks first)",
        };
        push(&mut out, label, "temper prune");
    }
    if converges("temper restore-dconf") {
        push(
            &mut out,
            "reload the desktop snapshot, clobbering live tweaks (asks first)",
            "temper restore-dconf",
        );
    }
    // Spec ← machine (absorb the machine's state into the spec). Fires only for
    // kinds whose `absorb` cell actually names reconcile.
    if absorbs("temper reconcile") {
        let mut parts = Vec::new();
        if pkg_capture {
            parts.push("packages, tap-trust");
        }
        if ext_capture {
            parts.push("GNOME extensions");
        }
        if dconf_capture {
            parts.push("desktop keys");
        }
        let label = format!(
            "interactively add extras / drop entries you removed on purpose ({})",
            parts.join(", ")
        );
        push(&mut out, &label, "temper reconcile");
    }
    if absorbs("temper snapshot-dconf") {
        push(
            &mut out,
            "capture the whole desktop subtree into the spec instead",
            "temper snapshot-dconf",
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
    // Config drift: re-apply, or revert the last run. Gated on the registry
    // naming `install` for a kind that actually drifted — never on "everything
    // not in this list", which is how a failed `needs` came to be answered by a
    // verb that bails on a failed `needs`.
    if converges("temper install") {
        push(
            &mut out,
            "re-apply the drifted config steps above (copy/block/setkey/sysfile/exec/profile)",
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
        // A manager that was asked and could not answer is reported, never
        // passed over: its packages are silently absent from both directions
        // below, and an unexplained silence there reads as "in sync".
        for m in installed.unavailable_managers() {
            findings.push(Finding {
                app: "packages".into(),
                kind: "package-unavailable",
                target: m.as_str().to_string(),
                // Status-only — degraded, not drift.
                ok: true,
                status: "unavailable".into(),
                detail: Some(format!(
                    "`{}` is installed but failed to list what it has — \
                     drift and prune skip it rather than read the silence as empty",
                    m.as_str()
                )),
            });
        }
        for p in packages::missing(&effective, &installed) {
            findings.push(Finding {
                app: "packages".into(),
                kind: package_kind(p.manager, false),
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
                kind: package_kind(m, true),
                target,
                ok: false,
                status: "extra".into(),
                detail: None,
            });
        }
        for (m, name) in providers::brew_extras(&effective, ignore)? {
            findings.push(Finding {
                app: "packages".into(),
                kind: package_kind(m, true),
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
    if let (true, Some(trusted)) = (trust_is_declared(brew_trust), providers::trusted_taps()?) {
        // Declared but not trusted → a gap (brew silently skips the tap's
        // formulae). Fixed by `install`/`update`, which run `brew trust`.
        for tap in brew_trust {
            if !trusted.iter().any(|t| t == tap) {
                findings.push(Finding {
                    app: "brew-trust".into(),
                    kind: "brew-trust",
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
                    app: "brew-trust".into(),
                    kind: "brew-trust-extra",
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
        // Scope is a property of the declaration, so the answer differs per
        // item even though the kind is one. A bundle-declared extension is a
        // hand edit in that file; `reconcile` has no candidate for it and would
        // silently do nothing.
        let from = providers::declaring_bundle(
            home,
            machine,
            providers::DeclKind::GnomeExtension,
            &uuid,
        );
        findings.push(Finding {
            app: "gnome-extensions".into(),
            kind: "gnome-extension",
            target: uuid,
            ok: false,
            status: "missing".into(),
            detail: from.map(|f| {
                format!("declared in {f} — fleet scope, so removing it is a hand edit there")
            }),
        });
    }
    // The extras direction, which every other manager already reported. Carries
    // its remedy in the status: there is no single command for it, because
    // `extensions` lives in a shared bundle that only a human should edit.
    for (uuid, want) in
        providers::gext_enable_drift(&providers::effective_extension_specs(home, machine)?)
    {
        findings.push(Finding {
            app: "gnome-extensions".into(),
            kind: "gnome-extension-enable",
            target: uuid,
            ok: false,
            status: if want { "not enabled" } else { "enabled" }.into(),
            detail: Some(
                if want {
                    "declared enabled — `install` switches it on"
                } else {
                    "declared disabled — `install` switches it off"
                }
                .into(),
            ),
        });
    }
    for uuid in providers::gext_extras(&effective_ext, ignore) {
        findings.push(Finding {
            app: "gnome-extensions".into(),
            kind: "gnome-extension-extra",
            target: uuid,
            ok: false,
            status: "extra".into(),
            detail: None,
        });
    }
    // Packages declared unwanted that are nonetheless installed.
    {
        let unwanted: Vec<packages::Pkg> = crate::manifest::effective_retire_packages(home, machine)?
            .iter()
            .filter_map(|t| packages::parse(t).ok())
            .collect();
        if !unwanted.is_empty() {
            let installed = providers::probe(&unwanted)?;
            for p in &unwanted {
                if installed.probed(p.manager) && installed.contains(p.manager, &p.match_name()) {
                    findings.push(Finding {
                        app: "retired".into(),
                        kind: "retired-package",
                        target: format!("{} {}", p.manager.as_str(), p.match_name()),
                        ok: false,
                        status: "still installed".into(),
                        detail: Some("declared retired — `prune` removes it".into()),
                    });
                }
            }
        }
    }
    // Explicitly retired paths that are still here.
    for path in crate::manifest::effective_retire(home, machine)? {
        if crate::manifest::expand_tilde(&path).exists() {
            findings.push(Finding {
                app: "retired".into(),
                kind: "retired-present",
                target: path,
                ok: false,
                status: "still present".into(),
                detail: Some("declared retired — `prune` removes it".into()),
            });
        }
    }
    // Files this spec used to deploy and no longer declares. Nothing else can
    // see these: a filesystem cannot be asked which of its files temper wrote.
    let declared = deployed_paths(home, machine)?;
    for (_key, rec) in crate::ledger::residue(&machine.name, &declared) {
        if !crate::ledger::still_present(&rec) {
            continue;
        }
        let edited = !crate::ledger::is_untouched(&rec);
        findings.push(Finding {
            app: "deployed".into(),
            kind: "deployed-file-extra",
            target: match &rec.marker {
                Some(m) => format!("{} ({m})", rec.path),
                None => rec.path.clone(),
            },
            ok: false,
            status: "extra".into(),
            detail: Some(if edited {
                "edited since temper deployed it — prune will report, not remove".into()
            } else {
                format!("deployed by a `{}` step the spec no longer declares", rec.kind)
            }),
        });
    }
    let effective_remotes = providers::effective_remotes(home, machine)?;
    for r in providers::remotes_missing(&effective_remotes) {
        // `remotes_missing` yields "<name> <url>"; the name is the identity.
        let name = r.split_whitespace().next().unwrap_or(&r).to_string();
        let from =
            providers::declaring_bundle(home, machine, providers::DeclKind::FlatpakRemote, &name);
        findings.push(Finding {
            app: "flatpak-remote".into(),
            kind: "flatpak-remote",
            target: r,
            ok: false,
            status: "missing".into(),
            detail: from.map(|f| {
                format!("declared in {f} — fleet scope, so removing it is a hand edit there")
            }),
        });
    }
    for r in providers::remotes_extras(&effective_remotes, ignore) {
        findings.push(Finding {
            app: "flatpak-remote".into(),
            kind: "flatpak-remote-extra",
            target: r,
            ok: false,
            status: "extra".into(),
            detail: None,
        });
    }
    let effective_rpm = providers::effective_rpm(home, machine)?;
    for pkg in providers::rpm_ostree_extras(&effective_rpm, ignore) {
        findings.push(Finding {
            app: "rpm-ostree".into(),
            kind: "rpm-ostree-extra",
            target: pkg,
            ok: false,
            status: "extra".into(),
            detail: None,
        });
    }
    for pkg in providers::rpm_missing(&effective_rpm) {
        let from =
            providers::declaring_bundle(home, machine, providers::DeclKind::RpmOstree, &pkg);
        findings.push(Finding {
            app: "rpm-ostree".into(),
            kind: "rpm-ostree",
            target: pkg,
            ok: false,
            status: "missing".into(),
            detail: from.map(|f| {
                format!("declared in {f} — fleet scope, so removing it is a hand edit there")
            }),
        });
    }

    // Whole-desktop dconf snapshots (machine scope): the captured file versus a
    // live dump, both filtered through the same `strip`. Grouped per snapshot so
    // a narrow subtree (`…/shell/extensions/`) reads as its own section.
    // Degraded, not failed, on a host without dconf (a Mac).
    let mut all_snaps: Vec<crate::manifest::DconfSnapshot> = machine.dconf.clone();
    all_snaps.extend(providers::extension_snapshots(home, machine)?);
    for snap in &all_snaps {
        let group = format!("dconf/{}", snap.name());
        let owned = crate::dconf::owned_elsewhere(home, machine, snap)?;
        match crate::dconf::snapshot_state_owned(home, snap, &owned)? {
            // Reported, not dropped. A declared snapshot that cannot be
            // evaluated used to vanish from the report entirely, so a Mac — or
            // a Linux box with no session — showed no sign that a whole
            // machine-scope subtree was going unchecked. `setkey` already
            // degrades this way; a snapshot must too.
            crate::dconf::SnapshotState::Unobservable(why) => findings.push(Finding {
                app: group,
                kind: "dconf-unavailable",
                target: snap.file.clone(),
                // Status-only: degraded, not drift. `ok` keeps it out of the
                // out-of-sync count, and the `unavailable` prefix is what
                // `status_only()` reads.
                ok: true,
                status: "unavailable".into(),
                detail: Some(why),
            }),
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

/// A step's effective lifecycle. Defaults by primitive: exec, seed & profile are
/// install-only; copy/setkey/block are re-applied every update ("always").
///
/// `profile` is install-only because its apply is a **GUI window**, and a missing
/// profile is real drift: were it `always`, `update` would re-open System Settings
/// on every routine run until the user gave in. A file write can be re-applied
/// silently; a dialog cannot. So `drift` names the condition, `install` is the
/// deliberate act that re-offers it, and the boring upgrade path stays quiet.
fn lifecycle(step: &Step) -> &str {
    if let Some(r) = &step.run {
        return r;
    }
    if step.exec.is_some() || step.seed || step.profile.is_some() {
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
    /// Changes this run made that `undo` cannot revert, as `label — why`.
    /// Empty is the normal case, and the point is that it is *said* either way
    /// rather than discovered when the revert turns out to be a no-op.
    pub unrevertible: Vec<String>,
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
    // Remotes before the packages that come from them. A remote is where a
    // flatpak is fetched FROM, so adding it after the converge meant the first
    // run on a fresh box could not resolve a vendor app at all — it failed
    // (best-effort, warned), the remote appeared, and only a SECOND converge
    // installed it. Same reason tap-trust runs before brew.
    providers::remotes_converge(&providers::effective_remotes(home, machine)?, dry_run)?;
    // The journal opens BEFORE the converge, not after it. It used to be created
    // for the config-step phase only, so `install --packages-only` returned
    // without ever journaling anything — packages were unrevertible because
    // nothing recorded them, not because reversing them is hard.
    let mut journal = Journal::begin();
    // What the converge is about to add, captured BEFORE it runs. `converge`
    // hands the whole declared set to brew/flatpak and lets them skip what is
    // present, so the delta is only knowable from this side.
    let to_add: Vec<(packages::Manager, String)> = if dry_run {
        Vec::new()
    } else {
        let installed = providers::probe(&effective)?;
        packages::missing(&effective, &installed)
            .into_iter()
            .map(|p| (p.manager, p.match_name()))
            .collect()
    };
    let packages = providers::converge(&effective, dry_run, verbose)?;
    // Journal per provider, so undo dispatches to the right uninstall. Recorded
    // after the converge and only for managers whose install is not also an
    // upgrade path — see `Entry::PackagesInstalled`.
    for &mgr in packages::Manager::ALL {
        let Some(name) = packages::journal_provider(mgr) else {
            continue;
        };
        let added: Vec<String> = to_add
            .iter()
            .filter(|(m, _)| *m == mgr)
            .map(|(_, n)| n.clone())
            .collect();
        journal.record_packages(name, &added);
    }
    let gext_installed = providers::gext_converge(
        &providers::effective_extensions(home, machine)?,
        dry_run,
        verbose,
    )?;
    journal.record_packages("gnome-extensions", &gext_installed);
    // Enable/disable AFTER installing: an extension cannot be switched on until
    // it exists, so a fresh install would otherwise need a second converge.
    providers::gext_enable_converge(
        &providers::gext_enable_drift(&providers::effective_extension_specs(home, machine)?),
        dry_run,
    )?;
    let rpm_installed = providers::rpm_converge(
        &providers::effective_rpm(home, machine)?,
        dry_run,
        verbose,
    )?;
    journal.record_packages("rpm-ostree", &rpm_installed);
    // Layering stages a deployment, so anything layered means a reboot.
    let reboot = !rpm_installed.is_empty();

    // `install-missing`: packages only — skip the config-step phase entirely.
    if packages_only {
        journal.commit()?;
        return Ok(InstallReport {
            packages,
            upgraded: None, // `install-missing` adds, never upgrades
            steps_changed: 0,
            steps_ran: 0,
            steps_total: 0,
            reboot,
            skipped: Vec::new(),
            // Package *installs* are journaled, but this phase does three other
            // things that are not, and claiming an empty list said `undo` would
            // cover them. `interface.rs` scores brew-trust and flatpak-remote
            // `revertible: No` in as many words.
            unrevertible: packages_only_unrevertible(brew_trust, home, machine),
        });
    }

    // Phase 2 — config steps (`resolved` was needed above, for the root ask).
    let (mut changed, mut total, mut ran) = (0usize, 0usize, 0usize);
    let mut skipped = Vec::new();
    let mut unrevertible: Vec<String> = Vec::new();
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
                // AGENTS.md question 7: what `undo` cannot revert has to be
                // said while the run is still a forecast. Collected only for
                // steps that would actually change something — listing a
                // converged `sysfile` as unrevertible warns about work that
                // is not going to happen.
                if let Some(why) = unrevertible_reason(step) {
                    unrevertible.push(format!("{} — {why}", label.trim()));
                }
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
            if matches!(did, Applied::Changed | Applied::Ran) {
                if let Some(why) = unrevertible_reason(step) {
                    let named = format!("{} — {why}", label.trim());
                    // Also journaled, so the RUN exists even when nothing in it
                    // is revertible — otherwise a converge whose only changes
                    // were an `exec` and a `sysfile` wrote no run at all, and
                    // the next bare `undo` reverted an older, unrelated one.
                    journal.record_unrevertible(&named);
                    unrevertible.push(named);
                }
            }
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
        // Record what this spec deploys, so a step deleted later leaves residue
        // that `drift` can see. Best-effort: a ledger that cannot be written
        // must not fail a converge that already succeeded.
        if let Ok(l) = deployed_paths(home, machine) {
            let _ = crate::ledger::save(&machine.name, &l);
        }
    }
    Ok(InstallReport {
        packages,
        upgraded: None, // `install` converges the declared set; `update` upgrades
        steps_changed: changed,
        steps_ran: ran,
        steps_total: total,
        reboot,
        skipped,
        unrevertible,
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
    /// Paths a `retire` entry declares must be gone, that are still here.
    /// Unlike residue these are removed regardless of content: the spec does not
    /// say "remove this if temper wrote it", it says this must not exist.
    pub retired: Vec<String>,
    /// Files a `copy`/`sysfile` step deployed and the spec no longer declares,
    /// and which are still byte-identical to what temper wrote. An edited one is
    /// never in here — it is reported instead.
    pub residue: Vec<String>,
    /// Residue the user has since edited: reported, never removed.
    pub residue_edited: Vec<String>,
    /// Flatpak remotes to remove — the prune counterpart of a
    /// `flatpak-remote-extra`.
    pub flatpak_remotes: Vec<String>,
    /// Layered rpms to un-layer — the prune counterpart of an
    /// `rpm-ostree-extra`. Stages a new deployment, so it sets the reboot signal
    /// exactly as layering does.
    pub rpm_ostree: Vec<String>,
    /// Package managers that are installed and could not be asked what they
    /// hold. **Never removable**, and deliberately not in `items()`: nothing is
    /// planned for them, which is the point. They are here so an empty plan can
    /// say *why* it is empty.
    ///
    /// Principle #12 — report `unavailable`, never absent — was enforced for
    /// `drift` and not for the verb that deletes. A consumer reading
    /// `"extras": []` from a machine whose brew is broken cannot tell it from a
    /// converged one, and "nothing to remove" is the wrong conclusion to draw
    /// from "I could not look".
    pub unavailable: Vec<packages::Manager>,
    /// Extras this machine holds that `prune` **cannot** remove, with the reason.
    /// Reported, never counted, and deliberately not in `items()` — for the same
    /// reason as `unavailable`: nothing is planned for them.
    ///
    /// Today this is the flatpaks in a **custom** installation
    /// (`/etc/flatpak/installations.d/`), which removal cannot name without being
    /// told which one, and any whose installation could not be read at all.
    /// `prune` removes from both the system and user installations, so neither is
    /// in here. The executor draws the same line, and both must, because a
    /// confirm that overstates what a destructive run will do is worse than a
    /// verbose one — it spends the user's trust on the single prompt standing
    /// between them and an irreversible removal.
    pub unremovable: Vec<(packages::Manager, String, &'static str)>,
}

impl PrunePlan {
    /// Everything this prune would act on. `residue_edited` is deliberately
    /// absent: it is reported, never removed, so counting it would claim work
    /// that does not happen.
    pub fn items(&self) -> Vec<(&'static str, String)> {
        let mut out: Vec<(&'static str, String)> = Vec::new();
        for (m, n) in &self.packages {
            out.push(("packages", format!("{} {n}", m.as_str())));
        }
        for t in &self.untrust {
            out.push(("untrust", t.clone()));
        }
        for e in &self.extensions {
            out.push(("extensions", e.clone()));
        }
        for r in &self.rpm_ostree {
            out.push(("rpm_ostree", r.clone()));
        }
        for r in &self.flatpak_remotes {
            out.push(("flatpak_remotes", r.clone()));
        }
        for r in &self.residue {
            out.push(("residue", r.clone()));
        }
        for r in &self.retired {
            out.push(("retired", r.clone()));
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.items().is_empty()
    }

    /// Every item this prune acts on. Derived, because a count that omits a
    /// variant is a silent cap (Principle #6) — this once asked "remove 0
    /// item(s)?" and then uninstalled three GNOME extensions.
    pub fn len(&self) -> usize {
        self.items().len()
    }

    /// What this run will actually do, in words, one clause per non-empty list.
    ///
    /// The confirm used to recite everything `prune` is *capable* of —
    /// "uninstalls packages, GNOME extensions and flatpaks, untrusts taps,
    /// removes flatpak remotes, DELETES the files listed above, and un-layers
    /// rpms (a reboot applies that last one)" — on every run, whatever was in
    /// the plan. A run removing nine GNOME extensions warned about deleting
    /// files and rebooting, neither of which was going to happen.
    ///
    /// That is worse than verbose. The careful reader is alarmed by clauses that
    /// do not apply, and the frequent reader learns the wall of text carries no
    /// information and stops reading it — on the one prompt standing between
    /// them and an irreversible removal.
    pub fn effects(&self) -> Vec<String> {
        let mut out = Vec::new();
        let n = |v: &[String]| v.len();
        if !self.packages.is_empty() {
            out.push(format!("uninstalls {} package(s)", self.packages.len()));
        }
        if !self.extensions.is_empty() {
            out.push(format!(
                "uninstalls {} GNOME extension(s)",
                n(&self.extensions)
            ));
        }
        if !self.untrust.is_empty() {
            out.push(format!("untrusts {} tap(s)", n(&self.untrust)));
        }
        if !self.flatpak_remotes.is_empty() {
            out.push(format!(
                "removes {} flatpak remote(s)",
                n(&self.flatpak_remotes)
            ));
        }
        let files = n(&self.residue) + n(&self.retired);
        if files > 0 {
            out.push(format!("DELETES {files} file(s)"));
        }
        if !self.rpm_ostree.is_empty() {
            out.push(format!(
                "un-layers {} rpm(s) — a reboot applies that",
                n(&self.rpm_ostree)
            ));
        }
        out
    }

    /// The lists `items()` labels, in preview order. `residue_edited` is not one
    /// of them: it is reported, never removed.
    pub const LISTS: &'static [&'static str] = &[
        "packages",
        "untrust",
        "extensions",
        "rpm_ostree",
        "flatpak_remotes",
        "residue",
        "retired",
    ];

    /// How one item reads in the preview. The catch-all arm is the point: a list
    /// added to `items()` and forgotten here still prints itself, rather than
    /// being counted, confirmed and deleted without ever being shown.
    pub fn describe(list: &str, item: &str) -> String {
        match list {
            "packages" => item.to_string(),
            "untrust" => format!("untrust {item}"),
            "extensions" => format!("uninstall extension {item}"),
            "rpm_ostree" => format!("un-layer rpm {item}"),
            "flatpak_remotes" => format!("remove flatpak remote {item}"),
            "residue" => format!("remove {item} (deployed by a step the spec dropped)"),
            "retired" => format!("remove {item} (the spec retires this path)"),
            other => format!("{other} {item}"),
        }
    }

    /// The `--json` document. Derived one key per list for the same reason the
    /// count is: hand-writing the object omitted `flatpak_remotes` and `retired`
    /// while `commit_prune` removed both.
    ///
    /// `extras` keeps its name and its `{manager, name}` shape — it is the
    /// published contract, and tidiness is not a reason to break a consumer.
    pub fn to_json(&self, machine: &str, removed: bool) -> serde_json::Value {
        let extras: Vec<_> = self
            .packages
            .iter()
            .map(|(mgr, name)| serde_json::json!({ "manager": mgr.as_str(), "name": name }))
            .collect();
        let mut doc = serde_json::Map::new();
        doc.insert("machine".into(), serde_json::json!(machine));
        doc.insert("extras".into(), serde_json::json!(extras));
        for list in Self::LISTS {
            if *list == "packages" {
                continue; // published as `extras`, above
            }
            let of: Vec<String> = self
                .items()
                .into_iter()
                .filter(|(l, _)| l == list)
                .map(|(_, i)| i)
                .collect();
            doc.insert((*list).into(), serde_json::json!(of));
        }
        doc.insert(
            "residue_edited".into(),
            serde_json::json!(self.residue_edited),
        );
        // Not in `LISTS`, because nothing is planned for these — which is
        // exactly why they have to be published. Without them a consumer cannot
        // tell an empty plan from an unaskable machine.
        doc.insert(
            "unavailable".into(),
            serde_json::json!(self
                .unavailable
                .iter()
                .map(|m| m.as_str())
                .collect::<Vec<_>>()),
        );
        doc.insert(
            "unremovable".into(),
            serde_json::json!(self
                .unremovable
                .iter()
                .map(|(m, n, why)| serde_json::json!({
                    "manager": m.as_str(),
                    "name": n,
                    "why": why,
                }))
                .collect::<Vec<_>>()),
        );
        doc.insert("removed".into(), serde_json::json!(removed));
        serde_json::Value::Object(doc)
    }
}

/// Installed-but-undeclared packages, computed **once** for every verb that
/// reports them (`prune`, `adopt`) so they cannot disagree about what an extra
/// is. brew-family (brew/cask/tap) extras come from the dependency-aware
/// `providers::brew_extras`; a naive set-diff wrongly flags every installed
/// transitive dependency, which is why `adopt` used to list hundreds of entries
/// that `reconcile` then declined to offer.
///
/// Empty when the machine declares no packages, so an unconfigured machine
/// never proposes nuking everything. That gate belongs to packages ALONE —
/// tap-trust and extensions are separate kinds with their own opt-in gates.
fn package_extras(
    home: &Path,
    machine: &Machine,
    ignore: &Ignore,
) -> Result<Vec<(packages::Manager, String)>> {
    Ok(package_extras_observed(home, machine, ignore)?.0)
}

/// `package_extras`, plus the managers that could not be asked.
///
/// The extras list alone cannot distinguish "this manager reported nothing" from
/// "this manager is installed and failing", and for a **removal** verb those are
/// opposite instructions. `drift` has always made the distinction — it reports
/// `package-unavailable` — while `prune --json` emitted `"extras": []` either
/// way, which reads as a clean machine.
/// Extras, plus the managers that could not be asked for them.
type ExtrasObserved = (Vec<(packages::Manager, String)>, Vec<packages::Manager>);

fn package_extras_observed(
    home: &Path,
    machine: &Machine,
    ignore: &Ignore,
) -> Result<ExtrasObserved> {
    let effective = packages::effective_set(home, machine)?;
    if effective.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let installed = providers::probe(&effective)?;
    let unavailable: Vec<packages::Manager> = installed.unavailable_managers().collect();
    let mut out: Vec<(packages::Manager, String)> =
        packages::extras(&effective, &installed, ignore)
            .into_iter()
            .filter(|(m, _)| {
                !matches!(
                    m,
                    packages::Manager::Brew | packages::Manager::Cask | packages::Manager::Tap
                )
            })
            .collect();
    out.extend(providers::brew_extras(&effective, ignore)?);
    Ok((out, unavailable))
}

#[cfg(test)]
mod revertibility_tests {
    use super::*;

    fn step(src: &str) -> Step {
        toml::from_str(src).unwrap()
    }

    /// AGENTS.md question 7: a run whose only changes were unrevertible reverts
    /// nothing while reporting success, and the user finds out when the revert
    /// turns out to be a no-op. Each case here has a real reason, which matters
    /// because the *blanket* version of this claim ("packages can't be
    /// journaled") turned out to be false and kept six providers unwired.
    #[test]
    fn only_the_genuinely_unrevertible_is_named() {
        for src in [
            r#"exec = "setup.sh""#,
            r#"sysfile = "assets/x.conf"
to = "/etc/x.conf""#,
            r#"setkey = { backend = "defaults", key = "com.apple.dock.tilesize", value = 48 }"#,
            r#"profile = "assets/x.mobileconfig""#,
        ] {
            assert!(
                unrevertible_reason(&step(src)).is_some(),
                "expected an unrevertible reason for: {src}"
            );
        }

        // Journaled, so `undo` covers them — and saying otherwise would be the
        // same over-claim in the other direction.
        for src in [
            r#"copy = "assets/x"
to = "~/.config/x""#,
            r#"block = "assets/b"
in = "~/.zshrc"
marker = "temper""#,
            r#"setkey = { backend = "dconf", key = "/org/gnome/x", value = true }"#,
            r#"setkey = { backend = "json", file = "~/x.json", key = "a.b", value = 1 }"#,
        ] {
            assert!(
                unrevertible_reason(&step(src)).is_none(),
                "this IS revertible and must not be reported: {src}"
            );
        }
    }
}

#[cfg(test)]
mod prune_plan_tests {
    use super::*;

    /// Every list a prune can act on is counted.
    ///
    /// A count is output, so a count that omits a variant reports work it did as
    /// work it didn't (Principle #6). This has already been wrong once: `len()`
    /// summed two of three lists, so a prune whose only items were GNOME
    /// extensions asked "remove **0** item(s)?", uninstalled three, and reported
    /// "0 item(s) removed". Adding a fourth list is exactly when that recurs.
    #[test]
    fn the_count_covers_every_list_prune_acts_on() {
        let p = PrunePlan {
            packages: vec![(packages::Manager::Brew, "jq".into())],
            untrust: vec!["user/tap".into()],
            extensions: vec!["a@x".into()],
            rpm_ostree: vec!["vpn".into()],
            flatpak_remotes: vec!["vendor".into()],
            residue: vec!["~/.config/gone".into()],
            retired: vec!["~/.config/retired".into()],
            // Reported, never removed — so deliberately NOT counted as an item
            // prune acts on.
            residue_edited: vec!["~/.config/edited".into()],
            unavailable: Vec::new(),
            // Reported, never removed, for the same reason: prune is not able to
            // remove a system-scope flatpak, so counting it would make the
            // confirm promise work the executor declines.
            unremovable: vec![(
                packages::Manager::Flatpak,
                "org.image.Baked".into(),
                "installed system-wide, not by you",
            )],
        };
        assert!(!p.is_empty());
        assert_eq!(p.len(), 7, "a list prune acts on is not being counted");

        // …and each list alone is both non-empty and counted, so no single
        // variant can be the one that is silently dropped.
        for one in [
            PrunePlan { packages: p.packages.clone(), ..Default::default() },
            PrunePlan { untrust: p.untrust.clone(), ..Default::default() },
            PrunePlan { extensions: p.extensions.clone(), ..Default::default() },
            PrunePlan { rpm_ostree: p.rpm_ostree.clone(), ..Default::default() },
            PrunePlan { flatpak_remotes: p.flatpak_remotes.clone(), ..Default::default() },
            PrunePlan { residue: p.residue.clone(), ..Default::default() },
        ] {
            assert!(!one.is_empty());
            assert_eq!(one.len(), 1);
        }
        assert!(PrunePlan::default().is_empty());
        assert_eq!(PrunePlan::default().len(), 0);
    }
}

/// Prune installed-but-not-declared packages, trusted-but-undeclared taps, and
/// user-scope GNOME extensions no bundle declares. Returns the plan; the caller
/// previews and confirms before `commit_prune` applies it.
///
/// Each of the three is gated independently: a machine that declares no
/// packages still prunes its extension extras, because `drift` reports them.
/// The `Finding.kind` for a package manager, per **provider** rather than per
/// manager: brew, cask and brew-tap converge through one `brew bundle` and are
/// one provider, so they share a kind.
///
/// Literal strings, because the completeness scrape reads source for them — a
/// `format!`-built kind is invisible to it (PATTERNS, "Adding a provider").
fn package_kind(m: packages::Manager, extra: bool) -> &'static str {
    use packages::Manager::*;
    match (m, extra) {
        (Brew | Cask | Tap, false) => "brew-package",
        (Brew | Cask | Tap, true) => "brew-package-extra",
        (Flatpak, false) => "flatpak-package",
        (Flatpak, true) => "flatpak-package-extra",
        (Mas, false) => "mas-package",
        (Mas, true) => "mas-package-extra",
        (Vscode, false) => "vscode-package",
        (Vscode, true) => "vscode-package-extra",
    }
}

/// Whether the spec expresses an opinion about tap-trust at all.
///
/// The opt-in every other provider has, and the one tap-trust never got: "a
/// manager is only probed if you declare at least one of its packages". A spec
/// that names no tap is not a spec asking for every tap to be untrusted — but
/// `prune` read it that way, and would `brew untrust` the taps its own formulae
/// come from on any folder that simply never mentioned them.
///
/// Shared by `drift` and `prune` so the report and the removal cannot disagree
/// about whether the question is even being asked.
fn trust_is_declared(brew_trust: &[String]) -> bool {
    !brew_trust.is_empty()
}

pub fn run_prune(
    home: &Path,
    machine: &Machine,
    ignore: &Ignore,
    brew_trust: &[String],
) -> Result<PrunePlan> {
    let (mut packages, unavailable) = package_extras_observed(home, machine, ignore)?;
    // A flatpak extra is removable from either installation temper can name —
    // `prune` issues one batched call per scope. What it cannot name is a custom
    // installation (`--installation=NAME`), so those are reported instead. Split
    // them out HERE, at plan time, so the count, the confirm and `--json` all
    // describe the run that will actually happen: the executor draws the same
    // line, but a confirm that overstates a destructive run has already spent the
    // trust the prompt exists to earn.
    //
    // `None` means the installations could not be read, and the executor removes
    // nothing in that case; the plan has to agree, or it over-promises.
    let mut unremovable: Vec<(packages::Manager, String, &'static str)> = Vec::new();
    if packages.iter().any(|(m, _)| *m == packages::Manager::Flatpak) {
        let live = providers::flatpak_app_scopes();
        packages.retain(|(m, n)| {
            if *m != packages::Manager::Flatpak {
                return true;
            }
            let Some(live) = &live else {
                unremovable.push((
                    *m,
                    n.clone(),
                    "could not read which installation holds it",
                ));
                return false;
            };
            let mut holders = live.iter().filter(|(a, _)| a == n).peekable();
            if holders.peek().is_none() {
                // Installed according to one listing and in no installation
                // according to the other. Reported rather than dropped: silently
                // discarding an extra we cannot place is the same silent skip as
                // reading "could not ask" as "nothing there" (Principles #6, #12).
                unremovable.push((
                    *m,
                    n.clone(),
                    "installed, but no installation reports holding it",
                ));
                return false;
            }
            if holders.any(|(_, inst)| providers::flatpak_scope_flag(inst).is_some()) {
                true
            } else {
                unremovable.push((
                    *m,
                    n.clone(),
                    "in an installation temper does not manage",
                ));
                false
            }
        });
    }
    // A retired package is removed even though it is not an "extra": the spec
    // says it must not be here, which is a stronger statement than not
    // mentioning it. `[ignore]` deliberately does not silence this — ignoring is
    // "do not tell me about it", and retiring is "get rid of it".
    for token in crate::manifest::effective_retire_packages(home, machine)? {
        if let Ok(p) = packages::parse(&token) {
            let entry = (p.manager, p.match_name());
            if !packages.contains(&entry) {
                packages.push(entry);
            }
        }
    }
    // Split residue by whether it is still what temper left: removable, or
    // reportable. A ledger is a record, not a licence to delete a user's edits.
    let declared = deployed_paths(home, machine)?;
    let (mut residue, mut residue_edited) = (Vec::new(), Vec::new());
    for (key, rec) in crate::ledger::residue(&machine.name, &declared) {
        if !crate::ledger::still_present(&rec) {
            continue; // already gone — not work, and counting it would inflate the total
        }
        if crate::ledger::is_untouched(&rec) {
            residue.push(key);
        } else {
            residue_edited.push(key);
        }
    }

    // Trusted-but-undeclared taps → untrust (honors `[ignore].tap`). Skipped
    // without brew (`trusted_taps` → None), and skipped on a spec that says
    // nothing about taps (`trust_is_declared`).
    let mut untrust = Vec::new();
    if let (true, Some(trusted)) = (trust_is_declared(brew_trust), providers::trusted_taps()?) {
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
        rpm_ostree: providers::rpm_ostree_extras(&providers::effective_rpm(home, machine)?, ignore),
        flatpak_remotes: providers::remotes_extras(
            &providers::effective_remotes(home, machine)?,
            ignore,
        ),
        residue,
        residue_edited,
        retired: crate::manifest::effective_retire(home, machine)?
            .into_iter()
            .filter(|p| crate::manifest::expand_tilde(p).exists())
            .collect(),
        unavailable,
        unremovable,
    })
}

/// Apply a prune: uninstall the packages and `brew untrust` the taps.
/// Destructive — the caller previews and confirms first (`run_prune` computes
/// the plan without touching anything). Recomputes the effective set so `brew
/// bundle cleanup` keeps a declared package's transitive deps. A no-op on an
/// empty plan.
pub fn commit_prune(
    home: &Path,
    machine: &Machine,
    plan: &PrunePlan,
    ignore: &Ignore,
    brew_trust: &[String],
) -> Result<PruneOutcome> {
    // Uninstalling a pkg-based cask needs root per cask, same as installing one.
    // No `acquire` here: the plan is already confirmed and brew asks in its own
    // words on the first one — this just stops it asking again for the rest.
    let _sudo = crate::sudo::keep_alive();
    if !plan.packages.is_empty() {
        let effective = packages::effective_set(home, machine)?;
        // `ignore` and the declared trust both reach the executor because `brew
        // bundle cleanup` decides for itself what to remove *and* rewrites the
        // trust store from the same file: what temper leaves out, brew takes
        // away. Note the ordering below — this runs before `untrust_taps`, so a
        // wipe here could not be repaired by it either.
        providers::prune_apply(&effective, &plan.packages, ignore, brew_trust)?;
    }
    if !plan.untrust.is_empty() {
        providers::untrust_taps(&plan.untrust)?;
    }
    if !plan.extensions.is_empty() {
        providers::gext_uninstall(&plan.extensions)?;
    }
    // A retired target is as often a directory (`~/.config/old-app`) as a file,
    // and `sysfile` retires root-owned ones — `remove_file` alone failed both,
    // warned, and let the run report them removed.
    let mut failed: Vec<String> = Vec::new();
    for path in &plan.retired {
        let p = crate::manifest::expand_tilde(path);
        if let Err(e) = crate::ledger::remove_path(&p) {
            eprintln!(
                "{} could not remove {}: {e}",
                crate::ui::yellow(crate::ui::g_warn()),
                p.display()
            );
            failed.push(path.clone());
        }
    }
    // Residue carries how to remove it: a file is deleted, a block's region is
    // edited out of a file that stays.
    let mut recorded = crate::ledger::load(&machine.name);
    for key in &plan.residue {
        let Some(rec) = recorded.get(key).cloned() else {
            continue;
        };
        match crate::ledger::remove(&rec) {
            // What is gone stops being recorded. Leaving the entry made the next
            // `drift` report a file prune had just deleted, as one the user had
            // edited — red, permanent, and answerable by no verb.
            Ok(()) => {
                recorded.remove(key);
            }
            Err(e) => {
                eprintln!(
                    "{} could not remove {key}: {e}",
                    crate::ui::yellow(crate::ui::g_warn())
                );
                failed.push(key.clone());
            }
        }
    }
    let _ = crate::ledger::save(&machine.name, &recorded);
    providers::remotes_delete(&plan.flatpak_remotes)?;
    let reboot = providers::rpm_ostree_uninstall(&plan.rpm_ostree, false)?;
    Ok(PruneOutcome { reboot, failed })
}

/// What a prune actually did — as distinct from what it planned.
///
/// `prune` reported `plan.len()` removed regardless of what failed, so a
/// permission error or a missing tool still read as success. Principle #6b:
/// report the effect, never the invocation.
pub struct PruneOutcome {
    pub reboot: bool,
    /// Items that were planned, attempted, and are still here.
    pub failed: Vec<String>,
}

/// Snapshot: capture each declared `[[machine.dconf]]` subtree (filtered) into
/// its file — the spec←machine half of the capture/restore pair, and the
/// wholesale sibling of a per-key `reconcile`. A **recurring** verb: it is the
/// only way to update a snapshot wholesale.
///
/// Errors where `dconf` is absent rather than silently writing nothing.
/// Journaled, so `undo` reverts it.
pub fn run_snapshot(
    home: &Path,
    machine: &Machine,
    include_shared: bool,
) -> Result<Vec<std::path::PathBuf>> {
    // A machine that declares no subtrees of its own may still have extensions
    // that declare settings.
    if crate::dconf::all_snapshots(home, machine)?.is_empty() {
        return Ok(Vec::new());
    }
    if let crate::dconf::Store::Unreadable(why) = crate::dconf::observe() {
        bail!("cannot capture a dconf snapshot: {why}");
    }
    let mut journal = Journal::begin();
    let written = crate::dconf::capture(home, machine, include_shared, &mut journal)?;
    journal.commit()?;
    Ok(written)
}

/// Restore: load each declared dconf snapshot back into live dconf. The CLI
/// confirms first — this clobbers live desktop state (never run by `update`).
/// Journaled per subtree, so `undo` reverts it.
pub fn run_restore(home: &Path, machine: &Machine, dry_run: bool) -> Result<Vec<std::path::PathBuf>> {
    let loaded = crate::dconf::restore(home, machine, dry_run)?;
    // A `/org/gnome/shell/` snapshot carries `enabled-extensions` and
    // `disabled-extensions`, so a restore rewrites the very key the declaration
    // asserts — and whichever of `install` and `restore-dconf` you ran last won.
    // The declaration is the spec and the snapshot is a recording of the
    // machine, so the spec is re-asserted afterwards. Only for uuids temper
    // declares: the image-baked ones the snapshot restored are left exactly as
    // the snapshot put them.
    if !dry_run && !loaded.is_empty() {
        let specs = providers::effective_extension_specs(home, machine)?;
        let drift = providers::gext_enable_drift(&specs);
        if !drift.is_empty() {
            providers::gext_enable_converge(&drift, false)?;
        }
    }
    Ok(loaded)
}

/// Adopt (advisory v1): report the installed extras so they can be added to a
/// bundle, the machine loose list, or `[ignore]`. Non-mutating; interactive
/// folder-authoring is a later refinement.
pub fn run_adopt(
    home: &Path,
    machine: &Machine,
    ignore: &Ignore,
) -> Result<Vec<(packages::Manager, String)>> {
    package_extras(home, machine, ignore)
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
    let mut unrevertible: Vec<String> = Vec::new();
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
        if matches!(did, Applied::Changed | Applied::Ran) {
            if let Some(why) = unrevertible_reason(step) {
                let named = format!("{} — {why}", label.trim());
                journal.record_unrevertible(&named);
                unrevertible.push(named);
            }
        }
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
    if let Ok(l) = deployed_paths(home, machine) {
        let _ = crate::ledger::save(&machine.name, &l);
    }
    Ok(InstallReport {
        packages: effective.len(),
        upgraded,
        steps_changed: changed,
        steps_ran: ran,
        steps_total: total,
        reboot: false,
        skipped,
            unrevertible,
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
        let items = vec![f("brew-package", false), f("brew-package-extra", false)];
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
        //   - the dconf pair is chosen by a conditional, not a `kind: "…"` literal;
        //   - the package kinds come from `package_kind`, a match on the manager,
        //     so they are literals the scrape cannot see next to a `kind:`. The
        //     match is exhaustive over `Manager`, which is the stronger guarantee
        //     anyway: adding a manager forces a kind, and listing them here
        //     forces that kind to be registered.
        for k in [
            "brew-package",
            "brew-package-extra",
            "flatpak-package",
            "flatpak-package-extra",
            "mas-package",
            "mas-package-extra",
            "vscode-package",
            "vscode-package-extra",
            "dconf-key",
            "dconf-extra",
            "dconf-unavailable",
        ] {
            emitted.push(k.to_string());
        }
        // The assertion kinds come from `drift::kind`, which returns them rather
        // than writing them beside a `kind:` — so they are taken from the one
        // list that defines them instead of being copied here. A third copy is
        // how the set silently diverged before.
        for k in crate::drift::ASSERT_KINDS {
            emitted.push((*k).to_string());
        }
        emitted.push(crate::drift::ASSERT_UNKNOWN.to_string());
        emitted.sort();
        emitted.dedup();
        for k in &emitted {
            assert!(
                KIND_ANSWERS.iter().any(|spec| spec.name == k),
                "finding kind `{k}` has no entry in KIND_ANSWERS — say what resolves \
                 it (or that nothing does) before shipping the report"
            );
        }
        // …and the reverse: an entry for a kind nothing emits is dead config that
        // will quietly stop matching reality.
        for spec in KIND_ANSWERS {
            assert!(
                emitted.iter().any(|k| k == spec.name),
                "KIND_ANSWERS lists `{}`, which nothing emits any more — delete it",
                spec.name
            );
        }
        assert!(emitted.len() >= 15, "kind scrape found too few: {emitted:?}");
        // Honest limit: this reads the source for literal `kind: "…"`, so a kind
        // built from a variable or const is invisible to it. That has not
        // happened yet; if it does, the fix is to make `kind` a closed enum and
        // let the compiler enforce this instead of a scrape.
    }

    /// BOTH directions must be answered, for every kind.
    ///
    /// This is the test the old registry structurally could not have. It was a
    /// flat bag of verbs per kind, so `extension → [install]` — a kind with a
    /// converge answer and no spec-side answer at all — passed every check while
    /// `drift` told users to run `reconcile`, which had no code path for it.
    /// An empty cell is now a compile-shaped omission; an unhelpful one has to
    /// be written down as `Hand`/`NA` with a reason someone can argue with.
    #[test]
    fn every_kind_answers_both_directions() {
        for spec in KIND_ANSWERS {
            assert!(
                !spec.converge.is_empty(),
                "kind `{}` says nothing about changing the MACHINE",
                spec.name
            );
            assert!(
                !spec.absorb.is_empty(),
                "kind `{}` says nothing about changing the SPEC — if the answer is \
                 'a hand edit', say which file and why",
                spec.name
            );
            for a in spec.converge.iter().chain(spec.absorb) {
                if let Answer::NA(why) | Answer::Hand { why, .. } = a {
                    assert!(
                        why.len() > 12,
                        "kind `{}` declines a direction without a real reason",
                        spec.name
                    );
                }
            }
        }
    }

    /// `remediations` may never name a command the registry does not record for
    /// a kind that actually drifted.
    ///
    /// Only the registry→remediations direction was ever tested, so the reverse
    /// went unnoticed: `remediations` offered `temper reconcile` for `extension`,
    /// `rpm` and `dconf-uncaptured`, none of which reconcile can touch. Two
    /// encodings of one fact, and the untested direction is where they drifted.
    #[test]
    fn remediations_never_invent_a_verb_the_registry_lacks() {
        for spec in KIND_ANSWERS {
            let mut allowed = spec.converge_verbs();
            allowed.extend(spec.absorb_verbs());
            // Offered for any failing assertion, and never a repair claim.
            allowed.push("temper drift");
            // Paired with `install` as the revert of the same run.
            if allowed.contains(&"temper install") {
                allowed.push("temper undo");
            }
            for r in remediations(&[f(spec.name, false)]) {
                assert!(
                    allowed.contains(&r.command.as_str()),
                    "a `{}` finding makes drift offer `{}`, which the registry does \
                     not list for it — advice must be executable for the finding it names",
                    spec.name,
                    r.command
                );
            }
        }
    }

    /// Every kind the registry answers with a verb must actually produce that
    /// command from `remediations`. Catches an answer that was declared but
    /// never wired, and one whose wiring drifted from its declaration.
    #[test]
    fn registered_verbs_are_actually_emitted() {
        for spec in KIND_ANSWERS {
            let kind = spec.name;
            let mut wanted = spec.converge_verbs();
            wanted.extend(spec.absorb_verbs());
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
        assert!(cmds.contains(&"temper restore-dconf".to_string())); // spec → machine
        assert!(cmds.contains(&"temper reconcile".to_string())); // spec ← machine, per key
        assert!(cmds.contains(&"temper snapshot-dconf".to_string())); // spec ← machine, wholesale
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
        assert!(!cmds.contains(&"temper restore-dconf".to_string()));
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
        let items = vec![f("copy", true), f("brew-package", true)];
        assert!(remediations(&items).is_empty());
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    fn step_from(toml_src: &str) -> Step {
        #[derive(serde::Deserialize)]
        struct Bundle {
            step: Vec<Step>,
        }
        toml::from_str::<Bundle>(toml_src)
            .unwrap()
            .step
            .pop()
            .unwrap()
    }

    /// A `profile`'s apply is a System Settings window, so the routine upgrade path
    /// must not run it: since a missing profile is real drift, an `always` default
    /// would re-open that dialog on every `temper update` until the user gave in.
    /// `drift` reports it and `install` re-offers it instead.
    #[test]
    fn a_profile_is_install_only_so_update_never_pops_a_dialog() {
        let p = step_from("[[step]]\nprofile = \"assets/x.mobileconfig\"\n");
        assert_eq!(lifecycle(&p), "install");
        // …and `update`'s own filter agrees, which is the property that matters.
        assert!(!matches!(lifecycle(&p), "always" | "ensure"));
    }

    #[test]
    fn an_explicit_run_still_wins_over_the_default() {
        let p = step_from("[[step]]\nprofile = \"assets/x.mobileconfig\"\nrun = \"always\"\n");
        assert_eq!(lifecycle(&p), "always");
    }

    #[test]
    fn a_copy_is_still_always() {
        let c = step_from("[[step]]\ncopy = \"assets/x\"\nto = \"~/x\"\n");
        assert_eq!(lifecycle(&c), "always");
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
