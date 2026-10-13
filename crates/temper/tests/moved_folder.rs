//! A saved temper folder that moved: every answer names the old path first, a
//! lone match is re-pinned, and nothing sends the reader to `init` for a second,
//! empty folder. Each runs in its own process with a controlled HOME/XDG.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn os() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    }
}

fn manifest(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("temper.toml"),
        format!("[[machine]]\nname = \"m\"\nos = \"{}\"\n", os()),
    )
    .unwrap();
}

/// A HOME, an XDG base whose pointer names a folder that is not there, and a
/// cwd outside both.
struct Env {
    home: TempDir,
    xdg: TempDir,
    cwd: TempDir,
    old: PathBuf,
}

impl Env {
    fn new() -> Self {
        let home = TempDir::new().unwrap();
        let xdg = TempDir::new().unwrap();
        let old = home.path().join("Developer").join("steel");
        fs::create_dir_all(xdg.path().join("temper")).unwrap();
        fs::write(xdg.path().join("temper").join("home"), old.display().to_string()).unwrap();
        Env { home, xdg, cwd: TempDir::new().unwrap(), old }
    }

    fn temper(&self) -> Command {
        let mut c = Command::cargo_bin("temper").unwrap();
        c.env_remove("TEMPER_DIR")
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.xdg.path())
            .current_dir(self.cwd.path());
        c
    }

    fn pointer(&self) -> String {
        fs::read_to_string(self.xdg.path().join("temper").join("home")).unwrap()
    }
}

#[test]
fn a_lone_match_is_used_and_re_pinned() {
    let e = Env::new();
    let new = e.home.path().join("Developer").join("Private").join("steel");
    manifest(&new);
    e.temper()
        .arg("drift")
        .assert()
        .success()
        .stderr(predicate::str::contains(format!("{} is gone", e.old.display())))
        .stderr(predicate::str::contains("saved it as the temper home"));
    assert_eq!(Path::new(e.pointer().trim()), new.canonicalize().unwrap());

    // Pinned, so the next run says nothing about it.
    e.temper().arg("drift").assert().success().stderr(predicate::str::contains("is gone").not());
}

#[test]
fn nothing_found_names_the_old_folder_not_init() {
    let e = Env::new();
    e.temper()
        .arg("drift")
        .assert()
        .failure()
        .stderr(predicate::str::contains(format!("{} is gone — moved?", e.old.display())))
        .stderr(predicate::str::contains("temper setup <dir>"))
        .stderr(predicate::str::contains("temper init").not());
}

#[test]
fn a_folder_left_without_its_manifest_says_so() {
    let e = Env::new();
    fs::create_dir_all(&e.old).unwrap();
    e.temper()
        .arg("drift")
        .assert()
        .failure()
        .stderr(predicate::str::contains("has no temper.toml — moved?"));
}

#[test]
fn several_found_still_refuse_but_name_the_old_folder() {
    let e = Env::new();
    manifest(&e.home.path().join("Developer").join("Private").join("steel"));
    manifest(&e.home.path().join("Developer").join("Work").join("steel"));
    e.temper()
        .arg("drift")
        .assert()
        .failure()
        .stderr(predicate::str::contains("is gone — moved?"))
        .stderr(predicate::str::contains("several temper-homes"));
}

#[test]
fn setup_names_the_old_folder() {
    let e = Env::new();
    e.temper()
        .arg("setup")
        .assert()
        .failure()
        .stderr(predicate::str::contains("is gone — moved?"))
        .stderr(predicate::str::contains("temper init").not());
    e.temper()
        .args(["--json", "setup"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("\"stale\":\"{}\"", e.old.display())));
}

#[test]
fn init_points_at_setup_before_making_a_second_folder() {
    let e = Env::new();
    e.temper()
        .arg("init")
        .write_stdin("n\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("is gone — if it moved, `temper setup <dir>`"))
        .stdout(predicate::str::contains("nothing changed"));
}

#[test]
fn a_temper_dir_that_is_gone_says_moved() {
    let e = Env::new();
    e.temper()
        .env("TEMPER_DIR", &e.old)
        .arg("drift")
        .assert()
        .failure()
        .stderr(predicate::str::contains("does not exist — moved?"));
}
