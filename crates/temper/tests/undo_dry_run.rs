//! `undo --dry-run` must touch nothing — including packages.
//!
//! The file and dconf reverts each guard on `dry_run`; the package revert is the
//! one that shells out to a real `uninstall`, and it was the one arm handled
//! before the guard. A preview that uninstalls is worse than no preview: it is
//! the command you run *because* you are not sure yet.
//!
//! The tool is faked on PATH rather than mocked, because the claim under test is
//! "no child process ran" — which only an observation of the child can settle.

use std::fs;

use assert_cmd::Command;
use tempfile::TempDir;

/// A `gext` on PATH that records having been called, so the assertion is about
/// the child process rather than about temper's own reporting.
fn fake_gext(dir: &std::path::Path, marker: &std::path::Path) {
    let bin = dir.join("gext");
    fs::write(
        &bin,
        format!("#!/bin/sh\necho \"$@\" >> {}\n", marker.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A run whose only entry is a package install, written straight to the state
/// dir — the shape `record_packages` + `commit` produce.
fn a_run_that_installed_packages(state: &std::path::Path) {
    let dir = state.join("runs").join("1000000000-000000000");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.json"),
        r#"{"argv":["temper","install"],"entries":[
             {"op":"PackagesInstalled","provider":"gnome-extensions",
              "packages":["a@x","b@y"]}]}"#,
    )
    .unwrap();
}

#[test]
fn a_dry_run_undo_does_not_uninstall_anything() {
    let state = TempDir::new().unwrap();
    let bindir = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let marker = fake_home.path().join("gext-was-called");
    fake_gext(bindir.path(), &marker);
    a_run_that_installed_packages(state.path());

    let path = format!(
        "{}:{}",
        bindir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let out = Command::cargo_bin("temper")
        .unwrap()
        .args(["undo", "--dry-run"])
        .env("TEMPER_STATE_DIR", state.path())
        .env("HOME", fake_home.path())
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);

    assert!(
        !marker.exists(),
        "a dry run shelled out to the uninstaller: {:?}",
        fs::read_to_string(&marker)
    );
    // It still has to *report* the two it would remove — a silent preview is
    // the other way to get this wrong.
    let said = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("2 package(s)"),
        "the preview must name what it would un-install, got: {said}"
    );

    // And the run is still there to undo for real afterwards.
    assert!(state
        .path()
        .join("runs/1000000000-000000000/manifest.json")
        .is_file());
}

/// A run whose only changes were unrevertible still records a run — so a later
/// `undo` cannot silently revert an older, unrelated one.
///
/// The journal was written only when it had a revertible entry, so a converge
/// consisting of an `exec` wrote no run directory at all. `temper undo` then
/// picked the newest run it *could* see, reverted that, and reported success:
/// the user asked to undo what just happened and got something else undone.
#[test]
fn an_unrevertible_run_is_still_a_run() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let h = home.path();
    let os = if cfg!(target_os = "macos") { "mac" } else { "linux" };

    fs::create_dir_all(h.join("apps")).unwrap();
    fs::create_dir_all(h.join("assets")).unwrap();
    fs::write(h.join("temper.toml"),
        format!("[[machine]]\nname = \"t\"\nos = \"{os}\"\napps = [\"a\"]\n")).unwrap();

    let temper = || {
        let mut c = Command::cargo_bin("temper").unwrap();
        c.env("TEMPER_DIR", h)
            .env("HOME", fake_home.path())
            .env("TEMPER_STATE_DIR", state.path());
        c
    };

    // Run one: a revertible change. This is the run that must NOT be reverted.
    fs::write(h.join("assets/x.conf"), "first\n").unwrap();
    fs::write(h.join("apps/a.toml"),
        "[[step]]\ncopy = \"assets/x.conf\"\nto = \"~/x.conf\"\n").unwrap();
    temper().arg("install").assert().success();
    let deployed = fake_home.path().join("x.conf");
    assert!(deployed.is_file());

    // Run two: only an `exec`, which is not revertible.
    fs::write(h.join("assets/run.sh"), "touch \"$HOME/ran\"\n").unwrap();
    fs::write(h.join("apps/a.toml"),
        "[[step]]\ncopy = \"assets/x.conf\"\nto = \"~/x.conf\"\n\n\
         [[step]]\nexec = \"assets/run.sh\"\nrun = \"always\"\n").unwrap();
    temper().arg("install").assert().success();

    // Undoing "the last run" must land on run two — which reverts nothing and
    // says so — leaving run one's file alone.
    let out = temper().arg("undo").output().unwrap();
    let said = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
    assert!(
        deployed.exists(),
        "undo reverted an older run: the file from run one is gone.\n{said}"
    );
    assert!(
        said.contains("cannot be reverted") || said.contains("skipped 1"),
        "and it should say why nothing was reverted:\n{said}"
    );
}
