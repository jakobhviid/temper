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
        format!(
            "[[machine]]\nname = \"t\"\nos = \"{}\"\napps = [\"demo\"]\n",
            os()
        ),
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
    assert!(
        fake_home.path().join(".config/x.conf").exists(),
        "present probe should apply"
    );
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
    assert!(
        !fake_home.path().join(".config/x.conf").exists(),
        "absent probe should skip the step"
    );
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

/// A probe's `exec` is a **shell command**, not a script path — the reading
/// `SPEC.md` documents and every example writes.
///
/// The two `exec` spellings in the schema mean different things: a `[[step]]`'s
/// names a file under the temper-home, a probe's is the command line itself. Run
/// as a path, `exec = "true"` resolves to `<temper-home>/true`, which no folder
/// contains — so `sh` exits non-zero and the gate reports the machine as lacking
/// something it has. That failure is invisible, because a step skipped by a
/// broken probe is indistinguishable from one skipped by a legitimate gate-miss.
#[test]
fn exec_probe_runs_a_command_not_a_path() {
    let (home, fake_home, state) = setup(
        "[[step]]\ncopy = \"assets/x.conf\"\nto = \"~/.config/x.conf\"\nwhen = { exec = \"true\" }\n",
    );
    temper(home.path(), fake_home.path(), state.path())
        .arg("install")
        .assert()
        .success();
    assert!(
        fake_home.path().join(".config/x.conf").exists(),
        "a passing exec probe should apply the step"
    );
}

/// The other half: a command that exits non-zero still gates the step out, so
/// the fix does not turn every `exec` probe into a pass.
#[test]
fn failing_exec_probe_still_skips() {
    let (home, fake_home, state) = setup(
        "[[step]]\ncopy = \"assets/x.conf\"\nto = \"~/.config/x.conf\"\nwhen = { exec = \"test -d /no/such/dir/xyz\" }\n",
    );
    temper(home.path(), fake_home.path(), state.path())
        .arg("install")
        .assert()
        .success()
        .stdout(predicates::str::contains("skipped"));
    assert!(!fake_home.path().join(".config/x.conf").exists());
}
