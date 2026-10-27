//! brew extras are what someone asked for — never the dependencies that came
//! with it — and a prune leaves Homebrew's record of who asked for what true.
//!
//! `brew bundle cleanup` lists every formula the Brewfile does not account for,
//! dependencies included: with `jq` installed by hand it lists `oniguruma`
//! beside it. Read as extras, `reconcile` offered the dependency, `init` seeded
//! it and `prune` listed it as though it had been asked for. Homebrew records
//! the difference (`installed_on_request`), and the fake below carries it — in a
//! state file, because a prune changes it: `bundle cleanup --force` marks every
//! formula its file names as asked for, as Homebrew does, and `tab` clears it.
//!
//! The fake machine: `keeper` (declared); `jq` (installed by hand); `oniguruma`
//! (`jq`'s dependency); `tree` (unneeded already — an orphan `brew autoremove`
//! would take, which is not this prune's to remove). Some tests add `lib`:
//! asked for, declared nowhere, and needed by `keeper`.

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

const MACHINE: &str = "keeper true\njq true\noniguruma false\ntree false\n";
const CLEANUP: &str = "jq\noniguruma\ntree\n";

struct Env {
    home: TempDir,
    fake_home: TempDir,
    state: TempDir,
    bin: TempDir,
}

impl Env {
    fn new(orphans_readable: bool) -> Env {
        Env::with(orphans_readable, MACHINE, CLEANUP, "")
    }

    /// `formulae`: one `name asked-for` line per installed formula.
    /// `cleanup`: what a dry `brew bundle cleanup` lists, one per line.
    /// `orphans_readable = false` makes `brew autoremove --dry-run` fail, which
    /// is the fallback case.
    fn with(orphans_readable: bool, formulae: &str, cleanup: &str, machine_toml: &str) -> Env {
        let e = Env {
            home: TempDir::new().unwrap(),
            fake_home: TempDir::new().unwrap(),
            state: TempDir::new().unwrap(),
            bin: TempDir::new().unwrap(),
        };
        let log = e.fake_home.path().join("brew.log");
        let seen = e.fake_home.path().join("cleanup-brewfile");
        let formula_state = e.fake_home.path().join("formulae");
        fs::write(&formula_state, formulae).unwrap();
        let autoremove = if orphans_readable {
            "echo '==> Would autoremove 1 unneeded formula:'; echo tree; exit 0"
        } else {
            "exit 1"
        };
        let cleanup = if cleanup.is_empty() {
            "exit 0".to_string()
        } else {
            format!("printf 'Would uninstall formulae:\\n{}'; exit 1", cleanup.replace('\n', "\\n"))
        };
        let bin = e.bin.path().join("brew");
        fs::write(
            &bin,
            format!(
                r#"#!/bin/sh
state={state}
needs() {{ case "$1" in jq) echo oniguruma ;; keeper) echo lib ;; esac; }}
mark() {{ sed "s/^$1 .*/$1 $2/" "$state" > "$state.t" && mv "$state.t" "$state"; }}
case "$1 $2" in
  "list --formula") cut -d' ' -f1 "$state" ;;
  "list --cask")    : ;;
  "trust --json")   echo '{{"taps":[]}}' ;;
  "autoremove --dry-run") {autoremove} ;;
  "info --json=v2")
      case " $* " in
        *" --installed "*)
          printf '{{"formulae":['; sep=
          while read -r n asked; do
            d=$(needs "$n"); rd=
            [ -n "$d" ] && rd="{{\"full_name\":\"$d\"}}"
            printf '%s{{"name":"%s","full_name":"%s","installed":[{{"installed_on_request":%s,"runtime_dependencies":[%s]}}]}}' \
              "$sep" "$n" "$n" "$asked" "$rd"
            sep=,
          done < "$state"
          printf '],"casks":[]}}\n' ;;
      esac ;;
  "tab --no-installed-on-request")
      echo "tab $*" >> {log}
      for a in "$@"; do case "$a" in tab|--*) ;; *) mark "$a" false ;; esac; done ;;
  "bundle cleanup")
      case " $* " in
        *" --force "*)
          echo "cleanup NO_AUTOREMOVE=${{HOMEBREW_NO_AUTOREMOVE:-unset}} $*" >> {log}
          for a in "$@"; do
            [ -f "$a" ] || continue
            cp "$a" {seen}
            for n in $(sed -n 's/^brew "\(.*\)"$/\1/p' "$a"); do mark "$n" true; done
          done ;;
        *) {cleanup} ;;
      esac ;;
  *) : ;;
