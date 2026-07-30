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
fn autoupdate_sets_and_rejects_the_mode() {
    let home = TempDir::new().unwrap();
    let fake = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let tt = home.path().join("temper.toml");
    fs::write(&tt, "# fleet\n[vars]\nEDITOR = \"hx\"\n").unwrap();

    // A bad value names itself and changes nothing.
    temper(home.path(), fake.path(), state.path())
        .args(["autoupdate", "nope"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("expected off | warn | prompt | auto"));

    // A good value is written (comment-preserving) and stamps the version.
    temper(home.path(), fake.path(), state.path())
        .args(["autoupdate", "auto"])
        .assert()
        .success();
    let after = fs::read_to_string(&tt).unwrap();
    assert!(after.contains("[update]") && after.contains("mode = \"auto\""));
    assert!(after.contains("temper_version =")); // stamped on write
    assert!(after.contains("# fleet") && after.contains("EDITOR = \"hx\"")); // preserved

    // Showing the mode reports what we set.
    temper(home.path(), fake.path(), state.path())
        .arg("autoupdate")
        .assert()
        .success()
        .stdout(predicates::str::contains("autoupdate mode: auto"));
}
