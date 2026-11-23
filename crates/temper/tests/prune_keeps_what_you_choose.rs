//! `prune` removes what you choose and leaves the rest in place, for this run.
//!
//! The choice is made per item — `c` at the confirm, or `--keep` — and the
//! half that matters is the executor, not the preview: `brew bundle cleanup`
//! decides for itself what to remove, so a kept formula the file it reads does
//! not name is uninstalled anyway, outside the plan the user confirmed.
//!
//! brew is faked so this measures temper rather than the developer's machine,
//! and so the destructive call can be observed instead of performed.

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

struct Env {
    home: TempDir,
    fake_home: TempDir,
    state: TempDir,
    bin: TempDir,
}

impl Env {
    fn new() -> Env {
        Env {
            home: TempDir::new().unwrap(),
            fake_home: TempDir::new().unwrap(),
            state: TempDir::new().unwrap(),
            bin: TempDir::new().unwrap(),
        }
    }

    fn cleanup_log(&self) -> std::path::PathBuf {
        self.fake_home.path().join("cleanup-args")
    }
    fn brewfile_seen(&self) -> std::path::PathBuf {
        self.fake_home.path().join("cleanup-brewfile")
    }

    /// A `brew` with one declared formula and two orphans, that records what
    /// `bundle cleanup --force` was handed instead of removing anything.
    fn fake_brew(&self) {
        let bin = self.bin.path().join("brew");
        fs::write(
            &bin,
            format!(
                r#"#!/bin/sh
case "$1 $2" in
  "list --formula") echo keeper; echo trying-out; echo done-with ;;
  "trust --json")   echo '{{"taps":[]}}' ;;
  "bundle cleanup")
      case " $* " in
        *" --force "*)
            echo "$@" > {log}
            for a in "$@"; do [ -f "$a" ] && cp "$a" {seen}; done
            ;;
        *)  # The dry listing exits non-zero when it finds orphans, as brew does.
            echo "Would uninstall formulae:"
            echo "trying-out"
            echo "done-with"
            exit 1
            ;;
      esac
      ;;
  *) : ;;
esac
exit 0
"#,
                log = self.cleanup_log().display(),
                seen = self.brewfile_seen().display(),
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn spec(&self, extra: &str) {
        fs::write(
            self.home.path().join("temper.toml"),
            format!(
                "[[machine]]\nname = \"t\"\nos = \"{}\"\npackages = [\"brew \\\"keeper\\\"\"]\n{extra}",
                os()
            ),
        )
        .unwrap();
    }

    fn temper(&self) -> Command {
        let mut c = Command::cargo_bin("temper").unwrap();
        c.env("TEMPER_DIR", self.home.path())
            .env("HOME", self.fake_home.path())
            .env("TEMPER_STATE_DIR", self.state.path())
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.path().display()));
        c
    }

    /// The cleanup file's formula lines — what brew was told may stay.
    fn handed(&self) -> Vec<String> {
        read(&self.brewfile_seen())
            .lines()
            .filter_map(|l| l.strip_prefix("brew \"").and_then(|r| r.split('"').next()))
            .map(String::from)
            .collect()
    }
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_default()
}

fn extras(v: &serde_json::Value) -> Vec<&str> {
    v["extras"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect()
}

/// `--keep` takes the formula out of the plan, says so in `--json`, and hands
/// it to brew as one that stays.
#[test]
fn a_kept_formula_is_left_installed() {
    let e = Env::new();
    e.fake_brew();
    e.spec("");

    let out = e.temper().args(["prune", "--json", "--keep", "trying-out"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(extras(&v), vec!["done-with"], "{v}");
    assert_eq!(
        v["kept"],
        serde_json::json!([{ "list": "packages", "item": "brew trying-out", "why": null }]),
        "{v}"
    );

    e.temper().args(["prune", "--yes", "--keep", "trying-out"]).assert().success();
    let handed = e.handed();
    assert!(
        handed.contains(&"trying-out".to_string()),
        "the kept formula was handed to brew as an orphan — cleanup would \
         uninstall it. Formulae named: {handed:?}"
    );
    assert!(
        !handed.contains(&"done-with".to_string()),
        "the formula nobody kept was protected too: {handed:?}"
    );
}

/// `c` asks about each item; Enter keeps it. The plan is worked out again from
/// what was kept, shown, and confirmed before anything runs.
#[test]
fn choosing_per_item_prunes_only_what_you_said_yes_to() {
    let e = Env::new();
    e.fake_brew();
    e.spec("");

    // c to choose; Enter keeps trying-out; y prunes done-with; y confirms the
    // plan worked out again from that.
    let out = e
        .temper()
        .arg("prune")
        .write_stdin("c\n\ny\ny\n")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("uninstall brew trying-out?"), "{stdout}");
    assert!(stdout.contains("keep brew trying-out"), "the new plan must say what it keeps: {stdout}");
    // The fake still lists what cleanup was told to remove, so the count reads
    // it back as not removed; what is asserted here is what was kept.
    assert!(stdout.contains(", 1 left in place"), "{stdout}");

    let handed = e.handed();
    assert!(handed.contains(&"trying-out".to_string()), "{handed:?}");
    assert!(!handed.contains(&"done-with".to_string()), "{handed:?}");
}

/// Keeping everything is a prune with nothing to do, and brew is not called.
#[test]
fn keeping_every_item_removes_nothing() {
    let e = Env::new();
    e.fake_brew();
    e.spec("");

    let out = e.temper().arg("prune").write_stdin("c\n\n\n").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("nothing to remove — 2 item(s) left in place"), "{stdout}");
    assert!(read(&e.cleanup_log()).is_empty(), "cleanup ran: {}", read(&e.cleanup_log()));
}

/// A `--keep` that matches nothing stops the run: `--keep tryingout --yes`
/// would otherwise remove the formula it was written to protect.
#[test]
fn a_keep_that_matches_nothing_removes_nothing() {
    let e = Env::new();
    e.fake_brew();
    e.spec("");

    let out = e
        .temper()
        .args(["prune", "--yes", "--keep", "tryingout"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("names nothing this prune would act on"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(read(&e.cleanup_log()).is_empty(), "cleanup ran: {}", read(&e.cleanup_log()));
}

/// Files are choices too. A retired path has no ignore list, so the plan takes
/// it out directly — by the path as the spec writes it, or as the shell
/// expanded it.
#[test]
fn a_kept_path_stays() {
    let e = Env::new();
    e.fake_brew();
    e.spec("retire = [\"~/.config/old-a\", \"~/.config/old-b\", \"~/.config/old-c\"]\n");
    let dir = e.fake_home.path().join(".config");
    for f in ["old-a", "old-b", "old-c"] {
        fs::create_dir_all(dir.join(f)).unwrap();
    }

    let expanded = dir.join("old-b").display().to_string();
    e.temper()
        .args(["prune", "--yes", "--keep", "~/.config/old-a", "--keep", &expanded])
        .args(["--keep", "trying-out", "--keep", "done-with"])
        .assert()
        .success();
    assert!(dir.join("old-a").exists(), "kept by the spec's spelling, and removed");
    assert!(dir.join("old-b").exists(), "kept by the expanded path, and removed");
    assert!(!dir.join("old-c").exists(), "not kept, and still here");
}
