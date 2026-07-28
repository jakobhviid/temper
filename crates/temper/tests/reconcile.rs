//! Deterministic reconcile paths (the interactive add/drop mutation logic is
//! unit-tested in temper-core::reconcile; the live diff is verified by hand
//! since it depends on real installed state).

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
fn reconcile_without_brewfile_errors_helpfully() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    fs::write(
        home.path().join("temper.toml"),
        format!("[[machine]]\nname = \"t\"\nos = \"{}\"\n", os()),
    )
    .unwrap();

    temper(home.path(), fake_home.path(), state.path())
        .arg("reconcile")
        .assert()
        .failure()
        .stderr(predicates::str::contains("no `brewfile`"));
}

#[test]
fn reconcile_empty_set_is_in_sync() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    // Empty brewfile + no apps/loose → empty effective set → nothing to do,
    // deterministically (an empty set never probes a package manager).
    fs::create_dir_all(home.path().join("brewfiles")).unwrap();
    fs::write(home.path().join("brewfiles/t"), "").unwrap();
    fs::write(
        home.path().join("temper.toml"),
        format!(
            "[[machine]]\nname = \"t\"\nos = \"{}\"\nbrewfile = \"brewfiles/t\"\n",
            os()
        ),
    )
    .unwrap();

    temper(home.path(), fake_home.path(), state.path())
        .arg("reconcile")
        .assert()
        .success()
        .stdout(predicates::str::contains("already in sync"));

    // --json previews the (empty) plan without prompting.
    temper(home.path(), fake_home.path(), state.path())
        .args(["--json", "reconcile"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"adds\":[]"))
        .stdout(predicates::str::contains("\"drops\":[]"));
}
