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
        // XDG_CONFIG_HOME wins over HOME when temper locates the dconf
        // database, and DCONF_PROFILE makes it unknowable.
        .env("XDG_CONFIG_HOME", fake_home.join(".config"))
        .env_remove("DCONF_PROFILE")
        .env("TEMPER_STATE_DIR", state);
    c
}

#[test]
fn reconcile_without_brewfile_skips_packages_instead_of_failing() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    fs::write(
        home.path().join("temper.toml"),
        format!("[[machine]]\nname = \"t\"\nos = \"{}\"\n", os()),
    )
    .unwrap();

    // No `brewfile` is no longer fatal: there is nothing to write a package to,
    // but the desktop (dconf) half of reconcile still has to be reachable. With
    // no snapshots declared either, that leaves nothing to do at all.
    temper(home.path(), fake_home.path(), state.path())
        .arg("reconcile")
        .assert()
        .success()
        .stdout(predicates::str::contains("nothing for reconcile to absorb"));

    temper(home.path(), fake_home.path(), state.path())
        .args(["--json", "reconcile"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"brewfile\":null"))
        .stdout(predicates::str::contains("\"dconf\":[]"));
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
        .stdout(predicates::str::contains("nothing for reconcile to absorb"));

    // --json previews the (empty) plan without prompting.
    temper(home.path(), fake_home.path(), state.path())
        .args(["--json", "reconcile"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"adds\":[]"))
        .stdout(predicates::str::contains("\"drops\":[]"));
}
