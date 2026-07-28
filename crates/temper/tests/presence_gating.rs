//! Proves `when` (soft, skip-loudly) and `needs` (hard, error) presence gates.
//! Deterministic: `sh` is always present, a bogus binary never is.

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

fn temper(home: &Path, fake_home: &Path, state: &Path) -> Command {
    let mut c = Command::cargo_bin("temper").unwrap();
    c.env("TEMPER_DIR", home)
        .env("HOME", fake_home)
        .env("TEMPER_STATE_DIR", state);
    c
}

fn setup(bundle: &str) -> (TempDir, TempDir, TempDir) {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    fs::create_dir_all(home.path().join("apps")).unwrap();
    fs::create_dir_all(home.path().join("assets")).unwrap();
    fs::write(home.path().join("assets/x.conf"), "managed\n").unwrap();
    fs::write(
        home.path().join("temper.toml"),
        format!("[[machine]]\nname = \"t\"\nos = \"{}\"\napps = [\"demo\"]\n", os()),
    )
    .unwrap();
    fs::write(home.path().join("apps/demo.toml"), bundle).unwrap();
    (home, fake_home, state)
}

#[test]
fn when_present_applies() {
    let (home, fake_home, state) = setup(
        "[[step]]\ncopy = \"assets/x.conf\"\nto = \"~/.config/x.conf\"\nwhen = { binary = \"sh\" }\n",
    );
    temper(home.path(), fake_home.path(), state.path())
        .arg("install")
        .assert()
        .success();
    assert!(fake_home.path().join(".config/x.conf").exists(), "present probe should apply");
}

#[test]
fn when_absent_skips_loudly() {
    let (home, fake_home, state) = setup(
        "[[step]]\ncopy = \"assets/x.conf\"\nto = \"~/.config/x.conf\"\nwhen = { binary = \"no-such-bin-xyz\" }\n",
    );
    temper(home.path(), fake_home.path(), state.path())
        .arg("install")
        .assert()
        .success()
        .stdout(predicates::str::contains("skipped"));
    assert!(!fake_home.path().join(".config/x.conf").exists(), "absent probe should skip the step");
}

#[test]
fn needs_absent_errors() {
    let (home, fake_home, state) = setup(
        "[[step]]\ncopy = \"assets/x.conf\"\nto = \"~/.config/x.conf\"\nneeds = { binary = \"no-such-bin-xyz\" }\n",
    );
    temper(home.path(), fake_home.path(), state.path())
        .arg("install")
        .assert()
        .failure()
        .stderr(predicates::str::contains("needs"));
    assert!(!fake_home.path().join(".config/x.conf").exists());
}

#[test]
fn when_absent_is_status_only_in_drift() {
    let (home, fake_home, state) = setup(
        "[[step]]\ncopy = \"assets/x.conf\"\nto = \"~/.config/x.conf\"\nwhen = { binary = \"no-such-bin-xyz\" }\n",
    );
    // The gated-out step is NOT counted as drift (it's status-only).
    temper(home.path(), fake_home.path(), state.path())
        .arg("drift")
        .assert()
        .success()
        .stdout(predicates::str::contains("0 out of sync"));
}
