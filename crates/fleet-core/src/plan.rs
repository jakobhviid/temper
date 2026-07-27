//! Build the ordered set of steps for a machine, then evaluate (drift) or apply
//! (install) them. Live: `copy` steps (verbatim/template/seed/mode). Other
//! primitives plug into the same resolve → {drift, apply+journal} shape.
//!
//! Flows: `install` applies everything (with a `dry_run` preview); `update` /
//! `ensure` / `adopt` land later.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::journal::Journal;
use crate::manifest::{self, expand_tilde, Machine, Step};
use crate::primitives::{copy_apply, copy_state, CopyOpts, FileState};

/// A step resolved to the app it came from.
pub struct Resolved {
    pub app: String,
    pub step: Step,
}

/// Gather every step for a machine's composed apps, dropping OS-gated ones that
/// don't apply here.
pub fn resolve_steps(home: &Path, machine: &Machine) -> Result<Vec<Resolved>> {
    let mut out = Vec::new();
    for app in &machine.apps {
        let bundle = manifest::load_bundle(home, app)?;
        for step in bundle.step {
            if let Some(os) = &step.os {
                if os != &machine.os {
                    continue;
                }
            }
            out.push(Resolved {
                app: app.clone(),
                step,
            });
        }
    }
    Ok(out)
}

fn copy_opts<'a>(step: &'a Step, vars: &'a BTreeMap<String, String>) -> CopyOpts<'a> {
    CopyOpts {
        template: step.template,
        seed: step.seed,
        mode: step.mode.as_deref(),
        vars,
    }
}

/// One drift finding.
pub struct DriftItem {
    pub app: String,
    pub target: String,
    pub state: FileState,
}

pub fn run_drift(
    home: &Path,
    machine: &Machine,
    vars: &BTreeMap<String, String>,
) -> Result<Vec<DriftItem>> {
    let mut items = Vec::new();
    for r in resolve_steps(home, machine)? {
        if let (Some(copy), Some(to)) = (&r.step.copy, &r.step.to) {
            let state = copy_state(&home.join(copy), &expand_tilde(to), &copy_opts(&r.step, vars))?;
            items.push(DriftItem {
                app: r.app,
                target: to.clone(),
                state,
            });
        }
    }
    Ok(items)
}

/// Apply every step (install flow). With `dry_run`, report what would change
/// without writing or journaling. Returns (changed, total) copy steps.
pub fn run_install(
    home: &Path,
    machine: &Machine,
    vars: &BTreeMap<String, String>,
    dry_run: bool,
) -> Result<(usize, usize)> {
    let mut journal = Journal::begin();
    let (mut changed, mut total) = (0usize, 0usize);
    for r in resolve_steps(home, machine)? {
        if let (Some(copy), Some(to)) = (&r.step.copy, &r.step.to) {
            total += 1;
            let src = home.join(copy);
            let target = expand_tilde(to);
            let opts = copy_opts(&r.step, vars);
            if dry_run {
                if !copy_state(&src, &target, &opts)?.is_ok() {
                    changed += 1;
                }
            } else if copy_apply(&src, &target, &opts, &mut journal)? {
                changed += 1;
            }
        }
    }
    if !dry_run {
        journal.commit()?;
    }
    Ok((changed, total))
}
