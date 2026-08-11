//! `prune` must not empty the trust store on its way past.
//!
//! `brew bundle cleanup --force` calls `Homebrew::Trust.replace!` with whatever
//! `trusted:` options appear in the file it was handed — **replace**, not merge
//! (`Library/Homebrew/bundle/subcommand/cleanup.rb`). temper declares trust in
//! `[brew].trust` / `brew_trust` rather than in a Brewfile, so the file it built
//! for cleanup mentioned no trust at all and every prune with a brew extra wiped
//! the store outright.
//!
//! Observed on a real machine: one `prune` untrusted `jakobhviid/tap` — declared
//! at fleet scope, the tap temper itself installs from — while its preview named
//! only two unrelated taps and a cask. Homebrew then skips an untrusted tap's
//! formulae *silently*, so `brew bundle cleanup` could no longer compute extras
//! at all, and the next prune would have read every skipped formula as an orphan.
//!
//! The assertion is on the file brew is actually given, because that is the whole
//! mechanism: what temper leaves out of it, brew takes away.

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

    /// A `brew` that reports one declared formula plus one orphan — so prune has
    /// a brew extra and therefore runs `bundle cleanup --force` — and dumps the
    /// Brewfile it is handed for that run, which is what these tests read.
    fn brew(&self) {
        let dump = self.fake_home.path().join("cleanup-brewfile");
        let p = self.bin.path().join("brew");
        fs::write(
            &p,
            format!(
                r#"#!/bin/sh
case "$1 $2" in
  "list --formula") echo jq; echo orphan ;;
  "list --cask")    : ;;
  "tap")            echo vendor/tap ;;
  "trust --json")   echo '{{"taps":["vendor/tap"]}}' ;;
  "bundle cleanup")
      f=""; prev=""
      for a in "$@"; do
          [ "$prev" = "--file" ] && f="$a"
          prev="$a"
      done
      case " $* " in
        *" --force "*)
            # The destructive run: record exactly what brew was told may stay.
            [ -n "$f" ] && cat "$f" > {dump}
            exit 0
            ;;
        *)  echo "Would uninstall formulae:"
            echo "orphan"
            exit 1
            ;;
      esac
      ;;
esac
exit 0
"#,
                dump = dump.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn spec(&self, body: &str) {
        fs::write(self.home.path().join("temper.toml"), body).unwrap();
    }

    /// The Brewfile handed to `brew bundle cleanup --force`.
    fn cleanup_brewfile(&self) -> String {
        fs::read_to_string(self.fake_home.path().join("cleanup-brewfile")).unwrap_or_else(|_| {
            panic!("`brew bundle cleanup --force` never ran — the test proves nothing")
        })
    }

    fn temper(&self) -> Command {
        let mut c = Command::cargo_bin("temper").unwrap();
        c.env("TEMPER_DIR", self.home.path())
            .env("HOME", self.fake_home.path())
            .env("XDG_CONFIG_HOME", self.fake_home.path().join(".config"))
            .env_remove("DCONF_PROFILE")
            .env("TEMPER_STATE_DIR", self.state.path())
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.path().display()));
        c
    }
}

fn os() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    }
}

/// Fleet-scope trust, and a machine whose Brewfile names no tap at all — the
/// exact shape that lost `jakobhviid/tap`.
#[test]
fn fleet_trust_survives_a_prune_that_removes_a_package() {
    let e = Env::new();
    e.brew();
    e.spec(&format!(
        "[brew]\ntrust = [\"vendor/tap\"]\n\n\
         [[machine]]\nname = \"box\"\nos = \"{}\"\npackages = [\"brew \\\"jq\\\"\"]\n",
        os()
    ));

    e.temper().args(["prune", "--yes"]).output().unwrap();

    let bf = e.cleanup_brewfile();
    assert!(
        bf.contains("tap \"vendor/tap\", trusted: true"),
        "the cleanup file must declare the trust temper declares, or brew's \
         Trust.replace! empties the store:\n{bf}"
    );
}

/// Machine-scope trust reaches it too — the scopes union, and the executor must
/// see the union rather than the fleet list alone.
#[test]
fn machine_scope_trust_survives_too() {
    let e = Env::new();
    e.brew();
    e.spec(&format!(
        "[[machine]]\nname = \"box\"\nos = \"{}\"\n\
         brew_trust = [\"vendor/tap\"]\npackages = [\"brew \\\"jq\\\"\"]\n",
        os()
    ));

    e.temper().args(["prune", "--yes"]).output().unwrap();

    let bf = e.cleanup_brewfile();
    assert!(
        bf.contains("tap \"vendor/tap\", trusted: true"),
        "a machine's own brew_trust must reach the cleanup file:\n{bf}"
    );
}

/// The control, and the half that keeps this honest: a spec declaring no trust
/// must not have trust invented for it. Without this the fix could "pass" by
/// writing every tap it can see.
#[test]
fn a_spec_declaring_no_trust_gets_none_written() {
    let e = Env::new();
    e.brew();
    e.spec(&format!(
        "[[machine]]\nname = \"box\"\nos = \"{}\"\npackages = [\"brew \\\"jq\\\"\"]\n",
        os()
    ));

    e.temper().args(["prune", "--yes"]).output().unwrap();

    let bf = e.cleanup_brewfile();
    assert!(
        !bf.contains("trusted:"),
        "nothing declares trust here, so nothing may be written:\n{bf}"
    );
    // …and the declared package is still what the file says may stay.
    assert!(bf.contains("brew \"jq\""), "the declared set survived:\n{bf}");
}
