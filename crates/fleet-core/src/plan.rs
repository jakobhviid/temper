//! Build the ordered steps + assertions for a machine, then evaluate (drift) or
//! apply (install) them. Live step primitives: `copy`, `block`, `setkey(json)`.
//! Assertions are drift-only.
//!
//! Flows: `install` applies every step (with a `dry_run` preview); `update` /
//! `ensure` / `adopt` land later.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Result};

use crate::journal::Journal;
use crate::manifest::{self, expand_tilde, Assert, Machine, Step};
use crate::primitives::{self, CopyOpts, FileState};
use crate::drift;

fn os_gated(step_os: &Option<String>, machine: &Machine) -> bool {
    match step_os {
        Some(os) => os != &machine.os,
        None => false,
    }
}

/// Everything a machine composes, OS-gated to this host.
pub struct Resolved {
    pub steps: Vec<(String, Step)>,   // (app, step)
    pub asserts: Vec<(String, Assert)>, // (app, assert)
}

pub fn resolve(home: &Path, machine: &Machine) -> Result<Resolved> {
    let mut steps = Vec::new();
    let mut asserts = Vec::new();
    for app in &machine.apps {
        let bundle = manifest::load_bundle(home, app)?;
        for step in bundle.step {
            if !os_gated(&step.os, machine) {
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

/// One drift finding across any primitive or assertion.
pub struct Finding {
    pub app: String,
    pub kind: &'static str,
    pub target: String,
    pub ok: bool,
    pub status: String,
}

impl Finding {
    fn from_state(app: &str, kind: &'static str, target: String, state: FileState) -> Finding {
        Finding {
            app: app.to_string(),
            kind,
            target,
            ok: state.is_ok(),
            status: state.label().to_string(),
        }
    }
}

/// Drift a single step, if it's one we evaluate. Returns None for empty steps.
fn step_finding(
    home: &Path,
    app: &str,
    step: &Step,
    vars: &BTreeMap<String, String>,
) -> Result<Option<Finding>> {
    if let (Some(copy), Some(to)) = (&step.copy, &step.to) {
        let state = primitives::copy_state(&home.join(copy), &expand_tilde(to), &copy_opts(step, vars))?;
        return Ok(Some(Finding::from_state(app, "copy", to.clone(), state)));
    }
    if let (Some(block), Some(in_file)) = (&step.block, &step.in_file) {
        let marker = step.marker.as_deref().unwrap_or("block");
        let state = primitives::block_state(&home.join(block), &expand_tilde(in_file), marker)?;
        return Ok(Some(Finding::from_state(app, "block", in_file.clone(), state)));
    }
    if let Some(sk) = &step.setkey {
        let state = primitives::setkey_state(sk)?;
        let target = format!("{}:{}", sk.file.as_deref().unwrap_or(&sk.backend), sk.key);
        return Ok(Some(Finding::from_state(app, "setkey", target, state)));
    }
    Ok(None)
}

pub fn run_drift(
    home: &Path,
    machine: &Machine,
    vars: &BTreeMap<String, String>,
) -> Result<Vec<Finding>> {
    let resolved = resolve(home, machine)?;
    let mut findings = Vec::new();
    for (app, step) in &resolved.steps {
        if let Some(f) = step_finding(home, app, step, vars)? {
            findings.push(f);
        }
    }
    for (app, assert) in &resolved.asserts {
        let (ok, status) = drift::eval(assert)?;
        findings.push(Finding {
            app: app.clone(),
            kind: drift::kind(assert),
            target: drift::target(assert),
            ok,
            status,
        });
    }
    Ok(findings)
}

/// Apply a single step. Returns whether it changed anything.
fn apply_step(
    home: &Path,
    step: &Step,
    vars: &BTreeMap<String, String>,
    journal: &mut Journal,
) -> Result<bool> {
    if let (Some(copy), Some(to)) = (&step.copy, &step.to) {
        return primitives::copy_apply(&home.join(copy), &expand_tilde(to), &copy_opts(step, vars), journal);
    }
    if let (Some(block), Some(in_file)) = (&step.block, &step.in_file) {
        let marker = step.marker.as_deref().unwrap_or("block");
        return primitives::block_apply(&home.join(block), &expand_tilde(in_file), marker, journal);
    }
    if let Some(sk) = &step.setkey {
        return primitives::setkey_apply(sk, journal);
    }
    bail!("step names no known primitive (copy / block / setkey)")
}

/// Would this step change anything? (dry-run preview.)
fn step_would_change(home: &Path, step: &Step, vars: &BTreeMap<String, String>) -> Result<bool> {
    Ok(step_finding(home, "", step, vars)?.map_or(false, |f| !f.ok))
}

/// Apply every step (install flow). With `dry_run`, report what would change
/// without writing or journaling. Returns (changed, total) mutating steps.
pub fn run_install(
    home: &Path,
    machine: &Machine,
    vars: &BTreeMap<String, String>,
    dry_run: bool,
) -> Result<(usize, usize)> {
    let resolved = resolve(home, machine)?;
    let mut journal = Journal::begin();
    let (mut changed, mut total) = (0usize, 0usize);
    for (_app, step) in &resolved.steps {
        // Only steps that name a primitive are mutating; skip anything else.
        let is_step = step.copy.is_some() || step.block.is_some() || step.setkey.is_some();
        if !is_step {
            continue;
        }
        total += 1;
        if dry_run {
            if step_would_change(home, step, vars)? {
                changed += 1;
            }
        } else if apply_step(home, step, vars, &mut journal)? {
            changed += 1;
        }
    }
    if !dry_run {
        journal.commit()?;
    }
    Ok((changed, total))
}
