//! Scope is a property of the **declaration**, not of the kind.
//!
//! This is the bug the whole scope model exists to prevent, and it survived one
//! notch narrower than the original. `KIND_ANSWERS` answers per kind, so a
//! missing GNOME extension named `temper reconcile` whoever declared it — while
//! reconcile's drop candidates are machine-scope only. For a bundle-declared
//! one it silently did nothing, every converge put the extension back, and the
//! only way out was a hand edit nothing pointed you at.
//!
//! rpm-ostree is the case with teeth, and it is **faked** here rather than
//! required. This test used to return early unless the host had a real
//! `rpm-ostree` — so on CI, which is `ubuntu-latest`, the one test guarding the
//! defect the whole scope model exists to prevent was a green no-op. Its own
//! comment said "a vacuous pass is worse than an honest skip" while being one.

use std::fs;

use assert_cmd::Command;
use tempfile::TempDir;

/// An `rpm-ostree` that reports a booted deployment layering nothing, so a
/// declared package reads as genuinely missing.
///
/// `--version` answers because `have()` gates the whole provider on it, and
/// `status --json` carries the shape `requested_from_status` parses: one booted
/// deployment, no `requested-packages`.
fn fake_rpm_ostree(dir: &std::path::Path) {
    let p = dir.join("rpm-ostree");
    fs::write(
        &p,
        r#"#!/bin/sh
case "$1 $2" in
  "status --json") echo '{"deployments":[{"booted":true,"requested-packages":[]}]}' ;;
  *) : ;;
esac
exit 0
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
}
/// `drift --json` against a folder, with the fake on PATH.
fn drift(h: &std::path::Path, fake_home: &std::path::Path, state: &std::path::Path, bin: &std::path::Path) -> serde_json::Value {
    let out = Command::cargo_bin("temper")
        .unwrap()
        .args(["drift", "--json"])
        .env("TEMPER_DIR", h)
        .env("HOME", fake_home)
        .env("XDG_CONFIG_HOME", fake_home.join(".config"))
        .env("TEMPER_STATE_DIR", state)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .output()
        .unwrap();
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "drift did not emit one document ({e}):\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

#[test]
fn a_bundle_declared_item_names_its_file_not_a_verb_that_cannot_act() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();
    fake_rpm_ostree(bin.path());
    let h = home.path();

    fs::create_dir_all(h.join("apps")).unwrap();
    fs::write(
        h.join("temper.toml"),
        "[[machine]]\nname = \"t\"\nos = \"linux\"\napps = [\"vpn\"]\n",
    )
    .unwrap();
    // Declared in a BUNDLE — fleet scope. Nothing on this box layers it.
    fs::write(
        h.join("apps/vpn.toml"),
        "rpm_ostree = [\"temper-test-not-layered-anywhere\"]\n",
    )
    .unwrap();

    let v = drift(h, fake_home.path(), state.path(), bin.path());
    let item = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "rpm-ostree" && i["status"] == "missing")
        .unwrap_or_else(|| panic!("expected a missing rpm-ostree finding: {v}"));

    let detail = item["detail"].as_str().unwrap_or("");
    assert!(
        detail.contains("apps/vpn.toml"),
        "a bundle-declared item must name the file a human edits, got {detail:?}"
    );

    // And the machine-scope case keeps the verb, because reconcile really can
    // drop from the machine's own list.
    fs::write(
        h.join("temper.toml"),
        "[[machine]]\nname = \"t\"\nos = \"linux\"\n\
         rpm_ostree = [\"temper-test-not-layered-anywhere\"]\n",
    )
    .unwrap();
    let v = drift(h, fake_home.path(), state.path(), bin.path());
    let item = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "rpm-ostree" && i["status"] == "missing")
        .unwrap_or_else(|| panic!("expected a missing rpm-ostree finding: {v}"));
    assert!(
        item["detail"].is_null(),
        "a machine-declared item is reconcile's to drop, so it needs no hand-edit \
         note: {item}"
    );
}

/// A verb that has checked one row of the matrix may not announce a verdict on
/// all of it.
///
/// `adopt` looks at installed packages and nothing else, and said "nothing to
/// adopt — machine matches its spec" on a box where `drift` was reporting
/// eleven changed desktop keys, two extensions switched off against their
/// declaration, and a failing assertion. Every one of those was true and none
/// was a package. The reader stops, because the tool told them they were done.
#[test]
fn adopt_does_not_claim_the_machine_matches_its_spec() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let h = home.path();
    fs::write(
        h.join("temper.toml"),
        "[[machine]]\nname = \"t\"\nos = \"linux\"\n",
    )
    .unwrap();

    let out = Command::cargo_bin("temper")
        .unwrap()
        .args(["adopt"])
        .env("TEMPER_DIR", h)
        .env("HOME", fake_home.path())
        .env("TEMPER_STATE_DIR", state.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("matches its spec") && !stdout.contains("in sync"),
        "`adopt` checked packages only — it cannot report on the machine:\n{stdout}"
    );
    assert!(
        stdout.contains("package"),
        "the empty result has to name the row it looked at:\n{stdout}"
    );
    assert!(
        stdout.contains("drift"),
        "…and point at the verb that covers the rest:\n{stdout}"
    );
}
