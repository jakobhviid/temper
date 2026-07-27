//! Regression tests for confirmed bugs found in the adversarial review.
//! All in temp dirs (HOME/TEMPER_DIR/TEMPER_STATE_DIR sandboxed).

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use tempfile::TempDir;

fn os() -> &'static str {
    if cfg!(target_os = "macos") { "mac" } else { "linux" }
}

fn temper(home: &Path, fake_home: &Path, state: &Path) -> Command {
    let mut c = Command::cargo_bin("temper").unwrap();
    c.env("TEMPER_DIR", home).env("HOME", fake_home).env("TEMPER_STATE_DIR", state);
    c
}

fn machine(h: &Path, app_body: &str) {
    fs::create_dir_all(h.join("apps")).unwrap();
    fs::write(h.join("temper.toml"), format!("[[machine]]\nname=\"t\"\nos=\"{}\"\napps=[\"a\"]\n", os())).unwrap();
    fs::write(h.join("apps/a.toml"), app_body).unwrap();
}

// undo --list must NOT revert (help says it only lists); then undo does revert.
#[test]
fn undo_list_is_read_only() {
    let (home, fh, st) = (TempDir::new().unwrap(), TempDir::new().unwrap(), TempDir::new().unwrap());
    fs::write(home.path().join("x"), "v\n").unwrap();
    machine(home.path(), "[[step]]\ncopy=\"x\"\nto=\"~/.config/x\"\n");
    let target = fh.path().join(".config/x");

    temper(home.path(), fh.path(), st.path()).arg("install").assert().success();
    assert!(target.exists());

    // --list must leave the file in place
    temper(home.path(), fh.path(), st.path()).args(["undo", "--list"]).assert().success();
    assert!(target.exists(), "undo --list wrongly reverted the run");

    // undo reverts (Create → remove)
    temper(home.path(), fh.path(), st.path()).arg("undo").assert().success();
    assert!(!target.exists(), "undo did not revert");
}

