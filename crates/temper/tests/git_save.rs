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

fn temper(home: &Path) -> Command {
    let mut c = Command::cargo_bin("temper").unwrap();
    c.env("TEMPER_DIR", home).env_remove("TEMPER_DIR_UNUSED");
    c
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Proc::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

fn git_home() -> TempDir {
    let d = TempDir::new().unwrap();
    git(d.path(), &["init", "-q"]);
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
    let h = git_home();
    temper(h.path())
        .args(["configure", "set", "git.auto_commit", "true"])
        .assert()
        .success();
    temper(h.path())
        .args(["configure", "set", "git.auto_push", "true"])
        .assert()
        .success();
    let toml = fs::read_to_string(h.path().join("temper.toml")).unwrap();
    assert!(toml.contains("[git]"));
    assert!(toml.contains("auto_commit = true"));
    assert!(toml.contains("auto_push = true"));

    // Turning one back off writes false (unset would drop the line entirely).
    temper(h.path())
        .args(["configure", "set", "git.auto_commit", "false"])
        .assert()
        .success();
    let toml = fs::read_to_string(h.path().join("temper.toml")).unwrap();
    assert!(toml.contains("auto_commit = false"));
}

#[test]
fn save_commits_a_dirty_git_home() {
    let h = git_home();
    fs::write(h.path().join("newfile.txt"), "hi\n").unwrap(); // dirty the repo
    temper(h.path())
        .args(["save", "-m", "test save", "--no-push"])
        .assert()
        .success()
        .stdout(predicates::str::contains("committed: test save"));
    // clean now
    temper(h.path())
        .args(["save", "--no-push"])
        .assert()
        .success()
        .stdout(predicates::str::contains("nothing to commit"));
}

#[test]
fn non_git_home_is_dormant() {
    let plain = TempDir::new().unwrap();
    fs::write(
        plain.path().join("temper.toml"),
        format!("[[machine]]\nname = \"t\"\nos = \"{}\"\n", os()),
    )
    .unwrap();
    temper(plain.path())
        .arg("save")
        .assert()
        .failure()
        .stderr(predicates::str::contains("not a git repo"));
    // `status` reports the home dormant, doesn't error
    temper(plain.path())
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
    let origin = TempDir::new().unwrap();
    git(origin.path(), &["init", "-q", "--bare"]);

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
    git(h, &["push", "-q", "-u", "origin", "HEAD:main"]);
    git(h, &["branch", "-q", "--set-upstream-to=origin/main"]);

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
    temper(h)
        .args(["install", "--dry-run"])
        .assert()
        .success()
        .stdout(predicates::str::contains("spec updated (2 commits)"));

    // Nothing new the second time → no git line at all.
    temper(h)
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
    let origin = TempDir::new().unwrap();
    git(origin.path(), &["init", "-q", "--bare"]);
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
    git(h, &["push", "-q", "-u", "origin", "HEAD:main"]);
    git(h, &["branch", "-q", "--set-upstream-to=origin/main"]);

    // Dirty tree → a real commit and a real push.
    fs::write(h.join("note.txt"), "hi\n").unwrap();
    temper(h)
        .arg("save")
        .assert()
        .success()
        .stdout(predicates::str::contains("pushed"));

    // Clean tree, remote already current → nothing was pushed, so don't say it was.
    temper(h)
        .arg("save")
        .assert()
        .success()
        .stdout(predicates::str::contains("nothing to commit"))
        .stdout(predicates::str::contains("pushed").not());
}
