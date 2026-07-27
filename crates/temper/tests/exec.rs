//! Proves the `exec` primitive: check-gated run, drift-hook, secret
//! passthrough, and loud failure on a missing secret — all in temp dirs.

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
fn exec_check_secret_and_failure() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let h = home.path();

    fs::create_dir_all(h.join("apps")).unwrap();
    fs::create_dir_all(h.join("scripts")).unwrap();
    // setup writes the secret into a marker file under $HOME
    fs::write(h.join("scripts/setup.sh"), "echo \"$MY_SECRET\" > \"$HOME/.exec-ran\"\n").unwrap();
    // check passes once the marker file exists
    fs::write(h.join("scripts/check.sh"), "test -f \"$HOME/.exec-ran\"\n").unwrap();
    fs::write(
        h.join("temper.toml"),
        format!("[[machine]]\nname = \"test\"\nos = \"{}\"\napps = [\"demo\"]\n", os()),
    )
    .unwrap();
    fs::write(
        h.join("apps/demo.toml"),
        "[[step]]\nexec = \"scripts/setup.sh\"\ncheck = \"scripts/check.sh\"\nsecrets = [\"MY_SECRET\"]\n",
    )
    .unwrap();

    let marker = fake_home.path().join(".exec-ran");

    // install → check fails (no marker), script runs with the secret
    fleet(h, fake_home.path(), state.path())
        .env("MY_SECRET", "hunter2")
        .arg("install")
        .assert()
        .success();
    assert_eq!(fs::read_to_string(&marker).unwrap(), "hunter2\n");

    // drift → check passes, reported in sync
    fleet(h, fake_home.path(), state.path())
        .env("MY_SECRET", "hunter2")
        .arg("drift")
        .assert()
        .success()
        .stdout(predicates::str::contains("in sync"))
        .stdout(predicates::str::contains("0 out of sync"));

    // re-install with a DIFFERENT secret → check already passes, so the script
    // is skipped (idempotent); the marker keeps its original content.
    fleet(h, fake_home.path(), state.path())
        .env("MY_SECRET", "changed")
        .arg("install")
        .assert()
        .success();
    assert_eq!(fs::read_to_string(&marker).unwrap(), "hunter2\n", "exec re-ran despite passing check");

    // remove the marker (check now fails) and drop the secret → install must
    // fail loudly rather than run without the required secret.
    fs::remove_file(&marker).unwrap();
    fleet(h, fake_home.path(), state.path())
        .env_remove("MY_SECRET")
        .arg("install")
        .assert()
        .failure()
        .stderr(predicates::str::contains("MY_SECRET"));
}
