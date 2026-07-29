//! Proves the newer `[[assert]]` checks: not_member, shell, json_semantic.
//! Read-only (drift), so it only inspects — never mutates the machine.

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
fn assert_not_member_shell_json_semantic() {
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
    // reference json files (in the temper-home)
    fs::write(h.join("ref-eq.json"), "{\"b\":2,\"a\":1}\n").unwrap();
    fs::write(h.join("ref-diff.json"), "{\"x\":2}\n").unwrap();
    fs::write(
        h.join("apps/demo.toml"),
        r#"
[[assert]]
not_member = { group = "temper-nonexistent-grp" }        # ok

[[assert]]
json_semantic = { file = "~/.config/eq.json", against = "ref-eq.json" }   # ok (order-independent)

[[assert]]
json_semantic = { file = "~/.config/diff.json", against = "ref-diff.json" }  # violated

[[assert]]
shell = "/bin/nonexistent-shell"                          # violated
"#,
    )
    .unwrap();

    // deployed json (on the "machine")
    let cfg = fake_home.path().join(".config");
    fs::create_dir_all(&cfg).unwrap();
    fs::write(cfg.join("eq.json"), "{\"a\":1,\"b\":2}\n").unwrap(); // same as ref, different order
    fs::write(cfg.join("diff.json"), "{\"x\":1}\n").unwrap(); // differs from ref

    // USER must be set for the shell assert's lookup.
    // Human view: the two violations are surfaced; in-sync asserts (not_member,
    // the order-independent json match) are collapsed, so their status text is
    // intentionally absent here — see the --json check below for those.
    temper(h, fake_home.path(), state.path())
        .env(
            "USER",
            std::env::var("USER").unwrap_or_else(|_| "runner".into()),
        )
        .arg("drift")
        .assert()
        .success()
        .stdout(predicates::str::contains("2 out of sync"))
        .stdout(predicates::str::contains("differs from reference"));

    // --json carries every finding (flat, uncollapsed) — assert the in-sync
    // not_member check is present and ok there.
    temper(h, fake_home.path(), state.path())
        .env(
            "USER",
            std::env::var("USER").unwrap_or_else(|_| "runner".into()),
        )
        .args(["--json", "drift"])
        .assert()
        .success()
        .stdout(predicates::str::contains("not a member"))
        .stdout(predicates::str::contains("\"out_of_sync\":2"));
}
