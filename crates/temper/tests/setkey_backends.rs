//! Proves setkey toml + ini/.desktop (+ macOS defaults) backends, all inside
//! temp dirs. The defaults case targets a temp plist file, never a real domain.

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
fn setkey_toml_and_ini() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let h = home.path();
    fs::create_dir_all(h.join("apps")).unwrap();
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
        r#"
[[step]]
setkey = { backend = "toml", file = "~/.config/amdl/config.toml", key = "keys.acoustid", value = "abc123" }

[[step]]
setkey = { backend = "ini", file = "~/.local/share/applications/x.desktop", key = "Desktop Entry.Icon", value = "my-icon" }
"#,
    )
    .unwrap();

    // pre-seed the .desktop with a section + a sibling key to preserve
    let desktop = fake_home.path().join(".local/share/applications/x.desktop");
    fs::create_dir_all(desktop.parent().unwrap()).unwrap();
    fs::write(&desktop, "[Desktop Entry]\nName=X\nIcon=old-icon\n").unwrap();

    temper(h, fake_home.path(), state.path())
        .arg("install")
        .assert()
        .success();

    // toml: nested key set
    let cfg = fs::read_to_string(fake_home.path().join(".config/amdl/config.toml")).unwrap();
    let parsed: toml::Value = toml::from_str(&cfg).unwrap();
    assert_eq!(parsed["keys"]["acoustid"].as_str(), Some("abc123"));

    // ini: Icon replaced, Name preserved
    let d = fs::read_to_string(&desktop).unwrap();
    assert!(d.contains("Icon=my-icon"), "icon not set: {d:?}");
    assert!(d.contains("Name=X"), "sibling lost: {d:?}");
    assert!(!d.contains("old-icon"));

    // drift → both in sync
    temper(h, fake_home.path(), state.path())
        .arg("drift")
        .assert()
        .success()
        .stdout(predicates::str::contains("0 out of sync"));
}

#[test]
fn setkey_template_renders_a_var_and_stays_in_sync() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let h = home.path();
    fs::create_dir_all(h.join("apps")).unwrap();
    fs::write(
        h.join("temper.toml"),
        format!(
            "[vars]\nBIN = \"/opt/x/bin/app\"\n\n[[machine]]\nname = \"t\"\nos = \"{}\"\napps = [\"demo\"]\n",
            os()
        ),
    )
    .unwrap();
    // template = true → the {{ var }} is rendered at apply time (a literal {{ }}
    // in a value would otherwise stay literal).
    fs::write(
        h.join("apps/demo.toml"),
        "[[step]]\nsetkey = { backend = \"json\", file = \"~/.config/x.json\", key = \"command\", value = \"{{ var \\\"BIN\\\" }}\", template = true }\n",
    )
    .unwrap();

    temper(h, fake_home.path(), state.path())
        .arg("install")
        .assert()
        .success();

    let cfg = fs::read_to_string(fake_home.path().join(".config/x.json")).unwrap();
    assert!(
        cfg.contains("/opt/x/bin/app"),
        "template not rendered: {cfg}"
    );
    assert!(
        !cfg.contains("{{"),
        "unrendered template leaked into the file: {cfg}"
    );

    // Second run: the value re-renders to the same path and matches the file, so
    // a dynamic value doesn't report permanent false drift.
    temper(h, fake_home.path(), state.path())
        .arg("drift")
        .assert()
        .success()
        .stdout(predicates::str::contains("0 out of sync"));
}

#[cfg(target_os = "macos")]
#[test]
fn setkey_defaults_against_temp_plist() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let h = home.path();
    fs::create_dir_all(h.join("apps")).unwrap();
    fs::write(
        h.join("temper.toml"),
        "[[machine]]\nname = \"t\"\nos = \"mac\"\napps = [\"demo\"]\n",
    )
    .unwrap();
    // target is a temp plist path (~/prefs -> $HOME/prefs.plist), never a real domain
    fs::write(
        h.join("apps/demo.toml"),
        "[[step]]\nsetkey = { backend = \"defaults\", file = \"~/prefs\", key = \"TemperTestKey\", value = \"hello\" }\n",
    )
    .unwrap();

    temper(h, fake_home.path(), state.path())
        .arg("install")
        .assert()
        .success();

    // verify via the real `defaults` tool against the temp plist
    let out = std::process::Command::new("defaults")
        .args([
            "read",
            &format!("{}/prefs", fake_home.path().display()),
            "TemperTestKey",
        ])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");

    temper(h, fake_home.path(), state.path())
        .arg("drift")
        .assert()
        .success()
        .stdout(predicates::str::contains("0 out of sync"));
}
