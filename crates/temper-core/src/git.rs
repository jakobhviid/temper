//! Optional git convenience for a git-backed temper-home: detect, pull (warn,
//! never abort), commit temper's own writes with an auto message, and push.
//!
//! Principle #9 stands — temper does NOT *manage* the folder's sync. This only
//! persists temper's own writes when the home happens to be git and the user
//! opts in past the reminder. Everything here is a silent no-op on a non-git
//! folder. All shell-outs capture their output (never leak to stdout, so
//! `--json` stays clean); warnings go to the caller as values.

use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use crate::primitives::which;

/// Write the `[git]` table in `temper.toml` (comment-preserving, toml_edit).
/// Backs `temper git enable/disable`.
pub fn write_config(
    home: &Path,
    remind: bool,
    auto_commit: bool,
    auto_push: bool,
    auto_pull: bool,
    auto_rebase: bool,
) -> Result<()> {
    let p = home.join("temper.toml");
    let s = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    let mut doc: toml_edit::DocumentMut = s.parse().context("parsing temper.toml")?;
    let git = doc
        .as_table_mut()
        .entry("git")
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| anyhow!("[git] in temper.toml is not a table"))?;
    git["remind"] = toml_edit::value(remind);
    git["auto_commit"] = toml_edit::value(auto_commit);
    git["auto_push"] = toml_edit::value(auto_push);
    git["auto_pull"] = toml_edit::value(auto_pull);
    git["auto_rebase"] = toml_edit::value(auto_rebase);
    std::fs::write(&p, doc.to_string()).with_context(|| format!("writing {}", p.display()))?;
    Ok(())
}

/// The last non-empty line of some git stderr — a terse reason for a warning.
fn reason(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("failed")
        .trim()
        .to_string()
}

fn git(home: &Path, args: &[&str]) -> Option<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(home)
        .args(args)
        .output()
        .ok()
}

/// Is `home` inside a git work tree (and is `git` installed)?
pub fn is_repo(home: &Path) -> bool {
    if which("git").is_none() {
        return false;
    }
    git(home, &["rev-parse", "--is-inside-work-tree"])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Does the work tree have uncommitted changes (staged, unstaged, or untracked)?
pub fn is_dirty(home: &Path) -> bool {
    git(home, &["status", "--porcelain"])
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

/// The changed paths (porcelain), for building a `save` commit message.
pub fn changed_paths(home: &Path) -> Vec<String> {
    git(home, &["status", "--porcelain"])
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                // porcelain line: "XY path" — take the path (last whitespace field).
                .filter_map(|l| l.get(3..).map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// A short status line for `temper git`: branch + ahead/behind + dirty.
pub fn status_line(home: &Path) -> String {
    let branch = git(home, &["rev-parse", "--abbrev-ref", "HEAD"])
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "?".into());
    let ahead_behind = git(
        home,
        &["rev-list", "--left-right", "--count", "@{u}...HEAD"],
    )
    .filter(|o| o.status.success())
    .map(|o| {
        let s = String::from_utf8_lossy(&o.stdout);
        let mut it = s.split_whitespace();
        let behind: u32 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        let ahead: u32 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        format!("↓{behind} ↑{ahead}")
    })
    .unwrap_or_else(|| "no upstream".into());
    let dirty = if is_dirty(home) { "dirty" } else { "clean" };
    format!("{branch} ({ahead_behind}) — {dirty}")
}

/// Outcome of a best-effort pull.
pub enum Pull {
    /// Up to date or fast-forwarded.
    Ok,
    /// Couldn't pull — carries a short human reason (offline, diverged, dirty…).
    Warn(String),
    /// Not a git repo (or git absent) — nothing to do.
    NotRepo,
}

/// `git pull` the home — `--rebase` when `rebase`, else `--ff-only` (the safe
/// default). Never aborts the caller — a failure is a `Warn` with the reason, so
/// a run continues on a possibly-stale spec.
pub fn pull(home: &Path, rebase: bool) -> Pull {
    if !is_repo(home) {
        return Pull::NotRepo;
    }
    let mode = if rebase { "--rebase" } else { "--ff-only" };
    match git(home, &["pull", mode]) {
        Some(o) if o.status.success() => Pull::Ok,
        Some(o) => Pull::Warn(reason(&o.stderr)),
        None => Pull::Warn("could not run git".into()),
    }
}

/// What a `save`/auto-commit did.
pub struct SaveReport {
    pub committed: bool,
    pub pushed: bool,
    pub message: String,
    /// A non-fatal warning (e.g. pull/push couldn't complete).
    pub warning: Option<String>,
}

/// Stage everything, commit with `message`, and (if `push`) push — pulling
/// first (`--rebase` when `rebase`, else `--ff-only`) so the push isn't
/// rejected by a diverged remote. A clean tree is not an error
/// (`committed=false`). Errors only on a genuine git failure at commit time.
pub fn save(home: &Path, message: &str, push: bool, rebase: bool) -> Result<SaveReport> {
    if !is_repo(home) {
        bail!("{} is not a git repo — nothing to save", home.display());
    }
    let mut warning = None;

    if !is_dirty(home) {
        // Nothing local to commit; still offer to push if we're ahead.
        if push {
            if let Pull::Warn(w) = pull(home, rebase) {
                warning = Some(w);
            }
            let _ = git(home, &["push"]);
        }
        return Ok(SaveReport {
            committed: false,
            pushed: push,
            message: message.to_string(),
            warning,
        });
    }

    // Stage + commit.
    let added = git(home, &["add", "-A"])
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !added {
        bail!("git add failed in {}", home.display());
    }
    let commit = git(home, &["commit", "-m", message]);
    let committed = commit.as_ref().map(|o| o.status.success()).unwrap_or(false);
    if !committed {
        let stderr = commit.map(|o| o.stderr).unwrap_or_default();
        bail!("git commit failed: {}", reason(&stderr));
    }

    // Push, pulling first so a diverged remote doesn't reject us.
    let mut pushed = false;
    if push {
        if let Pull::Warn(w) = pull(home, rebase) {
            warning = Some(w);
        }
        match git(home, &["push"]) {
            Some(o) if o.status.success() => pushed = true,
            Some(o) => {
                warning = Some(format!("committed, but push failed: {}", reason(&o.stderr)));
            }
            None => warning = Some("committed, but could not run git push".into()),
        }
    }
    Ok(SaveReport {
        committed,
        pushed,
        message: message.to_string(),
        warning,
    })
}

/// Build a commit message from the changed paths (for a `save` with no verb
/// context / after hand edits): `spec update: a, b, c (+N more)`.
pub fn message_from_changes(home: &Path) -> String {
    let paths = changed_paths(home);
    if paths.is_empty() {
        return "temper: save spec".into();
    }
    let shown: Vec<&str> = paths.iter().take(3).map(String::as_str).collect();
    let more = paths.len().saturating_sub(shown.len());
    if more > 0 {
        format!("spec update: {} (+{more} more)", shown.join(", "))
    } else {
        format!("spec update: {}", shown.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_takes_last_nonempty_line() {
        assert_eq!(reason(b"fatal: no upstream\n\n"), "fatal: no upstream");
        assert_eq!(reason(b"line one\nline two\n"), "line two");
        assert_eq!(reason(b"   \n\n"), "failed");
    }

    #[test]
    fn non_repo_is_dormant() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(!is_repo(dir.path())); // a plain dir isn't a git repo
        assert!(!is_dirty(dir.path()));
        assert!(save(dir.path(), "x", false, false).is_err());
    }
}
