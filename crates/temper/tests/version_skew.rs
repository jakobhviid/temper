//! Proves the "written by a newer temper" path: a parse failure caused by a
//! newer stamp is diagnosed as a version skew (with the upgrade path), a skew
//! with `[update].mode = "off"` falls back to the plain parser error, and a
//! folder that PARSES but was stamped newer still gets nudged while the command
//! carries on. The test binary lives under `target/`, not `/Cellar/`, so brew
//! self-update never fires — these paths are deterministic and never prompt.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
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
        .env("TEMPER_STATE_DIR", state)
        .env_remove("TEMPER_SELF_UPDATED");
    c
}

#[test]
fn unknown_field_with_newer_stamp_is_a_version_skew() {
    let home = TempDir::new().unwrap();
    let fake = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    // A field this build can't parse, plus a stamp from a much newer temper.
    fs::write(
        home.path().join("temper.toml"),
        "temper_version = \"999.0.0\"\nfuture_field = true\n",
    )
    .unwrap();

    temper(home.path(), fake.path(), state.path())
        .arg("update")
        .assert()
        .failure()
        .stderr(predicates::str::contains("written by temper 999.0.0"))
        .stderr(predicates::str::contains("you're running"))
        .stderr(predicates::str::contains("Upgrade temper"));
}

#[test]
fn mode_off_falls_back_to_the_plain_parser_error() {
    let home = TempDir::new().unwrap();
    let fake = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    fs::write(
        home.path().join("temper.toml"),
        "temper_version = \"999.0.0\"\nfuture_field = true\n[update]\nmode = \"off\"\n",
    )
    .unwrap();

    temper(home.path(), fake.path(), state.path())
        .arg("update")
        .assert()
        .failure()
        // the raw parser error names the offending field...
        .stderr(predicates::str::contains("future_field"))
        // ...and does NOT dress it up as a version skew.
        .stderr(predicates::str::contains("written by temper").not());
}

#[test]
fn genuine_typo_without_a_newer_stamp_is_a_plain_error() {
    let home = TempDir::new().unwrap();
    let fake = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    fs::write(home.path().join("temper.toml"), "future_field = true\n").unwrap();

    temper(home.path(), fake.path(), state.path())
        .arg("update")
        .assert()
        .failure()
        .stderr(predicates::str::contains("future_field"))
        .stderr(predicates::str::contains("written by temper").not());
}

#[test]
fn parse_ok_but_newer_stamp_nudges_then_continues() {
    let home = TempDir::new().unwrap();
    let fake = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    // Valid manifest, but stamped by a newer temper: the command must still run.
    fs::write(
        home.path().join("temper.toml"),
        format!(
            "temper_version = \"999.0.0\"\n[[machine]]\nname = \"test\"\nos = \"{}\"\n",
            os()
        ),
    )
    .unwrap();

    temper(home.path(), fake.path(), state.path())
        .arg("update")
        .assert()
        .success()
        .stderr(predicates::str::contains("written by temper 999.0.0"));
}

#[test]
fn configure_sets_gets_and_rejects() {
    let home = TempDir::new().unwrap();
    let fake = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let tt = home.path().join("temper.toml");
    fs::write(&tt, "# fleet\n[vars]\nEDITOR = \"hx\"\n").unwrap();

    // Unknown key is rejected by clap, listing the valid keys (discoverability).
    temper(home.path(), fake.path(), state.path())
        .args(["configure", "set", "bogus.key", "on"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("possible values").and(predicates::str::contains("update.mode")));

    // Bad value for a known key names the domain.
    temper(home.path(), fake.path(), state.path())
        .args(["configure", "set", "update.mode", "nope"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("off | warn | prompt | auto"));

    // Good sets are written (comment-preserving) and stamp the version.
    temper(home.path(), fake.path(), state.path())
        .args(["configure", "set", "git.auto_push", "true"])
        .assert()
        .success();
    temper(home.path(), fake.path(), state.path())
        .args(["configure", "set", "update.mode", "auto"])
        .assert()
        .success();
    let after = fs::read_to_string(&tt).unwrap();
    assert!(after.contains("[git]") && after.contains("auto_push = true"));
    assert!(after.contains("[update]") && after.contains("mode = \"auto\""));
    assert!(after.contains("temper_version =")); // stamped
    assert!(after.contains("# fleet") && after.contains("EDITOR = \"hx\"")); // preserved

    // `get` prints the bare value (composes in scripts); bools show on/off.
    temper(home.path(), fake.path(), state.path())
        .args(["configure", "get", "git.auto_push"])
        .assert()
        .success()
        .stdout(predicates::str::diff("on\n"));

    // `unset` reverts to default.
    temper(home.path(), fake.path(), state.path())
        .args(["configure", "unset", "git.auto_push"])
        .assert()
        .success();
    temper(home.path(), fake.path(), state.path())
        .args(["configure", "get", "git.auto_push"])
        .assert()
        .success()
        .stdout(predicates::str::diff("off\n"));
}

#[test]
fn status_shows_home_machine_and_settings() {
    let home = TempDir::new().unwrap();
    let fake = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    // A sole machine resolves regardless of hostname, so a static name is fine.
    fs::write(
        home.path().join("temper.toml"),
        format!(
            "[[machine]]\nname = \"box\"\nos = \"{}\"\n[update]\nmode = \"warn\"\n",
            os()
        ),
    )
    .unwrap();

    temper(home.path(), fake.path(), state.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicates::str::contains("home:"))
        .stdout(predicates::str::contains("git:"))
        .stdout(predicates::str::contains("update:"))
        .stdout(predicates::str::contains("mode=warn"));
}

#[test]
fn completions_expose_every_configure_key() {
    // The keys MUST be shell-completable or nobody finds them. clap's
    // PossibleValuesParser puts them in the generated script.
    let home = TempDir::new().unwrap();
    let fake = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    for key in [
        "git.remind",
        "git.auto_commit",
        "git.auto_push",
        "git.auto_pull",
        "git.auto_rebase",
        "update.mode",
    ] {
        temper(home.path(), fake.path(), state.path())
            .args(["completions", "zsh"])
            .assert()
            .success()
            .stdout(predicates::str::contains(key));
    }
}
