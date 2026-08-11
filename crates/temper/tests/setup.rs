//! Proves `setup` (rename of `use`): explicit path, the `use` alias, `--json`
//! candidate listing, the not-a-terminal refusal, and the "several libraries →
//! refuse" ambiguity guard. Each runs in its own process with a controlled
//! HOME/XDG so there's no env race.

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

fn temper() -> Command {
    let mut c = Command::cargo_bin("temper").unwrap();
    c.env_remove("TEMPER_DIR"); // don't inherit the harness's, if any
    c
}

fn manifest(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("temper.toml"),
        format!("[[machine]]\nname = \"m\"\nos = \"{}\"\n", os()),
    )
    .unwrap();
}

#[test]
fn setup_explicit_then_resolves_via_pointer() {
    let steel = TempDir::new().unwrap();
    let xdg = TempDir::new().unwrap();
    let elsewhere = TempDir::new().unwrap(); // empty cwd/home, no temper.toml
    manifest(steel.path());

    temper()
        .env("XDG_CONFIG_HOME", xdg.path())
        .current_dir(elsewhere.path())
        .args(["setup", steel.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("temper home set to"));

    // With the pointer saved, a later command resolves it from an unrelated cwd.
    temper()
        .env("XDG_CONFIG_HOME", xdg.path())
        .env("HOME", elsewhere.path())
        .current_dir(elsewhere.path())
        .arg("drift")
        .assert()
        .success();
}

#[test]
fn use_alias_still_works() {
    let steel = TempDir::new().unwrap();
    let xdg = TempDir::new().unwrap();
    manifest(steel.path());
    temper()
        .env("XDG_CONFIG_HOME", xdg.path())
        .args(["use", steel.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("temper home set to"));
}

/// With nothing discovered there is nothing to pick, and a terminal would not
/// have helped — so leading with "not a terminal" sent the reader looking for
/// the wrong problem. The larger group here has just installed temper and has no
/// folder at all, and `setup` cannot make one: it *picks* an existing folder.
/// Naming `init` is the whole point of the message.
#[test]
fn setup_with_nothing_to_pick_names_the_verb_that_creates_one() {
    let xdg = TempDir::new().unwrap();
    let empty = TempDir::new().unwrap();
    temper()
        .env("XDG_CONFIG_HOME", xdg.path())
        .env("HOME", empty.path())
        .current_dir(empty.path())
        .arg("setup")
        .assert()
        .failure()
        .stderr(predicates::str::contains("nothing to pick"))
        .stderr(predicates::str::contains("temper init"));
}

/// …and when there IS something to pick, the tty is the real obstacle. This
/// branch had no test, which is why the two could be conflated.
#[test]
fn setup_no_arg_non_terminal_refuses() {
    let home = TempDir::new().unwrap();
    let xdg = TempDir::new().unwrap();
    manifest(&home.path().join("steel")); // discoverable → the picker would open
    // piped stdin (assert_cmd) is not a tty → can't prompt.
    temper()
        .env("XDG_CONFIG_HOME", xdg.path())
        .env("HOME", home.path())
        .current_dir(home.path())
        .arg("setup")
        .assert()
        .failure()
        .stderr(predicates::str::contains("not a terminal"))
        .stderr(predicates::str::contains("steel"));
}

#[test]
fn setup_json_lists_candidates() {
    let home = TempDir::new().unwrap();
    let xdg = TempDir::new().unwrap();
    manifest(&home.path().join("steel")); // a scanned name under HOME
    temper()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", xdg.path())
        .args(["--json", "setup"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"candidates\""))
        .stdout(predicates::str::contains("steel"));
}

#[test]
fn several_libraries_refuse_to_guess() {
    let home = TempDir::new().unwrap();
    let xdg = TempDir::new().unwrap(); // empty → no saved pointer
    let cwd = TempDir::new().unwrap(); // outside HOME, no temper.toml above
    manifest(&home.path().join("steel"));
    manifest(&home.path().join("Developer").join("steel"));

    temper()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", xdg.path())
        .current_dir(cwd.path())
        .arg("drift")
        .assert()
        .failure()
        .stderr(predicates::str::contains("several temper-homes"));
}
