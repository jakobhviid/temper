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
