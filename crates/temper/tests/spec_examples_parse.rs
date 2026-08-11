//! SPEC's worked examples must actually parse.
//!
//! SPEC is the parser-of-record, and its examples are what a human or an agent
//! copies to start a folder. Three of them did not parse at all, and had not for
//! some time — nothing ever fed them to the parser they document:
//!
//!   * an inline table split across lines (`{ uuid = "x",\n  settings = "y" }`),
//!     which TOML 1.0 does not permit however readable it looks;
//!   * `[[assert]] absent = "…"` — a table header and a key cannot share a line,
//!     so the entire assertion reference was unusable;
//!   * `[ignore]` opened before the bundle-level keys, so `packages`,
//!     `gnome_extensions` and `rpm_ostree` all parsed as `[ignore].*` and were
//!     rejected as unknown fields.
//!
//! The sibling test `spec_is_the_parser_of_record` checks SPEC names every field
//! the parser accepts. This one checks the reverse direction: that what SPEC
//! shows, the parser takes.

use std::fs;

use assert_cmd::Command;
use tempfile::TempDir;

/// Every ```toml block in SPEC, in document order.
fn toml_blocks() -> Vec<String> {
    let spec = include_str!("../../../SPEC.md");
    let mut out = Vec::new();
    let mut rest = spec;
    while let Some(i) = rest.find("```toml\n") {
        rest = &rest[i + "```toml\n".len()..];
        let end = rest.find("```").expect("unterminated toml block in SPEC.md");
        out.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    out
}

#[test]
fn every_spec_example_loads_as_a_real_folder() {
    let blocks = toml_blocks();
    assert!(
        blocks.len() >= 4,
        "expected SPEC's manifest, bundle, step and assert examples; found {}",
        blocks.len()
    );

    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let h = home.path();
    fs::create_dir_all(h.join("apps")).unwrap();
    fs::create_dir_all(h.join("assets")).unwrap();

    // Block 0 is `temper.toml`; the rest are the bundle, its steps and its
    // asserts, which belong to one file.
    let manifest = blocks[0]
        .replace(
            "apps     = [\"shell\", \"ssh\"] # bundle names in apps/",
            "apps     = [\"demo\"]",
        )
        // The stamp is a deliberate future version, and a skew warning is not
        // what this test is about.
        .replace("temper_version = \"1.42.0\"", "temper_version = \"0.0.1\"");
    fs::write(h.join("temper.toml"), manifest).unwrap();
    // The bundle block ends with two ellipsis placeholders — `[[step]] # … see
    // steps below …` — which are prose, not entries. Concatenated with the real
    // step and assert blocks they would become empty tables, and temper
    // correctly refuses an assertion with no check.
    let bundle = blocks[1]
        .split("[[step]]   # ordered")
        .next()
        .expect("bundle block")
        .to_string();
    let mut file = bundle;
    for b in &blocks[2..] {
        file.push('\n');
        file.push_str(b);
    }
    fs::write(h.join("apps/demo.toml"), file).unwrap();

    // The examples reference assets by name; create them so a missing file
    // cannot be mistaken for a schema error.
    for f in [
        "x.conf", "snippet", "setup.sh", "check.sh", "x.mobileconfig",
        "1password.policy", "reference.json",
    ] {
        fs::write(h.join("assets").join(f), "# fixture\n").unwrap();
    }

    // SPEC's `setkey` example renders `{{ which "ghostty" }}`, which resolves at
    // load — so on a host without ghostty the folder fails to load and this test
    // reports a schema error that isn't one. It did exactly that on CI while
    // passing on the developer's machine, where ghostty happens to be installed.
    //
    // The example stays honest (ghostty is the real-world case); the test stops
    // asking the host what software it has. Every binary SPEC names is provided
    // here, on a PATH this test controls.
    let bin = TempDir::new().unwrap();
    stub(bin.path(), "ghostty");

    let out = Command::cargo_bin("temper")
        .unwrap()
        .args(["drift", "--json"])
        .env("TEMPER_DIR", h)
        .env("HOME", fake_home.path())
        .env("TEMPER_STATE_DIR", state.path())
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.path().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !stderr.contains("TOML parse error") && !stderr.contains("unknown field"),
        "SPEC's examples do not parse — the document teaching the schema is \
         rejected by the parser it documents:\n{stderr}"
    );
    // …and the whole thing loads, so the folder is coherent and not merely
    // syntactically valid.
    assert!(
        out.status.success(),
        "SPEC's examples parse but the folder does not load:\n{stderr}"
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("drift did not emit a document ({e})"));
    assert_eq!(v["machine"], "chronos", "SPEC's example machine should resolve");
}

/// A do-nothing executable, so a `which` in SPEC resolves without asking the
/// host whether it happens to have that software installed.
fn stub(dir: &std::path::Path, name: &str) {
    let p = dir.join(name);
    fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
}
