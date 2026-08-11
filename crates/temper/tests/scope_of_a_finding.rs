//! Scope is a property of the **declaration**, not of the kind.
//!
//! This is the bug the whole scope model exists to prevent, and it survived one
//! notch narrower than the original. `KIND_ANSWERS` answers per kind, so a
//! missing GNOME extension named `temper reconcile` whoever declared it — while
//! reconcile's drop candidates are machine-scope only. For a bundle-declared
//! one it silently did nothing, every converge put the extension back, and the
//! only way out was a hand edit nothing pointed you at.
//!
//! `gnome-extensions` is absent from this host, so drift reports nothing for it
//! and the assertion would be vacuous. rpm-ostree is here, which makes the
//! Linux case the one with teeth; the mechanism is shared.

use std::fs;

use assert_cmd::Command;
use tempfile::TempDir;

/// Skip where the probe cannot answer — the finding would not be emitted at all,
/// and a vacuous pass is worse than an honest skip.
fn rpm_ostree_present() -> bool {
    std::process::Command::new("rpm-ostree")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn a_bundle_declared_item_names_its_file_not_a_verb_that_cannot_act() {
    if !cfg!(target_os = "linux") || !rpm_ostree_present() {
        eprintln!("skipped: needs rpm-ostree");
        return;
    }
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
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

    let out = Command::cargo_bin("temper")
        .unwrap()
        .args(["drift", "--json"])
        .env("TEMPER_DIR", h)
        .env("HOME", fake_home.path())
        .env("TEMPER_STATE_DIR", state.path())
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
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
    let out = Command::cargo_bin("temper")
        .unwrap()
        .args(["drift", "--json"])
        .env("TEMPER_DIR", h)
        .env("HOME", fake_home.path())
        .env("TEMPER_STATE_DIR", state.path())
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
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
