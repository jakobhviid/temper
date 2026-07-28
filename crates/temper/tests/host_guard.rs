//! Proves the install host-mismatch guard: an explicit machine name that isn't
//! this host confirms (or, under --json, refuses without --yes), while no name
//! and --yes both proceed. Uses a machine name that can't be the test runner's
//! hostname, so the mismatch is deterministic.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use tempfile::TempDir;

fn os() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    }
}

const NAME: &str = "definitely-not-this-host-xyz";

fn setup() -> (TempDir, TempDir, TempDir) {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    fs::write(
        home.path().join("temper.toml"),
        format!("[[machine]]\nname = \"{NAME}\"\nos = \"{}\"\n", os()),
    )
    .unwrap();
    (home, fake_home, state)
}

fn temper(home: &Path, fake_home: &Path, state: &Path) -> Command {
    let mut c = Command::cargo_bin("temper").unwrap();
    c.env("TEMPER_DIR", home)
        .env("HOME", fake_home)
        .env("TEMPER_STATE_DIR", state);
    c
}

#[test]
fn no_name_proceeds_without_confirm() {
    let (h, fh, s) = setup();
    // No machine argument → resolves this host (single-machine fallback here);
    // the guard never fires.
    temper(h.path(), fh.path(), s.path())
        .args(["--json", "install"])
        .assert()
        .success();
}

#[test]
fn explicit_mismatch_refuses_under_json_without_yes() {
    let (h, fh, s) = setup();
    temper(h.path(), fh.path(), s.path())
        .args(["--json", "install", NAME])
        .assert()
        .failure()
        .stderr(predicates::str::contains("pass --yes to confirm"));
}

#[test]
fn explicit_mismatch_proceeds_with_yes() {
    let (h, fh, s) = setup();
    temper(h.path(), fh.path(), s.path())
        .args(["--json", "install", NAME, "--yes"])
        .assert()
        .success();
}

#[test]
fn dry_run_never_gates() {
    let (h, fh, s) = setup();
    // A cross-host name is fine to *preview* from anywhere.
    temper(h.path(), fh.path(), s.path())
        .args(["install", NAME, "--dry-run"])
        .assert()
        .success();
}
