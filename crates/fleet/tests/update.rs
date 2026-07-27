//! Proves `update` re-applies `always` steps but leaves `install`-only steps
//! alone. Declares NO packages, so it never triggers a real `brew upgrade` —
//! everything stays inside temp dirs.

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
    let mut c = Command::cargo_bin("fleet").unwrap();
    c.env("FLEET_DIR", home)
        .env("HOME", fake_home)
        .env("FLEET_STATE_DIR", state);
    c
}

#[test]
fn update_reapplies_always_not_install_only() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let h = home.path();

    fs::create_dir_all(h.join("apps")).unwrap();
    fs::create_dir_all(h.join("assets")).unwrap();
    fs::write(h.join("assets/always.conf"), "managed\n").unwrap();
    fs::write(h.join("assets/once.conf"), "seed-default\n").unwrap();
    fs::write(
        h.join("fleet.toml"),
        format!("[[machine]]\nname = \"test\"\nos = \"{}\"\napps = [\"demo\"]\n", os()),
    )
    .unwrap();
    // one always-managed copy, one seed (install-only)
    fs::write(
        h.join("apps/demo.toml"),
        "[[step]]\ncopy = \"assets/always.conf\"\nto = \"~/.config/always.conf\"\n\n\
         [[step]]\ncopy = \"assets/once.conf\"\nto = \"~/.config/once.conf\"\nseed = true\n",
    )
    .unwrap();

    let always = fake_home.path().join(".config/always.conf");
    let once = fake_home.path().join(".config/once.conf");

    // install → both land
    fleet(h, fake_home.path(), state.path()).arg("install").assert().success();
    assert_eq!(fs::read_to_string(&always).unwrap(), "managed\n");
    assert_eq!(fs::read_to_string(&once).unwrap(), "seed-default\n");

    // tamper both
    fs::write(&always, "tampered\n").unwrap();
    fs::write(&once, "user-edited\n").unwrap();

    // update → re-applies the always step, leaves the seed (install-only) alone
    fleet(h, fake_home.path(), state.path())
        .arg("update")
        .assert()
        .success()
        .stdout(predicates::str::contains("re-applied 1 of 1"));
    assert_eq!(fs::read_to_string(&always).unwrap(), "managed\n", "always step not re-applied");
    assert_eq!(fs::read_to_string(&once).unwrap(), "user-edited\n", "install-only step wrongly re-applied");
}
