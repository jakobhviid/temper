//! A package manager that is present and **fails** must not read as "nothing is
//! installed".
//!
//! `have()` answers "is the tool here", never "did it work". A failing
//! `brew list` / `mas list` / `code --list-extensions` exits non-zero with empty
//! stdout, and as a bare `Vec` that is indistinguishable from a machine with
//! nothing installed — at which point every declared package is "declared but
//! absent", which is what `reconcile --current-state-wins` drops from the spec.
//! On a folder with `auto_commit`/`auto_push` on, that emptied spec is pushed to
//! the whole fleet.
//!
//! `code` is the tool faked here because it is the one that fails for ordinary
//! reasons on the platform this suite runs on (over ssh, or when `code` is a
//! flatpak wrapper). `mas` fails the same way on a Mac that is not signed into
//! the App Store; the seam is shared, so proving one proves the shape.

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
    /// A folder declaring two VS Code extensions — one via the machine's loose
    /// `packages` list, one via its Brewfile — so both drop paths are covered.
    fn new() -> Env {
        let e = Env {
            home: TempDir::new().unwrap(),
            fake_home: TempDir::new().unwrap(),
            state: TempDir::new().unwrap(),
            bin: TempDir::new().unwrap(),
        };
        fs::write(
            e.home.path().join("temper.toml"),
            format!(
                "[[machine]]\nname = \"t\"\nos = \"{}\"\n\
                 packages = [\"vscode \\\"loose.ext\\\"\"]\n\
                 brewfile = \"brewfiles/t\"\n",
                os()
            ),
        )
        .unwrap();
        fs::create_dir_all(e.home.path().join("brewfiles")).unwrap();
        fs::write(
            e.home.path().join("brewfiles/t"),
            "vscode \"bundled.ext\"\n",
        )
        .unwrap();
        e
    }

    /// A `code` on PATH that exists and fails — the case `have()` cannot see.
    fn failing_code(&self) {
        let bin = self.bin.path().join("code");
        fs::write(&bin, "#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// Replace the manifest — for the tests below, which are about brew rather
    /// than the VS Code folder `new()` builds.
    fn spec(&self, body: &str) {
        fs::write(self.home.path().join("temper.toml"), body).unwrap();
    }

    fn brew(&self, body: &str) {
        let bin = self.bin.path().join("brew");
        fs::write(&bin, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// Installed and failing — the state `have()` cannot see, and the one that
    /// makes "no extras" ambiguous.
    fn broken_brew(&self) {
        self.brew("echo 'Error: Your Homebrew is broken.' >&2\nexit 1");
    }

    /// Installed and answering: one formula, which is also the declared one, so
    /// there is genuinely nothing to prune.
    fn working_brew(&self) {
        self.brew(
            "case \"$1 $2\" in\n\
             \x20 \"list --formula\") echo jq ;;\n\
             \x20 \"trust --json\") echo '{\"taps\":[]}' ;;\n\
             \x20 \"bundle cleanup\") exit 0 ;;\n\
             esac\nexit 0",
        );
    }

    fn temper(&self) -> Command {
        let mut c = Command::cargo_bin("temper").unwrap();
        c.env("TEMPER_DIR", self.home.path())
            .env("HOME", self.fake_home.path())
            .env("XDG_CONFIG_HOME", self.fake_home.path().join(".config"))
            .env_remove("DCONF_PROFILE")
            .env("TEMPER_STATE_DIR", self.state.path())
            // A deliberately narrow PATH: the fake `code`, plus the system
            // basics. It must NOT inherit the developer's PATH, or the real
            // brew on this host contributes its own extras and the assertions
            // below measure the machine instead of the code under test.
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.path().display()));
        c
    }
}

#[test]
fn a_failing_probe_is_reported_and_never_becomes_a_drop() {
    let e = Env::new();
    e.failing_code();

    let out = e.temper().args(["drift", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let items = v["items"].as_array().unwrap();
    let kinds: Vec<&str> = items.iter().map(|i| i["kind"].as_str().unwrap()).collect();

    // Reported, not passed over in silence (Principle #6).
    assert!(
        kinds.contains(&"package-unavailable"),
        "a failing manager must be reported, got {kinds:?}"
    );
    // …and status-only: the machine may well be fine, we simply could not look.
    assert_eq!(
        v["out_of_sync"], 0,
        "an unreadable manager is degraded, not drift: {v}"
    );
    // The declared extensions must NOT be reported missing — that claim needs
    // an answer from the tool, and there wasn't one.
    assert!(
        !items
            .iter()
            .any(|i| i["kind"] == "vscode-package" && i["status"] == "missing"),
        "a failed probe was read as 'nothing installed': {v}"
    );

    // The whole point: --csw must find nothing to drop.
    let out = e
        .temper()
        .args(["reconcile", "--csw", "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    for list in ["drops", "package_drops"] {
        let n = v[list].as_array().map(|a| a.len()).unwrap_or(0);
        assert_eq!(
            n, 0,
            "--csw would delete {n} declaration(s) from `{list}` on the strength \
             of a probe that failed: {v}"
        );
    }

    // …and then actually run the write. The preview is not the dangerous half:
    // `--csw --yes` is what edits the Brewfile and the machine block, and what
    // `after_repo_change` then commits and (with auto_push) sends to the fleet.
    // Asserting on a preview would leave the writing path untested, which is the
    // shape of "covered the safe variant of a risky operation".
    let before_bf = fs::read_to_string(e.home.path().join("brewfiles/t")).unwrap();
    let before_tt = fs::read_to_string(e.home.path().join("temper.toml")).unwrap();
    e.temper()
        .args(["reconcile", "--csw", "--yes", "--json"])
        .output()
        .unwrap();
    assert_eq!(
        fs::read_to_string(e.home.path().join("brewfiles/t")).unwrap(),
        before_bf,
        "a failed probe emptied the Brewfile — this is the path that reaches the \
         whole fleet through auto_commit/auto_push"
    );
    assert_eq!(
        fs::read_to_string(e.home.path().join("temper.toml")).unwrap(),
        before_tt,
        "…and the machine block must be untouched too"
    );
}

/// The control: when the tool answers, the same folder behaves normally. Without
/// this the test above would pass on a build that never probes vscode at all.
#[test]
fn a_working_probe_still_reports_missing_packages() {
    let e = Env::new();
    let bin = e.bin.path().join("code");
    // Answers successfully, listing neither declared extension.
    fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let out = e.temper().args(["drift", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let items = v["items"].as_array().unwrap();
    let missing: Vec<&str> = items
        .iter()
        .filter(|i| i["kind"] == "vscode-package" && i["status"] == "missing")
        .map(|i| i["target"].as_str().unwrap())
        .collect();
    assert_eq!(
        missing.len(),
        2,
        "an answering manager reporting none means both are genuinely missing: {v}"
    );
}

/// `prune` must distinguish "nothing to remove" from "could not look".
///
/// Principle #12 — report `unavailable`, never absent — was enforced for `drift`
/// and not for the verb that deletes. On a machine whose brew is installed and
/// failing, `prune --json` emitted `"extras": []`, which is byte-identical to a
/// converged machine. For a removal verb those are opposite instructions.
#[test]
fn prune_says_when_it_could_not_ask() {
    let e = Env::new();
    e.broken_brew();
    e.spec(&format!(
        "[[machine]]\nname = \"box\"\nos = \"{}\"\npackages = [\"brew \\\"jq\\\"\"]\n",
        os()
    ));

    let out = e.temper().args(["prune", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|err| {
        panic!(
            "prune did not emit one document ({err}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });

    assert_eq!(
        v["extras"].as_array().map(|a| a.len()).unwrap_or(0),
        0,
        "a manager that cannot be asked must not yield extras to remove: {v}"
    );
    let un: Vec<&str> = v["unavailable"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    assert!(
        un.contains(&"brew"),
        "prune reported an empty plan without saying brew could not be asked — \
         indistinguishable from a converged machine: {v}"
    );

    // …and the human reading the terminal is told too, not only the parser.
    let human = e.temper().args(["prune"]).output().unwrap();
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(
        text.contains("could not ask brew"),
        "the terminal output claimed nothing to remove and left out why: {text}"
    );
}

/// The control: a manager that answers normally is never listed as unavailable,
/// or the field means nothing.
#[test]
fn a_working_manager_is_not_reported_unavailable() {
    let e = Env::new();
    e.working_brew();
    e.spec(&format!(
        "[[machine]]\nname = \"box\"\nos = \"{}\"\npackages = [\"brew \\\"jq\\\"\"]\n",
        os()
    ));

    let out = e.temper().args(["prune", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let un = v["unavailable"].as_array().map(|a| a.len()).unwrap_or(0);
    assert_eq!(un, 0, "a healthy brew was reported unavailable: {v}");
}
