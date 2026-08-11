//! Proves `copy` template/seed/mode and `install --dry-run`, all inside temp
//! dirs (HOME/TEMPER_DIR/state sandboxed) so the real machine is untouched.

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
fn template_seed_mode_and_dry_run() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let h = home.path();

    fs::create_dir_all(h.join("apps")).unwrap();
    fs::create_dir_all(h.join("assets")).unwrap();
    fs::write(
        h.join("assets/rendered.conf"),
        "prefix={{ var \"BREW_PREFIX\" }} sh={{ which \"sh\" }}\n",
    )
    .unwrap();
    fs::write(h.join("assets/seeded.conf"), "seed-default\n").unwrap();
    fs::write(h.join("assets/secret.conf"), "token\n").unwrap();
    fs::write(
        h.join("temper.toml"),
        format!(
            "[vars]\nBREW_PREFIX = \"/opt/homebrew\"\n\n\
             [[machine]]\nname = \"test\"\nos = \"{}\"\napps = [\"demo\"]\n",
            os()
        ),
    )
    .unwrap();
    fs::write(
        h.join("apps/demo.toml"),
        "[[step]]\ncopy = \"assets/rendered.conf\"\nto = \"~/.config/rendered.conf\"\ntemplate = true\n\n\
         [[step]]\ncopy = \"assets/seeded.conf\"\nto = \"~/.config/seeded.conf\"\nseed = true\n\n\
         [[step]]\ncopy = \"assets/secret.conf\"\nto = \"~/.secret.conf\"\nmode = \"0600\"\n",
    )
    .unwrap();

    let rendered = fake_home.path().join(".config/rendered.conf");
    let seeded = fake_home.path().join(".config/seeded.conf");
    let secret = fake_home.path().join(".secret.conf");

    // install → all three land
    temper(h, fake_home.path(), state.path())
        .arg("install")
        .assert()
        .success();

    // template: var substituted, {{ which }} resolved to a real path, no braces left
    let r = fs::read_to_string(&rendered).unwrap();
    assert!(
        r.contains("prefix=/opt/homebrew"),
        "var not substituted: {r:?}"
    );
    assert!(
        r.contains("sh=/") && !r.contains("{{"),
        "which not resolved: {r:?}"
    );

    // seed: created with the default
    assert_eq!(fs::read_to_string(&seeded).unwrap(), "seed-default\n");

    // mode: 0600 enforced
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&secret).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode not enforced: {mode:o}");
    }

    // seed is hands-off: user edits it, a re-install must NOT clobber it
    fs::write(&seeded, "user-edited\n").unwrap();
    temper(h, fake_home.path(), state.path())
        .arg("install")
        .assert()
        .success();
    assert_eq!(fs::read_to_string(&seeded).unwrap(), "user-edited\n");

    // dry-run: tamper the templated file, preview reports a change but writes nothing
    fs::write(&rendered, "tampered\n").unwrap();
    temper(h, fake_home.path(), state.path())
        .args(["install", "--dry-run"])
        .assert()
        .success()
        .stdout(predicates::str::contains("would apply"));
    assert_eq!(
        fs::read_to_string(&rendered).unwrap(),
        "tampered\n",
        "dry-run must not write"
    );

    // real install → fixes it
    temper(h, fake_home.path(), state.path())
        .arg("install")
        .assert()
        .success();
    assert!(fs::read_to_string(&rendered)
        .unwrap()
        .contains("prefix=/opt/homebrew"));
}

/// A declared `mode` is drift-checked, not just enforced.
///
/// `copy_state` compared content only, so a file whose permissions had been
/// changed reported **in sync** — while every converge silently chmod'd it back
/// and reported "0 changed". Enforcement without a drift story is exactly what
/// Principle #7 forbids, and `sysfile` has compared mode all along.
#[cfg(unix)]
#[test]
fn a_declared_mode_is_drift_checked_and_counted() {
    use std::os::unix::fs::PermissionsExt;

    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let h = home.path();
    let os = if cfg!(target_os = "macos") { "mac" } else { "linux" };

    fs::create_dir_all(h.join("apps")).unwrap();
    fs::create_dir_all(h.join("assets")).unwrap();
    fs::write(h.join("assets/key"), "secret\n").unwrap();
    fs::write(
        h.join("temper.toml"),
        format!("[[machine]]\nname = \"t\"\nos = \"{os}\"\napps = [\"a\"]\n"),
    )
    .unwrap();
    fs::write(
        h.join("apps/a.toml"),
        "[[step]]\ncopy = \"assets/key\"\nto = \"~/key\"\nmode = \"0600\"\n",
    )
    .unwrap();

    let temper = || {
        let mut c = Command::cargo_bin("temper").unwrap();
        c.env("TEMPER_DIR", h)
            .env("HOME", fake_home.path())
            .env("TEMPER_STATE_DIR", state.path());
        c
    };

    temper().arg("install").assert().success();
    let deployed = fake_home.path().join("key");
    assert_eq!(
        fs::metadata(&deployed).unwrap().permissions().mode() & 0o7777,
        0o600
    );

    // Content untouched, permissions widened — the case that read as in sync.
    fs::set_permissions(&deployed, fs::Permissions::from_mode(0o644)).unwrap();

    let out = temper().args(["drift", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["out_of_sync"], 1,
        "a file with the wrong mode is out of sync: {v}"
    );

    // …and fixing it counts as a change, rather than happening silently.
    let out = temper().arg("install").output().unwrap();
    let said = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        said.contains("1 changed"),
        "a mode-only correction is still a change:\n{said}"
    );
    assert_eq!(
        fs::metadata(&deployed).unwrap().permissions().mode() & 0o7777,
        0o600
    );
}
