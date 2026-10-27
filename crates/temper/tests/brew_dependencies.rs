//! brew extras are what someone asked for — never the dependencies that came
//! with it.
//!
//! `brew bundle cleanup` lists every formula the Brewfile does not account for,
//! dependencies included: with `jq` installed by hand it lists `oniguruma`
//! beside it. Read as extras, `reconcile` offered the dependency, `init` seeded
//! it and `prune` listed it as though it had been asked for. Homebrew records
//! the difference (`installed_on_request`), and the fake below carries it.
//!
//! The fake machine: `keeper` (declared), `jq` (installed by hand), `oniguruma`
//! (`jq`'s dependency) and `tree` (unneeded already — an orphan `brew
//! autoremove` would take, which is not this prune's to remove).

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

struct Env {
    home: TempDir,
    fake_home: TempDir,
    state: TempDir,
    bin: TempDir,
}

impl Env {
    /// `orphans_readable = false` makes `brew autoremove --dry-run` fail, which
    /// is the fallback case.
    fn new(orphans_readable: bool) -> Env {
        let e = Env {
            home: TempDir::new().unwrap(),
            fake_home: TempDir::new().unwrap(),
            state: TempDir::new().unwrap(),
            bin: TempDir::new().unwrap(),
        };
        let log = e.fake_home.path().join("brew.log");
        let seen = e.fake_home.path().join("cleanup-brewfile");
        let autoremove = if orphans_readable {
            "echo '==> Would autoremove 1 unneeded formula:'; echo tree; exit 0"
        } else {
            "exit 1"
        };
        let bin = e.bin.path().join("brew");
        fs::write(
            &bin,
            format!(
                r#"#!/bin/sh
case "$1 $2" in
  "list --formula") printf 'keeper\njq\noniguruma\ntree\n' ;;
  "list --cask")    : ;;
  "trust --json")   echo '{{"taps":[]}}' ;;
  "autoremove --dry-run") {autoremove} ;;
  "info --json=v2")
      case " $* " in
        *" --installed "*)
          echo '{{"formulae":[
            {{"name":"keeper","full_name":"keeper","installed":[{{"installed_on_request":true}}]}},
            {{"name":"jq","full_name":"jq","installed":[{{"installed_on_request":true}}]}},
            {{"name":"oniguruma","full_name":"oniguruma","installed":[{{"installed_on_request":false}}]}},
            {{"name":"tree","full_name":"tree","installed":[{{"installed_on_request":false}}]}}
          ],"casks":[]}}' ;;
      esac ;;
  "bundle cleanup")
      case " $* " in
        *" --force "*)
          echo "cleanup NO_AUTOREMOVE=${{HOMEBREW_NO_AUTOREMOVE:-unset}} $*" >> {log}
          for a in "$@"; do [ -f "$a" ] && cp "$a" {seen}; done ;;
        *) printf 'Would uninstall formulae:\njq\noniguruma\ntree\n'; exit 1 ;;
      esac ;;
  *) : ;;
esac
exit 0
"#,
                log = log.display(),
                seen = seen.display(),
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::create_dir_all(e.home.path().join("brewfiles")).unwrap();
        fs::write(e.home.path().join("brewfiles/m"), "brew \"keeper\"\n").unwrap();
        fs::write(
            e.home.path().join("temper.toml"),
            format!("[[machine]]\nname = \"m\"\nos = \"{}\"\nbrewfile = \"brewfiles/m\"\n", os()),
        )
        .unwrap();
        e
    }

    fn temper(&self) -> Command {
        let mut c = Command::cargo_bin("temper").unwrap();
        c.env("TEMPER_DIR", self.home.path())
            .env("HOME", self.fake_home.path())
            .env("TEMPER_STATE_DIR", self.state.path())
            .env_remove("HOMEBREW_NO_AUTOREMOVE")
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.path().display()));
        c
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let out = self.temper().arg("--json").args(args).output().unwrap();
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!("{args:?} --json: {e}\n{}", String::from_utf8_lossy(&out.stdout))
        })
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.fake_home.path().join(name)).unwrap_or_default()
    }
}

/// drift and reconcile name the formula that was asked for, not what it pulled
/// in, and not the orphan.
#[test]
fn drift_and_reconcile_offer_what_was_asked_for_not_its_dependencies() {
    let e = Env::new(true);
    let drift = e.json(&["drift"]);
    let extras: Vec<String> = drift["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["kind"] == "brew-package-extra")
        .map(|f| f["target"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(extras, vec!["brew jq"], "{drift}");

    let plan = e.json(&["reconcile"]);
    assert_eq!(plan["adds"], serde_json::json!(["brew \"jq\""]), "{plan}");
}

/// prune removes the extra and the dependency only it needed — previewed — and
/// keeps the orphan that was unneeded already, with Homebrew's own whole-system
/// autoremove switched off for the run.
#[test]
fn prune_takes_the_dependencies_only_its_extras_needed() {
    let e = Env::new(true);
    let plan = e.json(&["prune", "--dry-run"]);
    let extras: Vec<&str> = plan["extras"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert_eq!(extras, vec!["jq"], "{plan}");
    assert_eq!(plan["dependencies"], serde_json::json!(["brew oniguruma"]), "{plan}");
    assert_eq!(plan["autoremove"], serde_json::json!([]), "{plan}");

    e.temper().args(["prune", "--yes"]).assert().success();
    let brewfile = e.read("cleanup-brewfile");
    assert!(brewfile.contains("brew \"tree\""), "the orphan is kept:\n{brewfile}");
    assert!(
        !brewfile.contains("oniguruma") && !brewfile.contains("\"jq\""),
        "the extra and its dependency go:\n{brewfile}"
    );
    assert!(e.read("brew.log").contains("NO_AUTOREMOVE=1"), "{}", e.read("brew.log"));
}

/// Where brew cannot list what it holds as unneeded, its autoremove runs
/// instead — said in the preview — rather than leaving the dependencies behind.
#[test]
fn prune_falls_back_to_brews_autoremove_and_says_so() {
    let e = Env::new(false);
    let plan = e.json(&["prune", "--dry-run"]);
    assert_eq!(plan["autoremove"], serde_json::json!(["brew"]), "{plan}");
    assert_eq!(plan["dependencies"], serde_json::json!([]), "{plan}");
    let out = e.temper().args(["prune", "--dry-run"]).output().unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("run brew's autoremove"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    e.temper().args(["prune", "--yes"]).assert().success();
    let brewfile = e.read("cleanup-brewfile");
    assert!(
        !brewfile.contains("tree") && !brewfile.contains("oniguruma"),
        "the fallback takes every unneeded formula:\n{brewfile}"
    );
    assert!(e.read("brew.log").contains("NO_AUTOREMOVE=unset"), "{}", e.read("brew.log"));
}
