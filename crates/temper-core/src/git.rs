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

use anyhow::{bail, Result};

use crate::primitives::which;

// The `[git]` table is written by `temper configure set git.*` (see
// `crate::settings`), which stamps the version too — this module only reads git
// state and performs git operations.

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

/// Outcome of a best-effort pull — the *effect*, not "we ran git".
pub enum Pull {
    /// Already current: the pull succeeded and moved nothing.
    UpToDate,
    /// Fast-forwarded or rebased onto new upstream work — how many commits landed.
    Updated(u32),
    /// Couldn't pull — carries a short human reason (offline, diverged, dirty…).
    Warn(String),
    /// Not a git repo (or git absent) — nothing to do.
    NotRepo,
}

/// The current commit, for measuring what a pull moved.
fn head(home: &Path) -> Option<String> {
    git(home, &["rev-parse", "HEAD"])
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// `git pull` the home — `--rebase` when `rebase`, else `--ff-only` (the safe
/// default). Never aborts the caller — a failure is a `Warn` with the reason, so
/// a run continues on a possibly-stale spec.
///
/// The effect is measured by comparing HEAD before and after and counting the
/// commits between them, **never** by reading git's own report: "Already up to
/// date." is localized (`LANG=da_DK` says something else entirely), so matching it
/// would work on the author's machine and quietly stop working on someone else's.
pub fn pull(home: &Path, rebase: bool) -> Pull {
    if !is_repo(home) {
        return Pull::NotRepo;
    }
    let before = head(home);
    let mode = if rebase { "--rebase" } else { "--ff-only" };
    match git(home, &["pull", mode]) {
        Some(o) if o.status.success() => {
            let after = head(home);
            match (before, after) {
                (Some(b), Some(a)) if b != a => {
                    let n = git(home, &["rev-list", "--count", &format!("{b}..{a}")])
                        .filter(|o| o.status.success())
                        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
                        // HEAD moved but the range doesn't count (a rebase replayed
                        // local work): "something landed" is still true and useful.
                        .unwrap_or(1);
                    Pull::Updated(n)
                }
                _ => Pull::UpToDate,
            }
        }
        Some(o) => Pull::Warn(reason(&o.stderr)),
        None => Pull::Warn("could not run git".into()),
    }
}

/// What a `save`/auto-commit did.
pub struct SaveReport {
    pub committed: bool,
    /// Whether a push **succeeded and moved the remote**. Not "we ran push":
    /// `git push` with nothing to send exits 0 saying "Everything up-to-date", and
    /// claiming a push there is the same defect as a child's "nothing to update"
    /// standing in for temper's verdict.
    pub pushed: bool,
    pub message: String,
    /// A non-fatal warning (e.g. pull/push couldn't complete).
    pub warning: Option<String>,
}

/// Push, reporting whether the remote actually moved. The remote ref's commit is
/// compared before and after, so "up to date" and "pushed" are distinguishable
/// without reading git's (localized) prose. A failure is a warning string.
fn push(home: &Path) -> (bool, Option<String>) {
    let upstream = || {
        git(home, &["rev-parse", "@{u}"])
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let before = upstream();
    match git(home, &["push"]) {
        Some(o) if o.status.success() => {
            let after = upstream();
            // Both known and different → the remote moved. Unknown upstream (a
            // brand-new branch push) counts as moved: the push succeeded and there
            // was nothing there before.
            let moved = match (&before, &after) {
                (Some(b), Some(a)) => b != a,
                (None, Some(_)) => true,
                _ => false,
            };
            (moved, None)
        }
        Some(o) => (false, Some(format!("push failed: {}", reason(&o.stderr)))),
        None => (false, Some("could not run git push".into())),
    }
}

/// Stage everything, commit with `message`, and (if `push`) push — pulling
/// first (`--rebase` when `rebase`, else `--ff-only`) so the push isn't
/// rejected by a diverged remote. A clean tree is not an error
/// (`committed=false`). Errors only on a genuine git failure at commit time.
pub fn save(home: &Path, message: &str, push_it: bool, rebase: bool) -> Result<SaveReport> {
    if !is_repo(home) {
        bail!("{} is not a git repo — nothing to save", home.display());
    }
    let mut warning = None;

    if !is_dirty(home) {
        // Nothing local to commit; still offer to push if we're ahead.
        let mut pushed = false;
        if push_it {
            if let Pull::Warn(w) = pull(home, rebase) {
                warning = Some(w);
            }
            // Was `let _ = git(&["push"])` with `pushed: push_it` returned — i.e.
            // "we intended to push", which printed `✓ pushed` on a clean tree with
            // nothing to send, and even when the push failed outright.
            let (moved, w) = push(home);
            pushed = moved;
            warning = warning.or(w);
        }
        return Ok(SaveReport {
            committed: false,
            pushed,
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
    if push_it {
        if let Pull::Warn(w) = pull(home, rebase) {
            warning = Some(w);
        }
        let (moved, w) = push(home);
        pushed = moved;
        if let Some(w) = w {
            warning = Some(format!("committed, but {w}"));
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
