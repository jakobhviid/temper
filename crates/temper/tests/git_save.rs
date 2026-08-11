//! Proves the git convenience layer: `configure set git.*` writes `[git]`, `save`
//! commits a git-backed home, and a non-git home stays dormant (save errors,
//! `status` reports dormant). Requires `git` on PATH (present in CI).

use std::fs;
use std::path::Path;
use std::process::Command as Proc;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn os() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    }
}

/// A sandbox for the child: its own HOME and state dir.
///
/// This file had neither. `journal::state_dir()` falls back to
/// `HOME/.local/state` when `TEMPER_STATE_DIR` is unset, so every temper here
/// read — and any journaling verb would have written — the developer's real
/// state directory. The `env_remove("TEMPER_DIR_UNUSED")` it did carry names no
/// variable temper has ever read.
struct Sandbox {
    home: TempDir,
    state: TempDir,
}

fn sandbox() -> Sandbox {
    Sandbox {
        home: TempDir::new().unwrap(),
        state: TempDir::new().unwrap(),
    }
}

fn temper_in(home: &Path, sb: &Sandbox) -> Command {
    let mut c = Command::cargo_bin("temper").unwrap();
    c.env("TEMPER_DIR", home)
        .env("HOME", sb.home.path())
        .env("XDG_CONFIG_HOME", sb.home.path().join(".config"))
        .env("TEMPER_STATE_DIR", sb.state.path())
        // Colour is gated on stdout being a terminal, so under a pty every
        // string assertion in this file would be matching against ANSI escapes.
        .env("NO_COLOR", "1")
        // temper shells out to git, so the *child* inherits the developer's
        // global config unless it is held off here. Measured against a hostile
        // config: `commit.gpgsign = true` makes temper's commit fail outright
        // ("failed to write commit object" — there is no TTY for pinentry), and
        // a `core.excludesFile` matching a test's filename makes the dirty file
        // invisible so temper truthfully reports "nothing to commit" and the
        // assertion fails. Neither is a bug in temper.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t");
    c
}

