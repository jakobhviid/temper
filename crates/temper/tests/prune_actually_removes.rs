//! `prune`'s destructive executors must actually run, with the right argv.
//!
//! `PrunePlan::LISTS` has seven entries. Before this file, three of them —
//! GNOME extensions, layered rpms, flatpak remotes — were never executed by any
//! test, and tap-untrust only negatively (a test asserting `brew untrust` did
//! **not** run). So the plan half was covered and the acting half was not, for
//! exactly the kinds whose *absence* caused the two defects AGENTS.md names:
//! gext extras were reported a release before `prune` could remove them, and a
//! removed GNOME extension came back on every converge for two releases.
//!
//! A plan is a claim about what will happen. These tests make the claim
//! observable: each tool is faked, records the argv it was called with, and the
//! assertions are on what the child was actually told to do.

use std::fs;
use std::path::Path;

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

    /// Write an executable stub that appends its argv to `<fake_home>/<name>.log`
    /// and otherwise answers `body`.
    fn stub(&self, name: &str, body: &str) {
        let log = self.fake_home.path().join(format!("{name}.log"));
        let p = self.bin.path().join(name);
        fs::write(
            &p,
            format!("#!/bin/sh\necho \"$@\" >> {}\n{body}\nexit 0\n", log.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn log(&self, name: &str) -> String {
        fs::read_to_string(self.fake_home.path().join(format!("{name}.log"))).unwrap_or_default()
    }

    fn spec(&self, body: &str) {
        fs::write(self.home.path().join("temper.toml"), body).unwrap();
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

/// A GNOME extension installed but declared nowhere is an extra, and `prune`
/// must actually call `gext uninstall` on it.
#[test]
fn prune_uninstalls_an_undeclared_gnome_extension() {
    let e = Env::new();
    // `gnome-extensions list` enumerates; `gext` is the only thing that can
    // uninstall. Both are needed, and they are different capabilities.
    e.stub("gnome-extensions", "echo keep@x; echo stray@x");
    e.stub("gext", "");
    e.spec(&format!(
        "[[machine]]\nname = \"t\"\nos = \"{}\"\n\
         gnome_extensions = [\"keep@x\"]\n",
        os()
    ));

    e.temper().args(["prune", "--yes"]).output().unwrap();

    let called = e.log("gext");
    assert!(
        called.contains("uninstall") && called.contains("stray@x"),
        "prune must uninstall the undeclared extension; gext was called with: {called:?}"
    );
    assert!(
        !called.contains("keep@x"),
        "the declared extension must survive: {called:?}"
    );
}

/// A layered rpm the spec does not declare is an extra, and un-layering it is a
/// real command with real flags — `--idempotent -y`, because a converge must not
/// stop to ask and must not fail on a package already gone.
#[test]
fn prune_unlayers_an_undeclared_rpm() {
    let e = Env::new();
    e.stub(
        "rpm-ostree",
        "case \"$1 $2\" in \"status --json\") \
         echo '{\"deployments\":[{\"booted\":true,\"requested-packages\":[\"keep\",\"stray\"]}]}' ;; esac",
    );
    e.spec(&format!(
        "[[machine]]\nname = \"t\"\nos = \"{}\"\nrpm_ostree = [\"keep\"]\n",
        os()
    ));

    e.temper().args(["prune", "--yes"]).output().unwrap();

    let called = e.log("rpm-ostree");
    let uninstall = called
        .lines()
        .find(|l| l.starts_with("uninstall"))
        .unwrap_or_else(|| panic!("rpm-ostree uninstall never ran; calls were:\n{called}"));
    assert!(
        uninstall.contains("stray"),
        "the undeclared rpm must be un-layered: {uninstall:?}"
    );
    assert!(
        !uninstall.contains("keep"),
        "the declared rpm must survive: {uninstall:?}"
    );
    assert!(
        uninstall.contains("--idempotent") && uninstall.contains("-y"),
        "un-layering must not stop to ask, nor fail on an already-absent package: \
         {uninstall:?}"
    );
}

/// A flatpak remote the spec does not declare is an extra. The scope flag is the
/// point: temper models the **user** installation, and deleting a system remote
/// is not the same operation.
#[test]
fn prune_deletes_an_undeclared_flatpak_remote_at_user_scope() {
    let e = Env::new();
    e.stub(
        "flatpak",
        "case \"$1\" in \
         remotes) echo 'keep\thttps://keep.example/repo'; echo 'stray\thttps://stray.example/repo' ;; \
         esac",
    );
    // `flatpak_remotes` is a list of "<name> <url>" strings, not a table array.
    e.spec(&format!(
        "[[machine]]\nname = \"t\"\nos = \"{}\"\n\
         flatpak_remotes = [\"keep https://keep.example/repo\"]\n",
        os()
    ));

    e.temper().args(["prune", "--yes"]).output().unwrap();

    let called = e.log("flatpak");
    let del = called
        .lines()
        .find(|l| l.starts_with("remote-delete"))
        .unwrap_or_else(|| panic!("flatpak remote-delete never ran; calls were:\n{called}"));
    assert!(
        del.contains("stray") && !del.contains("keep"),
        "only the undeclared remote may be deleted: {del:?}"
    );
    assert!(
        del.contains("--user"),
        "temper models the user installation — an unscoped delete is a different \
         operation on a different set of remotes: {del:?}"
    );
}

/// The positive half of tap-untrust. A test asserting `brew untrust` did *not*
/// run cannot tell a correct implementation from one that passes the wrong argv,
/// or none at all.
#[test]
fn prune_untrusts_a_tap_the_spec_stopped_declaring() {
    let e = Env::new();
    e.stub(
        "brew",
        "case \"$1 $2\" in \
         \"trust --json\") echo '{\"taps\":[\"a/keep\",\"b/stray\"]}' ;; esac",
    );
    e.spec(&format!(
        "[brew]\ntrust = [\"a/keep\"]\n\n\
         [[machine]]\nname = \"t\"\nos = \"{}\"\npackages = [\"brew \\\"jq\\\"\"]\n",
        os()
    ));

    e.temper().args(["prune", "--yes"]).output().unwrap();

    let called = e.log("brew");
    let untrust = called
        .lines()
        .find(|l| l.starts_with("untrust"))
        .unwrap_or_else(|| panic!("brew untrust never ran; calls were:\n{called}"));
    assert!(
        untrust.contains("b/stray") && !untrust.contains("a/keep"),
        "only the undeclared tap may be untrusted: {untrust:?}"
    );
}

/// The control that keeps the three above honest: with nothing undeclared, none
/// of these tools may be asked to remove anything at all.
#[test]
fn a_converged_machine_removes_nothing() {
    let e = Env::new();
    e.stub("gnome-extensions", "echo keep@x");
    e.stub("gext", "");
    e.stub(
        "rpm-ostree",
        "case \"$1 $2\" in \"status --json\") \
         echo '{\"deployments\":[{\"booted\":true,\"requested-packages\":[\"keep\"]}]}' ;; esac",
    );
    e.stub(
        "flatpak",
        "case \"$1\" in remotes) echo 'keep\thttps://keep.example/repo' ;; esac",
    );
    e.spec(&format!(
        "[[machine]]\nname = \"t\"\nos = \"{}\"\n\
         gnome_extensions = [\"keep@x\"]\nrpm_ostree = [\"keep\"]\n\
         flatpak_remotes = [\"keep https://keep.example/repo\"]\n",
        os()
    ));

    e.temper().args(["prune", "--yes"]).output().unwrap();

    assert!(
        !e.log("gext").contains("uninstall"),
        "nothing was undeclared, yet gext was told to uninstall: {}",
        e.log("gext")
    );
    for (tool, verb) in [("rpm-ostree", "uninstall"), ("flatpak", "remote-delete")] {
        assert!(
            !e.log(tool).lines().any(|l| l.starts_with(verb)),
            "nothing was undeclared, yet `{tool} {verb}` ran: {}",
            e.log(tool)
        );
    }
}

/// Guard: these tests are only meaningful if the stub is what temper reached.
#[test]
fn the_stubs_are_on_the_path_temper_uses() {
    let e = Env::new();
    e.stub("gnome-extensions", "echo probe@x");
    e.stub("gext", "");
    e.spec(&format!(
        "[[machine]]\nname = \"t\"\nos = \"{}\"\ngnome_extensions = [\"probe@x\"]\n",
        os()
    ));
    e.temper().args(["drift", "--json"]).output().unwrap();
    assert!(
        !e.log("gnome-extensions").is_empty(),
        "temper never called the stubbed `gnome-extensions`, so nothing in this \
         file is observing temper"
    );
}

/// Not a test of temper: a sanity check that `stub` produces something runnable,
/// so a broken helper reads as a broken helper and not as a passing suite.
#[test]
fn the_stub_helper_writes_a_working_executable() {
    let e = Env::new();
    e.stub("thing", "echo hello");
    let out = std::process::Command::new(e.bin.path().join("thing"))
        .arg("an-argument")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");
    assert!(e.log("thing").contains("an-argument"), "argv was not recorded");
    assert!(Path::new(&e.bin.path().join("thing")).exists());
}