// undo <run-id> reverts the NAMED run, not the newest.
#[test]
fn undo_targets_named_run() {
    let (home, fh, st) = (TempDir::new().unwrap(), TempDir::new().unwrap(), TempDir::new().unwrap());
    fs::write(home.path().join("x"), "v\n").unwrap();
    // run 1 creates a.conf
    machine(home.path(), "[[step]]\ncopy=\"x\"\nto=\"~/.config/a.conf\"\n");
    temper(home.path(), fh.path(), st.path()).arg("install").assert().success();
    // run 2 creates b.conf (a.conf already in sync)
    fs::write(home.path().join("apps/a.toml"),
        "[[step]]\ncopy=\"x\"\nto=\"~/.config/a.conf\"\n\n[[step]]\ncopy=\"x\"\nto=\"~/.config/b.conf\"\n").unwrap();
    temper(home.path(), fh.path(), st.path()).arg("install").assert().success();

    let a = fh.path().join(".config/a.conf");
    let b = fh.path().join(".config/b.conf");
    assert!(a.exists() && b.exists());

    // list is newest-first; the oldest id = run 1 (created a.conf)
    let out = temper(home.path(), fh.path(), st.path()).args(["undo", "--list", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let runs = v["runs"].as_array().unwrap();
    let run1 = runs.last().unwrap().as_str().unwrap().to_string();

    temper(home.path(), fh.path(), st.path()).args(["undo", &run1]).assert().success();
    assert!(!a.exists(), "named run (a.conf) not reverted");
    assert!(b.exists(), "wrong run reverted — b.conf gone");
}

// block refuses a malformed (orphan begin) marker region instead of corrupting.
#[test]
fn block_refuses_malformed_region() {
    let (home, fh, st) = (TempDir::new().unwrap(), TempDir::new().unwrap(), TempDir::new().unwrap());
    fs::write(home.path().join("body"), "INCLUDE me\n").unwrap();
    machine(home.path(), "[[step]]\nblock=\"body\"\nin=\"~/.ssh/config\"\nmarker=\"m\"\n");
    let target = fh.path().join(".ssh/config");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    // orphan begin marker + user content, no matching end
    let orig = "# >>> temper:m >>>\nHost keep\n  User me\n";
    fs::write(&target, orig).unwrap();

    temper(home.path(), fh.path(), st.path()).arg("install").assert().failure()
        .stderr(predicates::str::contains("malformed marker region"));
    assert_eq!(fs::read_to_string(&target).unwrap(), orig, "user content was altered");
}

// ensure = install-if-missing on update: create when absent, never overwrite.
#[test]
fn ensure_is_install_if_missing_on_update() {
    let (home, fh, st) = (TempDir::new().unwrap(), TempDir::new().unwrap(), TempDir::new().unwrap());
    fs::write(home.path().join("x"), "managed\n").unwrap();
    machine(home.path(), "[[step]]\ncopy=\"x\"\nto=\"~/.config/e.conf\"\nrun=\"ensure\"\n");
    let e = fh.path().join(".config/e.conf");

    temper(home.path(), fh.path(), st.path()).arg("install").assert().success();
    assert_eq!(fs::read_to_string(&e).unwrap(), "managed\n");

    // user edits it → update (ensure) must NOT overwrite
    fs::write(&e, "user-edited\n").unwrap();
    temper(home.path(), fh.path(), st.path()).arg("update").assert().success();
    assert_eq!(fs::read_to_string(&e).unwrap(), "user-edited\n", "ensure overwrote a present file");

    // deleted → update (ensure) recreates it
    fs::remove_file(&e).unwrap();
    temper(home.path(), fh.path(), st.path()).arg("update").assert().success();
    assert_eq!(fs::read_to_string(&e).unwrap(), "managed\n", "ensure did not recreate a missing file");
}

// setkey json refuses to descend into (and clobber) an existing scalar.
#[test]
fn setkey_json_refuses_scalar_intermediate() {
    let (home, fh, st) = (TempDir::new().unwrap(), TempDir::new().unwrap(), TempDir::new().unwrap());
    machine(home.path(),
        "[[step]]\nsetkey = { backend = \"json\", file = \"~/d.json\", key = \"a.x\", value = \"1\" }\n");
    let target = fh.path().join("d.json");
    fs::write(&target, "{\"a\": 5, \"b\": 7}\n").unwrap();
    temper(home.path(), fh.path(), st.path()).arg("install").assert().failure()
        .stderr(predicates::str::contains("not an object"));
    assert!(fs::read_to_string(&target).unwrap().contains("\"a\": 5"), "scalar was clobbered");
}

// duplicate machine names are rejected at load.
#[test]
fn duplicate_machine_names_error() {
    let (home, fh, st) = (TempDir::new().unwrap(), TempDir::new().unwrap(), TempDir::new().unwrap());
    fs::create_dir_all(home.path().join("apps")).unwrap();
    fs::write(home.path().join("temper.toml"),
        format!("[[machine]]\nname=\"t\"\nos=\"{}\"\n[[machine]]\nname=\"t\"\nos=\"{}\"\n", os(), os())).unwrap();
    temper(home.path(), fh.path(), st.path()).args(["drift", "t"]).assert().failure()
        .stderr(predicates::str::contains("duplicate machine name"));
}

// setkey json refuses to overwrite a file whose root isn't an object.
#[test]
fn setkey_json_refuses_non_object_root() {
    let (home, fh, st) = (TempDir::new().unwrap(), TempDir::new().unwrap(), TempDir::new().unwrap());
    machine(home.path(),
        "[[step]]\nsetkey = { backend = \"json\", file = \"~/data.json\", key = \"foo\", value = \"bar\" }\n");
    let target = fh.path().join("data.json");
    fs::write(&target, "[1, 2, 3, \"keep-me\"]\n").unwrap();

    temper(home.path(), fh.path(), st.path()).arg("install").assert().failure()
        .stderr(predicates::str::contains("root is not an object"));
    assert_eq!(fs::read_to_string(&target).unwrap(), "[1, 2, 3, \"keep-me\"]\n", "file was clobbered");
}
