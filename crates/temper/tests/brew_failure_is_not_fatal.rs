//! One failing package must not cancel every other provider and the config phase.
//!
//! `brew bundle` is the only converge child temper ran with all-or-nothing
//! semantics: a non-zero exit `bail!`ed, and `run_install` calls `converge` with
//! `?`, so a single unavailable cask aborted the run before flatpak, GNOME
//! extensions or rpm-ostree were touched and before one config step applied.
//!
//! brew does not behave that way itself — measured, two bad entries yield
//! "Installing <a> has failed! / Installing <b> has failed! / `brew bundle`
//! failed! 2 Brewfile dependencies failed to install" and exit 1 — so it attempts
//! every entry and the all-or-nothing was temper's alone.
//!
//! Continuing is only an improvement if the run says it fell short. Reporting
//! success with the reason buried in stderr is the quieter half of the same lie,
//! so the summary carries a warning and `--json` carries a `failed` list. Both
//! are asserted here; the first is what makes the second safe.

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

    fn stub(&self, name: &str, body: &str) {
        let p = self.bin.path().join(name);
        fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
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

/// A spec with one brew package (whose converge will fail) and one config step
/// in a later phase. The step's effect is observable, which is the point.
fn spec_with_a_step(e: &Env, marker: &std::path::Path) {
    fs::create_dir_all(e.home.path().join("apps")).unwrap();
    fs::create_dir_all(e.home.path().join("assets")).unwrap();
    fs::write(e.home.path().join("assets/x.conf"), "deployed\n").unwrap();
    fs::write(
        e.home.path().join("apps/a.toml"),
        format!(
            "[[step]]\ncopy = \"assets/x.conf\"\nto = \"{}\"\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::write(
        e.home.path().join("temper.toml"),
        format!(
            "[[machine]]\nname = \"m\"\nos = \"{}\"\napps = [\"a\"]\npackages = [\"brew \\\"nope\\\"\"]\n",
            os()
        ),
    )
    .unwrap();
}

/// The config phase runs even though the package phase failed.
#[test]
fn a_failing_brew_still_lets_the_config_phase_run() {
    let e = Env::new();
    // `brew bundle` exits non-zero, like a Brewfile with an unavailable cask.
    // Every other brew subcommand succeeds, so probing still works.
    e.stub("brew", "if [ \"$1\" = bundle ]; then echo 'Installing nope has failed!'; exit 1; fi\nexit 0");
    let marker = e.fake_home.path().join("x.conf");
    spec_with_a_step(&e, &marker);

    let out = e.temper().args(["install", "--yes"]).output().unwrap();
    let said = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);

    assert!(
        marker.exists(),
        "the config step must still have applied — one failing package is not a \
         reason to skip every other phase.\n{said}"
    );
    // …and the run must not pretend it succeeded.
    assert!(
        said.contains("could not finish"),
        "a run that fell short of the spec has to say so.\n{said}"
    );
    assert!(
        said.contains("drift"),
        "and point at the verb that names what is missing, since temper does not \
         parse which entries brew lost.\n{said}"
    );
}

/// `--json` carries the same fact, as data.
///
/// Under `--json` the warning line is not printed at all (one document on
/// stdout, Principle #6b), so the machine-readable channel is the only way a
/// caller can tell a short converge from a complete one.
#[test]
fn json_reports_which_provider_could_not_finish() {
    let e = Env::new();
    e.stub("brew", "if [ \"$1\" = bundle ]; then exit 1; fi\nexit 0");
    let marker = e.fake_home.path().join("x.conf");
    spec_with_a_step(&e, &marker);

    let out = e
        .temper()
        .args(["install", "--yes", "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|err| panic!("stdout was not one JSON document ({err}): {}", String::from_utf8_lossy(&out.stdout)));
    let failed = v["failed"].as_array().cloned().unwrap_or_default();
    assert!(
        failed.iter().any(|f| f == "brew"),
        "the failing provider must be named in the document: {v}"
    );
}

/// A converge where everything works reports no failure and stays quiet.
///
/// The mirror that keeps the warning worth reading: a field that is always
/// populated is a field nobody looks at.
#[test]
fn a_clean_converge_reports_no_failure() {
    let e = Env::new();
    e.stub("brew", "exit 0");
    let marker = e.fake_home.path().join("x.conf");
    spec_with_a_step(&e, &marker);

    let out = e
        .temper()
        .args(["install", "--yes", "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        v["failed"].as_array().is_none_or(|a| a.is_empty()),
        "nothing failed, so nothing may be reported as failed: {v}"
    );
}
