//! `prune` may only remove what the spec leaves undeclared — and `[ignore]`
//! counts as declared for that purpose.
//!
//! Two defects with one shape: the *plan* honoured the spec and the *executor*
//! did not. `brew bundle cleanup` decides for itself what to remove, so whatever
//! temper leaves out of the file it hands brew is taken away — and the file was
//! built from the declared set alone, which by Principle #4 never contains an
//! ignored package. Separately, tap-trust had no "declares at least one" opt-in,
//! so a spec silent about taps read as a spec demanding every tap be untrusted.
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

    /// A `brew` that reports two installed formulae and three trusted taps, and
    /// records what `bundle cleanup` was asked to do instead of doing it.
    fn fake_brew(&self) {
        let bin = self.bin.path().join("brew");
        fs::write(
            &bin,
            format!(
                r#"#!/bin/sh
case "$1 $2" in
  "list --formula") echo keeper; echo ignored-tool ;;
  "trust --json")   echo '{{"taps":["a/one","b/two","c/three"]}}' ;;
  "bundle cleanup")
      # Two callers, and only one is destructive. Without --force this is the
      # dry listing `brew_extras` uses to compute the plan; with it, this is the
      # executor actually removing things. Only the second is under test.
      case " $* " in
        *" --force "*)
            echo "$@" > {log}
            # The file temper hands us IS its claim about what may stay.
            for a in "$@"; do [ -f "$a" ] && cp "$a" {seen}; done
            ;;
        *)  # Real `brew bundle cleanup` exits NON-ZERO when it finds orphans —
            # that is its normal result, like `diff`. The fake must do the same,
            # or a caller that reads the exit code as failure passes here and
            # reports zero extras on every real machine that has any.
            echo "Would uninstall formulae:"
            echo "ignored-tool"
            echo "stray-orphan"
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

    fn spec(&self, body: &str) {
        fs::write(self.home.path().join("temper.toml"), body).unwrap();
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

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_default()
}

/// An ignored package is not an orphan. It must appear in the file brew reads,
/// or cleanup removes it — outside the plan the user confirmed.
#[test]
fn an_ignored_package_is_not_offered_to_brew_as_an_orphan() {
    let e = Env::new();
    e.fake_brew();
    e.spec(&format!(
        "[ignore]\nbrew = [\"ignored-tool\"]\n\n\
         [[machine]]\nname = \"t\"\nos = \"{}\"\npackages = [\"brew \\\"keeper\\\"\"]\n",
        os()
    ));

    // brew offers BOTH as orphans. The plan must drop the ignored one…
    let out = e.temper().args(["prune", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let extras: Vec<&str> = v["extras"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        extras,
        vec!["stray-orphan"],
        "an ignored package must not reach the plan as an extra: {v}"
    );

    // …and so must the executor, which is the half that was wrong: the plan
    // filtered `ignore` out of its *output*, then handed brew a file that never
    // mentioned it.
    e.temper().args(["prune", "--yes"]).output().unwrap();

    let handed = read(&e.brewfile_seen());
    assert!(
        !handed.is_empty(),
        "cleanup should have run; args were: {}",
        read(&e.cleanup_log())
    );
    assert!(
        handed.contains("ignored-tool"),
        "the ignored package was handed to brew as an orphan — cleanup would \
         uninstall it, unpreviewed and unconfirmed. File was:\n{handed}"
    );
    assert!(handed.contains("keeper"), "declared package missing:\n{handed}");
}

/// A spec that says nothing about taps is not a spec asking for every tap to be
/// untrusted. Same opt-in every other provider has.
#[test]
fn a_spec_silent_about_taps_untrusts_nothing() {
    let e = Env::new();
    e.fake_brew();
    e.spec(&format!(
        "[[machine]]\nname = \"t\"\nos = \"{}\"\npackages = [\"brew \\\"keeper\\\"\"]\n",
        os()
    ));

    let out = e.temper().args(["prune", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let untrust = v["untrust"].as_array().map(|a| a.len()).unwrap_or(0);
    assert_eq!(
        untrust, 0,
        "prune would untrust {untrust} tap(s) on a spec that never mentions \
         taps — including the ones its own formulae come from: {v}"
    );

    // drift must agree: it is the same question, and a report that contradicts
    // the verb is how a user learns to distrust both.
    let out = e.temper().args(["drift", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let kinds: Vec<&str> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["kind"].as_str().unwrap())
        .collect();
    assert!(
        !kinds.contains(&"brew-trust-extra"),
        "drift reported tap-trust extras on a spec with no opinion about taps: {kinds:?}"
    );
}

/// The control: once the spec declares a tap, the extras direction works again.
#[test]
fn a_declared_tap_still_makes_the_others_extras() {
    let e = Env::new();
    e.fake_brew();
    e.spec(&format!(
        "[brew]\ntrust = [\"a/one\"]\n\n\
         [[machine]]\nname = \"t\"\nos = \"{}\"\npackages = [\"brew \\\"keeper\\\"\"]\n",
        os()
    ));

    let out = e.temper().args(["prune", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let untrust: Vec<&str> = v["untrust"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert_eq!(
        untrust,
        vec!["b/two", "c/three"],
        "declaring one tap opts the question in for the rest: {v}"
    );
}
