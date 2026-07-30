//! Proves the git convenience layer: `configure set git.*` writes `[git]`, `save`
//! commits a git-backed home, and a non-git home stays dormant (save errors,
//! `status` reports dormant). Requires `git` on PATH (present in CI).

use std::fs;
use std::path::Path;
use std::process::Command as Proc;

use assert_cmd::Command;
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
