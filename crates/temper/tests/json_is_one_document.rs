//! Every `--json` verb writes exactly one JSON document to stdout, and nothing
//! else.
//!
//! Principle #6b: human output to stdout, progress and errors to stderr, and one
//! stray `println!` makes the whole stream unparseable. Four separate ways of
//! breaking that turned up in one review — `init` printing its own document and
//! then letting the seed print a second, a failing `exec`'s stdout, `prune`'s
//! destructive children running before the document, and `--verbose` streaming
//! converge children into it. Each was fixed where it was found; nothing
//! asserted the contract itself.
//!
//! Read-only and preview verbs only. `prune` and `reconcile` are run **without**
//! `--yes`, which makes them previews; the destructive halves are covered by the
//! tests that exercise them deliberately.

use std::fs;

use assert_cmd::Command;
use tempfile::TempDir;

struct Env {
    home: TempDir,
    fake_home: TempDir,
    state: TempDir,
    bin: TempDir,
}

impl Env {
    fn new() -> Env {
        let e = Env {
            home: TempDir::new().unwrap(),
            fake_home: TempDir::new().unwrap(),
            state: TempDir::new().unwrap(),
            bin: TempDir::new().unwrap(),
        };
        let os = if cfg!(target_os = "macos") { "mac" } else { "linux" };
        fs::create_dir_all(e.home.path().join("apps")).unwrap();
        fs::create_dir_all(e.home.path().join("assets")).unwrap();
        fs::write(e.home.path().join("assets/x.conf"), "content\n").unwrap();
        fs::write(
            e.home.path().join("temper.toml"),
            format!(
                "[[machine]]\nname = \"t\"\nos = \"{os}\"\napps = [\"a\"]\n\
                 retire = [\"~/gone.conf\"]\n"
            ),
        )
        .unwrap();
        // A step that will drift (nothing deployed yet), plus an assertion — so
        // the documents have content rather than being trivially empty.
        fs::write(
            e.home.path().join("apps/a.toml"),
            "[[step]]\ncopy = \"assets/x.conf\"\nto = \"~/x.conf\"\n\n\
             [[assert]]\nabsent = \"~/never-here.conf\"\n",
        )
        .unwrap();
        e
    }

    fn temper(&self) -> Command {
        let mut c = Command::cargo_bin("temper").unwrap();
        c.env("TEMPER_DIR", self.home.path())
            .env("HOME", self.fake_home.path())
            .env("TEMPER_STATE_DIR", self.state.path())
            // Narrow PATH: no real package manager, so this measures temper's
            // own output rather than the developer's machine.
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.path().display()));
        c
    }
}

#[test]
fn every_read_only_verb_emits_exactly_one_json_document() {
    let e = Env::new();
    // `prune` and `reconcile` without `--yes` are previews. `install --dry-run`
    // writes nothing. `undo --list` is read-only.
    let invocations: &[&[&str]] = &[
        &["drift", "--json"],
        &["adopt", "--json"],
        &["retired", "--json"],
        &["status", "--json"],
        &["prune", "--json"],
        &["reconcile", "--json"],
        &["install", "--dry-run", "--json"],
        &["undo", "--list", "--json"],
        &["configure", "list", "--json"],
    ];

    for args in invocations {
        let out = e.temper().args(*args).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(
            !stdout.trim().is_empty(),
            "`temper {}` printed nothing on stdout — a --json verb owes a document",
            args.join(" ")
        );
        // The whole of stdout, not the first line: a second document would parse
        // as trailing garbage, which is exactly how `init` was broken.
        serde_json::from_str::<serde_json::Value>(&stdout).unwrap_or_else(|err| {
            panic!(
                "`temper {}` did not emit one JSON document ({err}):\n{stdout}",
                args.join(" ")
            )
        });
    }
}

/// `--verbose` composes with `--json`, and must not put a child's output in the
/// document. Both are global flags, so the combination is reachable by anyone.
#[test]
fn verbose_does_not_leak_into_the_document() {
    let e = Env::new();
    let out = e
        .temper()
        .args(["drift", "--json", "--verbose"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    serde_json::from_str::<serde_json::Value>(&stdout)
        .unwrap_or_else(|err| panic!("`--json --verbose` broke the document ({err}):\n{stdout}"));
}
