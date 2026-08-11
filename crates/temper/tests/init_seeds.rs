//! `temper init` scaffolds a machine and then seeds it from live state.
//!
//! Two defects met here. The seed is `reconcile --current-state-wins` against
//! the block `init` has just written — which declares nothing — so every "a
//! manager is only probed once you declare one of its packages" gate answered
//! "nothing to look at" and the seed absorbed **nothing**, on a host with two
//! hundred formulae. That invariant is right for `drift` and exactly wrong for
//! the verb whose job is discovery.
//!
//! And under `--json` it printed the scaffold document and then let the seed
//! print a second, so stdout was two objects and parsed as neither.

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
        Env {
            home: TempDir::new().unwrap(),
            fake_home: TempDir::new().unwrap(),
            state: TempDir::new().unwrap(),
            bin: TempDir::new().unwrap(),
        }
    }

    /// A `brew` with two formulae and one cask installed, none of them declared
    /// anywhere — exactly the state `init` exists to capture.
    fn fake_brew(&self) {
        let bin = self.bin.path().join("brew");
        fs::write(
            &bin,
            r#"#!/bin/sh
case "$1 $2" in
  "list --formula") echo jq; echo ripgrep ;;
  "list --cask")    echo ghostty ;;
  "tap")            echo user/tap ;;
  "trust --json")   echo '{"taps":["user/tap"]}' ;;
  "bundle cleanup")
      # The dry listing: with an empty declared set every installed thing is an
      # orphan, which is what a seed wants to hear.
      echo "Would uninstall formulae:"
      echo "jq"
      echo "ripgrep"
      echo "ghostty"
      echo "Would untap:"
      echo "user/tap"
      # …and it exits NON-ZERO to say it found them, as the real one does. A
      # fake that exits 0 here agrees with whatever the caller assumes about the
      # exit code, which is how a regression reading it as failure passed a full
      # suite.
      exit 1
      ;;
  *) : ;;
esac
exit 0
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn temper(&self) -> Command {
        let mut c = Command::cargo_bin("temper").unwrap();
        c.env("TEMPER_DIR", self.home.path())
            .env("HOME", self.fake_home.path())
            .env("TEMPER_STATE_DIR", self.state.path())
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.path().display()));
        c
    }
}

#[test]
fn init_seeds_the_machines_packages_and_prints_one_document() {
    let e = Env::new();
    e.fake_brew();

    let out = e
        .temper()
        .args(["init", "probehost", "--json", "--yes"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    // One document, not two. `from_slice` on the whole of stdout is the
    // assertion — a second object makes it trailing garbage.
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}):\n{stdout}"));

    // …carrying BOTH halves: the scaffold facts and the seed's counts.
    assert_eq!(v["machine"], "probehost");
    assert_eq!(v["created_manifest"], true, "{v}");
    assert_eq!(v["brewfile"], "brewfiles/probehost", "{v}");
    assert!(
        v["added"].as_u64().unwrap_or(0) >= 4,
        "the seed absorbed {} entries — it should have found the three packages \
         and the tap: {v}",
        v["added"]
    );

    // And the Brewfile it wired up actually holds them.
    let bf = fs::read_to_string(e.home.path().join("brewfiles/probehost")).unwrap();
    for token in ["jq", "ripgrep", "ghostty", "user/tap"] {
        assert!(bf.contains(token), "`{token}` missing from the seed:\n{bf}");
    }
}

/// The seed is discovery; an ordinary `reconcile` is not. The probe opt-in has
/// to survive for every other caller, or a spec that declares no packages starts
/// reporting the whole machine.
#[test]
fn a_plain_reconcile_still_honours_the_probe_opt_in() {
    let e = Env::new();
    e.fake_brew();
    fs::write(
        e.home.path().join("temper.toml"),
        format!(
            "[[machine]]\nname = \"t\"\nos = \"{}\"\nbrewfile = \"brewfiles/t\"\n",
            if cfg!(target_os = "macos") { "mac" } else { "linux" }
        ),
    )
    .unwrap();
    fs::create_dir_all(e.home.path().join("brewfiles")).unwrap();
    fs::write(e.home.path().join("brewfiles/t"), "").unwrap();

    let out = e
        .temper()
        .args(["reconcile", "--csw", "--json"])
        .output()
        .unwrap();
    // Without `--yes` this is the preview document, so the candidate list is
    // what to read: `adds` empty means the opt-in still holds.
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["adds"].as_array().map(|a| a.len()).unwrap_or(0),
        0,
        "a spec declaring no packages must not have the machine's absorbed into \
         it by a plain reconcile: {v}"
    );
}
