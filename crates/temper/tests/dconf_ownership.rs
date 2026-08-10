//! Ownership derivation fails **closed**.
//!
//! temper works out which dconf keys a `setkey` step owns by resolving the
//! machine's bundles, and excludes those from a capture so the snapshot does not
//! become a second owner of them. Both that derivation and the snapshot list
//! swallowed a folder error and carried on with "nothing is owned" / "no
//! extension snapshots" — the most dangerous possible answer, because it silently
//! restores exactly the condition the mechanism exists to prevent.

use std::fs;

use assert_cmd::Command;
use tempfile::TempDir;

fn os() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    }
}

/// A bundle temper cannot parse stops a capture, rather than quietly widening
/// what it captures.
#[test]
fn an_unreadable_bundle_stops_a_capture_instead_of_widening_it() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let h = home.path();

    fs::create_dir_all(h.join("apps")).unwrap();
    fs::write(
        h.join("temper.toml"),
        format!(
            "[[machine]]\nname = \"t\"\nos = \"{}\"\napps = [\"a\"]\n\n\
             [[machine.dconf]]\npath = \"/org/gnome/shell/\"\n\
             file = \"assets/shell.dconf\"\n",
            os()
        ),
    )
    .unwrap();
    // Valid TOML, invalid schema: `deny_unknown_fields` rejects it. This is the
    // shape a typo takes, and it used to mean "no keys are owned".
    fs::write(h.join("apps/a.toml"), "not_a_real_field = true\n").unwrap();

    let out = Command::cargo_bin("temper")
        .unwrap()
        .args(["snapshot-dconf", "--json"])
        .env("TEMPER_DIR", h)
        .env("HOME", fake_home.path())
        .env("TEMPER_STATE_DIR", state.path())
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "a folder temper cannot read must not produce a capture: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !h.join("assets/shell.dconf").exists(),
        "and it must not have written a snapshot file"
    );
}
