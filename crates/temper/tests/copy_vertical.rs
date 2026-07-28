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

fn temper(home: &Path, fake_home: &Path, state: &Path) -> Command {
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
    temper(h, fake_home.path(), state.path())
        .arg("install")
        .assert()
        .success();
    assert_eq!(fs::read_to_string(&target).unwrap(), "content-X\n");

    // drift → clean
    temper(h, fake_home.path(), state.path())
        .arg("drift")
        .assert()
        .success()
        .stdout(predicates::str::contains("in sync"));

    // tamper → drift detects it
    fs::write(&target, "tampered\n").unwrap();
    temper(h, fake_home.path(), state.path())
        .arg("drift")
        .assert()
        .success()
        .stdout(predicates::str::contains("drifted"));

    // install again → redeploys X (journaling the tampered bytes as the inverse)
    temper(h, fake_home.path(), state.path())
        .arg("install")
        .assert()
        .success();
    assert_eq!(fs::read_to_string(&target).unwrap(), "content-X\n");

    // undo → restores the tampered bytes (the state before the last install)
    temper(h, fake_home.path(), state.path())
        .arg("undo")
        .assert()
        .success();
    assert_eq!(fs::read_to_string(&target).unwrap(), "tampered\n");
}

/// The after-hash guard (`journal.rs`): `undo` must NOT revert a file the user
/// changed since temper wrote it — it skips that entry and leaves the edit
/// intact. Without the guard, undo would delete the freshly-created file and
/// silently destroy the edit. (The test above only proves the guard *allows* a
/// revert when the hash matches; this proves it *blocks* one when it doesn't.)
#[test]
fn undo_preserves_a_post_install_hand_edit() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
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

    // install → creates the file (a Create journal entry, hash = content-X).
    temper(h, fake_home.path(), state.path()).arg("install").assert().success();
    assert_eq!(fs::read_to_string(&target).unwrap(), "content-X\n");

    // The user hand-edits the deployed file AFTER the install.
    fs::write(&target, "my-hand-edit\n").unwrap();

    // undo → the file no longer hashes to what temper wrote, so the entry is
    // skipped and the edit survives (never deleted/clobbered).
    temper(h, fake_home.path(), state.path()).arg("undo").assert().success();
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "my-hand-edit\n",
        "undo must preserve a post-install hand edit, not revert it"
    );
}