esac
exit 0
"#,
                state = formula_state.display(),
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
            format!(
                "[[machine]]\nname = \"m\"\nos = \"{}\"\nbrewfile = \"brewfiles/m\"\n{machine_toml}",
                os()
            ),
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

    /// Whether Homebrew's record holds `formula` as asked for.
    fn asked(&self, formula: &str) -> bool {
        self.read("formulae")
            .lines()
            .any(|l| l == format!("{formula} true"))
    }

    fn brew_extras(&self) -> Vec<serde_json::Value> {
        self.json(&["drift"])["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["kind"] == "brew-package-extra")
            .cloned()
            .collect()
    }
}

/// drift and reconcile name the formula that was asked for, not what it pulled
/// in, and not the orphan.
#[test]
fn drift_and_reconcile_offer_what_was_asked_for_not_its_dependencies() {
    let e = Env::new(true);
    let extras: Vec<_> = e.brew_extras().iter().map(|f| f["target"].clone()).collect();
    assert_eq!(extras, vec!["brew jq"]);

    let plan = e.json(&["reconcile"]);
    assert_eq!(plan["adds"], serde_json::json!(["brew \"jq\""]), "{plan}");
}

/// prune removes the extra and the dependency only it needed — previewed — and
/// keeps the orphan that was unneeded already, with Homebrew's own whole-system
/// autoremove switched off for the run. The orphan is still nobody's request
/// afterwards: cleanup marked it asked for, and prune put that back, or the next
/// drift would report it and the next prune remove it.
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
    assert_eq!(plan["demote"], serde_json::json!([]), "{plan}");

    e.temper().args(["prune", "--yes"]).assert().success();
    let brewfile = e.read("cleanup-brewfile");
    assert!(brewfile.contains("brew \"tree\""), "the orphan is kept:\n{brewfile}");
    assert!(
        !brewfile.contains("oniguruma") && !brewfile.contains("\"jq\""),
        "the extra and its dependency go:\n{brewfile}"
    );
    assert!(e.read("brew.log").contains("NO_AUTOREMOVE=1"), "{}", e.read("brew.log"));
    assert!(!e.asked("tree"), "the kept orphan is a dependency again:\n{}", e.read("formulae"));
    assert!(e.asked("keeper"), "a declared formula stays asked for:\n{}", e.read("formulae"));
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

/// A formula asked for and declared nowhere is an extra even when a declared
/// formula needs it — the state a dependency line deleted from a Brewfile
/// leaves, since `brew bundle install` marked it asked for. Cleanup keeps it,
/// so only Homebrew's record shows it. drift names it and what needs it;
/// reconcile declares it; prune keeps it installed and marks it as a dependency,
/// without running a cleanup that has nothing to remove.
#[test]
fn an_undeclared_formula_something_needs_is_kept_as_a_dependency() {
    let e = Env::with(true, "keeper true\nlib true\n", "", "");
    let extras = e.brew_extras();
    assert_eq!(extras.len(), 1, "{extras:?}");
    assert_eq!(extras[0]["target"], "brew lib");
    assert!(
        extras[0]["detail"].as_str().unwrap().contains("keeper needs it"),
        "{extras:?}"
    );
    assert_eq!(e.json(&["reconcile"])["adds"], serde_json::json!(["brew \"lib\""]));

    let plan = e.json(&["prune", "--dry-run"]);
    assert_eq!(plan["extras"], serde_json::json!([]), "{plan}");
    assert_eq!(plan["demote"], serde_json::json!(["brew lib"]), "{plan}");

    let out = e.temper().args(["prune", "--yes"]).output().unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("0 item(s) removed, 1 kept and marked as dependencies"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let log = e.read("brew.log");
    assert!(log.contains("tab --no-installed-on-request --formula lib"), "{log}");
    assert!(!log.contains("cleanup"), "nothing to remove, so no cleanup:\n{log}");
    assert!(!e.asked("lib") && e.read("formulae").contains("lib false"));
    assert!(e.brew_extras().is_empty(), "drift agrees once it is a dependency");
}

/// A retired formula that is not installed is nothing to remove: drift does not
/// report it and prune does not plan it. Installed, both name it.
#[test]
fn a_retired_formula_is_pruned_only_when_it_is_installed() {
    let retire = "retire_packages = ['brew \"nvtop\"']\n";
    let e = Env::with(true, MACHINE, CLEANUP, retire);
    let names = |plan: &serde_json::Value| -> Vec<String> {
        plan["extras"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["name"].as_str().unwrap().to_string())
            .collect()
    };
    let retired = |e: &Env| {
        e.json(&["drift"])["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["kind"] == "retired-package")
            .count()
    };
    assert_eq!(names(&e.json(&["prune", "--dry-run"])), vec!["jq"]);
    assert_eq!(retired(&e), 0);

    let e = Env::with(true, &format!("{MACHINE}nvtop true\n"), CLEANUP, retire);
    assert_eq!(names(&e.json(&["prune", "--dry-run"])), vec!["jq", "nvtop"]);
    assert_eq!(retired(&e), 1);
}
