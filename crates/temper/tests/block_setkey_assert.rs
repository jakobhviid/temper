//! Proves `block`, `setkey(json)`, and `[[assert]]` — all inside temp dirs
//! (HOME/TEMPER_DIR/state sandboxed), so the real machine is untouched.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;
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
fn block_setkey_assert() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let h = home.path();

    fs::create_dir_all(h.join("apps")).unwrap();
    fs::create_dir_all(h.join("assets")).unwrap();
    fs::write(
        h.join("assets/ssh-include"),
        "Include config.d/shared.conf\n",
    )
    .unwrap();
    fs::write(
        h.join("temper.toml"),
        format!(
            "[[machine]]\nname = \"test\"\nos = \"{}\"\napps = [\"demo\"]\n",
            os()
        ),
    )
    .unwrap();
    fs::write(
        h.join("apps/demo.toml"),
        r#"
[[step]]
block = "assets/ssh-include"
in = "~/.ssh/config"
marker = "ssh"

[[step]]
setkey = { backend = "json", file = "~/.claude/settings.json", key = "env._ZO_DOCTOR", value = "0" }

[[assert]]
absent = "~/.zshrc.local"

[[assert]]
executable_resolves = "sh"

[[assert]]
contains_line = { file = "~/.ssh/config", line = "Include config.d/shared.conf" }
"#,
    )
    .unwrap();

    // Pre-seed user-owned files that temper must NOT clobber wholesale.
    let ssh = fake_home.path().join(".ssh/config");
    fs::create_dir_all(ssh.parent().unwrap()).unwrap();
    fs::write(&ssh, "Host example\n  User me\n").unwrap();
    let settings = fake_home.path().join(".claude/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(&settings, "{ \"other\": true }\n").unwrap();

    // install → block appended, setkey merged
    temper(h, fake_home.path(), state.path())
        .arg("install")
        .assert()
        .success();

    let ssh_body = fs::read_to_string(&ssh).unwrap();
    assert!(
        ssh_body.contains("Host example"),
        "user content lost: {ssh_body:?}"
    );
    assert!(
        ssh_body.contains("# >>> temper:ssh >>>"),
        "marker missing: {ssh_body:?}"
    );
    assert!(ssh_body.contains("Include config.d/shared.conf"));

    let v: Value = serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(v["other"], Value::Bool(true), "sibling key lost");
    assert_eq!(v["env"]["_ZO_DOCTOR"], Value::String("0".into()));

    // drift → everything satisfied (0 out of sync)
    temper(h, fake_home.path(), state.path())
        .arg("drift")
        .assert()
        .success()
        .stdout(predicates::str::contains("0 out of sync"));

    // block region update: change the source; re-install replaces the region,
    // preserves the user's Host block, and drops the old line.
    fs::write(
        h.join("assets/ssh-include"),
        "Include config.d/other.conf\n",
    )
    .unwrap();
    temper(h, fake_home.path(), state.path())
        .arg("install")
        .assert()
        .success();
    let ssh_body = fs::read_to_string(&ssh).unwrap();
    assert!(ssh_body.contains("Host example"));
    assert!(ssh_body.contains("Include config.d/other.conf"));
    assert!(
        !ssh_body.contains("shared.conf"),
        "old region not replaced: {ssh_body:?}"
    );

    // assert violation: the forbidden file appears → drift flags it
    fs::write(fake_home.path().join(".zshrc.local"), "oops\n").unwrap();
    temper(h, fake_home.path(), state.path())
        .arg("drift")
        .assert()
        .success()
        .stdout(predicates::str::contains("should not exist"));
}