/// git, with the developer's own configuration held off.
///
/// `git()` inherited the whole environment. A global `commit.gpgsign = true`
/// makes `git commit` here fail with no TTY for pinentry; a global
/// `core.excludesFile` matching a test's filename breaks the dirty-tree
/// assertions; and `init.defaultBranch` decides a branch name two tests
/// compare against — the last of which cost a published release once, which is
/// why AGENTS.md prescribes running the suite with `GIT_CONFIG_GLOBAL=/dev/null`.
/// A test should not need the caller to remember.
fn git(dir: &Path, args: &[&str]) {
    let out = Proc::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_home() -> TempDir {
    let d = TempDir::new().unwrap();
    // `--initial-branch`: a stock git defaults to `master`, and two tests below
    // compare the branch name. Leaving it to the host's `init.defaultBranch` is
    // what let a green local run publish a release that failed on CI.
    git(d.path(), &["init", "-q", "--initial-branch=main"]);
    git(d.path(), &["config", "user.email", "t@t"]);
    git(d.path(), &["config", "user.name", "t"]);
    fs::write(
        d.path().join("temper.toml"),
        format!("[[machine]]\nname = \"t\"\nos = \"{}\"\n", os()),
    )
    .unwrap();
    git(d.path(), &["add", "-A"]);
    git(d.path(), &["commit", "-qm", "init"]);
    d
}

#[test]
fn configure_writes_git_toggles() {
    let sb = sandbox();
    let h = git_home();
    temper_in(h.path(), &sb)
        .args(["configure", "set", "git.auto_commit", "true"])
        .assert()
        .success();
    temper_in(h.path(), &sb)
        .args(["configure", "set", "git.auto_push", "true"])
        .assert()
        .success();
    let toml = fs::read_to_string(h.path().join("temper.toml")).unwrap();
    assert!(toml.contains("[git]"));
    assert!(toml.contains("auto_commit = true"));
    assert!(toml.contains("auto_push = true"));

    // Turning one back off writes false (unset would drop the line entirely).
    temper_in(h.path(), &sb)
        .args(["configure", "set", "git.auto_commit", "false"])
        .assert()
        .success();
    let toml = fs::read_to_string(h.path().join("temper.toml")).unwrap();
    assert!(toml.contains("auto_commit = false"));
}

#[test]
fn save_commits_a_dirty_git_home() {
    let sb = sandbox();
    let h = git_home();
    fs::write(h.path().join("newfile.txt"), "hi\n").unwrap(); // dirty the repo
    temper_in(h.path(), &sb)
        .args(["save", "-m", "test save", "--no-push"])
        .assert()
        .success()
        .stdout(predicates::str::contains("committed: test save"));
    // clean now
    temper_in(h.path(), &sb)
        .args(["save", "--no-push"])
        .assert()
        .success()
        .stdout(predicates::str::contains("nothing to commit"));
}

#[test]
fn non_git_home_is_dormant() {
    let sb = sandbox();
    let plain = TempDir::new().unwrap();
    fs::write(
        plain.path().join("temper.toml"),
        format!("[[machine]]\nname = \"t\"\nos = \"{}\"\n", os()),
    )
    .unwrap();
    temper_in(plain.path(), &sb)
        .arg("save")
        .assert()
        .failure()
        .stderr(predicates::str::contains("not a git repo"));
    // `status` reports the home dormant, doesn't error
    temper_in(plain.path(), &sb)
        .arg("status")
        .assert()
        .success()
        .stdout(predicates::str::contains("dormant"));
}

/// A pull must report what it *did*, not that it ran. Two commits waiting
/// upstream produce one line naming them; a pull that moves nothing says nothing
/// (during a converge, "already current" is git's business, not the user's).
#[test]
fn auto_pull_reports_commits_that_landed_then_stays_quiet() {
    let sb = sandbox();
    let origin = TempDir::new().unwrap();
    // `--initial-branch` pins the bare repo's HEAD: without it the branch name
    // comes from the machine's `init.defaultBranch` (`master` on a stock git,
    // `main` on many developers'), and a clone of a repo whose HEAD names a
    // nonexistent branch checks nothing out at all.
    git(origin.path(), &["init", "-q", "--bare", "--initial-branch=main"]);

    // The home: a clone that declares auto_pull.
    let home = TempDir::new().unwrap();
    let h = home.path();
    let url = origin.path().to_string_lossy().to_string();
    assert!(Proc::new("git")
        .args(["clone", "-q", &url])
        .arg(h)
        .output()
        .unwrap()
        .status
        .success());
    git(h, &["config", "user.email", "t@t"]);
    git(h, &["config", "user.name", "t"]);
    fs::write(
        h.join("temper.toml"),
        format!(
            "[git]\nauto_pull = true\n\n[[machine]]\nname = \"t\"\nos = \"{}\"\n",
            os()
        ),
    )
    .unwrap();
    git(h, &["add", "-A"]);
    git(h, &["commit", "-qm", "init"]);
    // Whatever `init.defaultBranch` produced, this branch is `main` from here on.
    git(h, &["branch", "-M", "main"]);
    git(h, &["push", "-q", "-u", "origin", "main"]);

    // A second clone pushes two commits.
    let other = TempDir::new().unwrap();
    assert!(Proc::new("git")
        .args(["clone", "-q", &url])
        .arg(other.path())
        .output()
        .unwrap()
        .status
        .success());
    let o = other.path();
    git(o, &["config", "user.email", "t@t"]);
    git(o, &["config", "user.name", "t"]);
    for (n, f) in [("one", "a.txt"), ("two", "b.txt")] {
        fs::write(o.join(f), format!("{n}\n")).unwrap();
        git(o, &["add", "-A"]);
        git(o, &["commit", "-qm", n]);
    }
    git(o, &["push", "-q"]);

    // The pre-run pull lands both and says so, with the count.
    temper_in(h, &sb)
        .args(["install", "--dry-run"])
        .assert()
        .success()
        .stdout(predicates::str::contains("spec updated (2 commits)"));

    // Nothing new the second time → no git line at all.
    temper_in(h, &sb)
        .args(["install", "--dry-run"])
        .assert()
        .success()
        .stdout(predicates::str::contains("spec updated").not());
}

/// `pushed` must mean the remote moved, not that a push was attempted: `git push`
/// with nothing to send exits 0 ("Everything up-to-date"), and reporting a push
/// there is the same defect as letting a tool's no-op stand as temper's verdict.
#[test]
fn save_claims_a_push_only_when_the_remote_moved() {
    let sb = sandbox();
    let origin = TempDir::new().unwrap();
    // `--initial-branch` pins the bare repo's HEAD: without it the branch name
    // comes from the machine's `init.defaultBranch` (`master` on a stock git,
    // `main` on many developers'), and a clone of a repo whose HEAD names a
    // nonexistent branch checks nothing out at all.
    git(origin.path(), &["init", "-q", "--bare", "--initial-branch=main"]);
    let home = TempDir::new().unwrap();
    let h = home.path();
    let url = origin.path().to_string_lossy().to_string();
    assert!(Proc::new("git")
        .args(["clone", "-q", &url])
        .arg(h)
        .output()
        .unwrap()
        .status
        .success());
    git(h, &["config", "user.email", "t@t"]);
    git(h, &["config", "user.name", "t"]);
    fs::write(
        h.join("temper.toml"),
        format!("[[machine]]\nname = \"t\"\nos = \"{}\"\n", os()),
    )
    .unwrap();
    git(h, &["add", "-A"]);
    git(h, &["commit", "-qm", "init"]);
    // Whatever `init.defaultBranch` produced, this branch is `main` from here on.
    git(h, &["branch", "-M", "main"]);
    git(h, &["push", "-q", "-u", "origin", "main"]);

    // Dirty tree → a real commit and a real push.
    fs::write(h.join("note.txt"), "hi\n").unwrap();
    temper_in(h, &sb)
        .arg("save")
        .assert()
        .success()
        .stdout(predicates::str::contains("pushed"));

    // Clean tree, remote already current → nothing was pushed, so don't say it was.
    temper_in(h, &sb)
        .arg("save")
        .assert()
        .success()
        .stdout(predicates::str::contains("nothing to commit"))
        .stdout(predicates::str::contains("pushed").not());
}
