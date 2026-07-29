//! Proves `install --packages-only` (the additive "install-missing" flow)
//! converges packages but skips the config-step phase. Declares NO packages, so
//! it never shells out to a real package manager — everything stays in temp dirs.

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
fn packages_only_skips_config_steps() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let h = home.path();

    fs::create_dir_all(h.join("apps")).unwrap();
    fs::create_dir_all(h.join("assets")).unwrap();
    fs::write(h.join("assets/x.conf"), "managed\n").unwrap();
    fs::write(
        h.join("temper.toml"),
        format!(
            "[[machine]]\nname = \"t\"\nos = \"{}\"\napps = [\"demo\"]\n",
            os()
        ),
    )
    .unwrap();
    fs::write(
        h.join("apps/demo.toml"),
        "[[step]]\ncopy = \"assets/x.conf\"\nto = \"~/.config/x.conf\"\n",
    )
    .unwrap();

    let target = fake_home.path().join(".config/x.conf");

    // packages-only: config step must NOT run → the file is not created.
    temper(h, fake_home.path(), state.path())
        .args(["install", "t", "--packages-only", "--yes"])
        .assert()
        .success()
        .stdout(predicates::str::contains("config skipped"));
    assert!(
        !target.exists(),
        "packages-only should not have applied the copy step"
    );

    // full install: the config step runs → the file is created.
    temper(h, fake_home.path(), state.path())
        .args(["install", "t", "--yes"])
        .assert()
        .success();
    assert!(
        target.exists(),
        "full install should have applied the copy step"
    );
}
