//! End-to-end proof of the `copy` vertical, entirely inside temp dirs — HOME,
//! TEMPER_DIR, and the journal state dir are all TempDirs, so this never touches
//! the real machine.

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

fn fleet(home: &Path, fake_home: &Path, state: &Path) -> Command {
    let mut c = Command::cargo_bin("temper").unwrap();
    c.env("TEMPER_DIR", home)
        .env("HOME", fake_home)
        .env("TEMPER_STATE_DIR", state);
    c
}

#[test]
fn deploy_drift_redeploy_undo() {
    let home = TempDir::new().unwrap(); // the temper-home (config folder)
    let fake_home = TempDir::new().unwrap(); // stand-in $HOME (deploy target root)
    let state = TempDir::new().unwrap(); // journal state
    let h = home.path();

    fs::create_dir_all(h.join("apps")).unwrap();
    fs::create_dir_all(h.join("assets")).unwrap();
    fs::write(h.join("assets/starship.toml"), "content-X\n").unwrap();
    fs::write(
        h.join("temper.toml"),
        format!("[[machine]]\nname = \"test\"\nos = \"{}\"\napps = [\"demo\"]\n", os()),
    )
    .unwrap();
    fs::write(
        h.join("apps/demo.toml"),
        "[[step]]\ncopy = \"assets/starship.toml\"\nto = \"~/.config/starship.toml\"\n",
    )
    .unwrap();

    let target = fake_home.path().join(".config/starship.toml");

    // install → deploys the file
    fleet(h, fake_home.path(), state.path())
        .arg("install")
        .assert()
        .success();
    assert_eq!(fs::read_to_string(&target).unwrap(), "content-X\n");

    // drift → clean
    fleet(h, fake_home.path(), state.path())
        .arg("drift")
        .assert()
        .success()
        .stdout(predicates::str::contains("in sync"));

    // tamper → drift detects it
    fs::write(&target, "tampered\n").unwrap();
    fleet(h, fake_home.path(), state.path())
        .arg("drift")
        .assert()
        .success()
        .stdout(predicates::str::contains("drifted"));

    // install again → redeploys X (journaling the tampered bytes as the inverse)
    fleet(h, fake_home.path(), state.path())
        .arg("install")
        .assert()
        .success();
    assert_eq!(fs::read_to_string(&target).unwrap(), "content-X\n");

    // undo → restores the tampered bytes (the state before the last install)
    fleet(h, fake_home.path(), state.path())
        .arg("undo")
        .assert()
        .success();
    assert_eq!(fs::read_to_string(&target).unwrap(), "tampered\n");
}
