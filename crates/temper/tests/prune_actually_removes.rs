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

/// Not a test of temper: a sanity check that `stub` writes something a shell
/// would run, so a broken helper reads as a broken helper and not as a passing
/// suite.
///
/// It does **not** execute the stub. Doing so raced the rest of the suite:
/// writing a file and exec'ing it immediately hits `ETXTBSY` whenever another
/// test forks in between and inherits the still-open write descriptor. That the
/// stubs really do run is settled by the four tests above, which fail with an
/// empty log if they do not.
#[test]
fn the_stub_helper_writes_something_runnable() {
    let e = Env::new();
    e.stub("thing", "echo hello");

    let p = e.bin.path().join("thing");
    let body = fs::read_to_string(&p).unwrap();
    assert!(body.starts_with("#!/bin/sh"), "no shebang: {body:?}");
    assert!(body.contains("echo hello"), "body missing: {body:?}");
    assert!(
        body.contains("thing.log"),
        "the stub must record its argv, or an empty log proves nothing: {body:?}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&p).unwrap().permissions().mode();
        assert!(mode & 0o111 != 0, "not executable: {mode:o}");
    }
    assert!(Path::new(&p).exists());
}

/// A converged machine is warned about nothing.
///
/// `install --packages-only` names what `undo` could not take back — and it keyed
/// off "does the spec declare any", so a machine already holding all three of its
/// declared taps was told its tap-trust was unrevertible while the run was about
/// to touch nothing. The step path is careful about exactly this (an in-sync
/// `sysfile` is not listed) and this path was not.
///
/// This replaces a source scrape that asserted `packages_only_unrevertible`'s
/// body *contained the string* `trusted_taps`. That passes if the name appears in
/// a comment, and fails if the code is refactored to call a correctly-behaved
/// wrapper — it was a proxy for a claim that is directly observable.
#[test]
fn a_converged_machine_is_warned_about_nothing_it_will_not_do() {
    let e = Env::new();
    // Everything the spec declares is already true of this machine: the tap is
    // trusted, the formula installed, the extension enabled.
    e.stub(
        "brew",
        "case \"$1 $2\" in \
         \"trust --json\") echo '{\"taps\":[\"a/keep\"]}' ;; \
         \"list --formula\") echo jq ;; \
         esac",
    );
    e.stub("gnome-extensions", "echo keep@x");
    e.stub("gext", "");
    e.spec(&format!(
        "[brew]\ntrust = [\"a/keep\"]\n\n\
         [[machine]]\nname = \"t\"\nos = \"{}\"\n\
         packages = [\"brew \\\"jq\\\"\"]\ngnome_extensions = [\"keep@x\"]\n",
        os()
    ));

    let out = e
        .temper()
        .args(["install", "--packages-only", "--dry-run", "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|err| {
        panic!(
            "install did not emit one document ({err}):\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    let warned = v["unrevertible"].as_array().cloned().unwrap_or_default();
    assert!(
        warned.is_empty(),
        "a converged machine was warned that {} could not be taken back, on a run \
         that changes nothing: {v}",
        warned.len()
    );
}

/// The control: when the run *would* change one of them, the warning is owed.
/// Without this, the assertion above is satisfied by never warning at all.
#[test]
fn a_tap_this_run_will_trust_is_named_as_unrevertible() {
    let e = Env::new();
    // The declared tap is NOT yet trusted, so this run will trust it.
    e.stub(
        "brew",
        "case \"$1 $2\" in \
         \"trust --json\") echo '{\"taps\":[]}' ;; \
         \"list --formula\") echo jq ;; \
         esac",
    );
    e.spec(&format!(
        "[brew]\ntrust = [\"a/keep\"]\n\n\
         [[machine]]\nname = \"t\"\nos = \"{}\"\npackages = [\"brew \\\"jq\\\"\"]\n",
        os()
    ));

    let out = e
        .temper()
        .args(["install", "--packages-only", "--dry-run", "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let warned = v["unrevertible"].as_array().cloned().unwrap_or_default();
    assert!(
        !warned.is_empty(),
        "this run will trust a tap that `undo` cannot untrust, and said nothing: {v}"
    );
}

/// The confirm must describe **this** run, not everything `prune` can do.
///
/// It used to recite the full catalogue every time — "uninstalls packages, GNOME
/// extensions and flatpaks, untrusts taps, removes flatpak remotes, DELETES the
/// files listed above, and un-layers rpms (a reboot applies that last one)" — so
/// a run removing nine GNOME extensions warned about deleting files and
/// rebooting, neither of which was going to happen. The careful reader is
/// alarmed by clauses that do not apply; the frequent reader stops reading. On
/// the one prompt standing between them and an irreversible removal.
#[test]
fn the_confirm_describes_only_what_this_run_does() {
    let e = Env::new();
    e.stub("gnome-extensions", "echo keep@x; echo stray@x");
    e.stub("gext", "");
    e.spec(&format!(
        "[[machine]]\nname = \"t\"\nos = \"{}\"\ngnome_extensions = [\"keep@x\"]\n",
        os()
    ));

    // No `--yes`, and stdin is not a tty → the prompt is printed, then declined.
    let out = e.temper().args(["prune"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);

    assert!(
        text.contains("uninstalls 1 GNOME extension(s)"),
        "the confirm must name what it will do: {text}"
    );
    for absent in ["DELETES", "un-layers", "untrusts", "flatpak remote"] {
        assert!(
            !text.contains(absent),
            "this run touches only extensions, yet the confirm mentioned \
             `{absent}` — a warning about work that is not going to happen: {text}"
        );
    }
    // …and nothing was removed, since the prompt was declined.
    assert!(
        !e.log("gext").contains("uninstall"),
        "declining the confirm must remove nothing"
    );
}

/// The control: a run that really does delete files and reboot says so. Without
/// this, "mentions fewer things" would be satisfied by mentioning nothing.
#[test]
fn a_run_that_deletes_and_reboots_still_says_so() {
    let e = Env::new();
    e.stub(
        "rpm-ostree",
        "case \"$1 $2\" in \"status --json\") \
         echo '{\"deployments\":[{\"booted\":true,\"requested-packages\":[\"kept\",\"strayrpm\"]}]}' ;; esac",
    );
    let doomed = e.fake_home.path().join("doomed.conf");
    fs::write(&doomed, "bye\n").unwrap();
    e.spec(&format!(
        // A declared rpm opts the question in — without one, the probe never
        // runs and nothing is an extra, which is the gate working correctly.
        "[[machine]]\nname = \"t\"\nos = \"{}\"\nrpm_ostree = [\"kept\"]\n\
         retire = [\"{}\"]\n",
        os(),
        doomed.display()
    ));

    let out = e.temper().args(["prune"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("DELETES 1 file(s)"),
        "a run that deletes a file must say so: {text}"
    );
    assert!(
        text.contains("un-layers") && text.contains("reboot"),
        "un-layering needs a reboot, and the confirm is where that belongs: {text}"
    );
    assert!(doomed.exists(), "declining the confirm must delete nothing");
}

/// The converge must name its flatpak installation **explicitly**.
///
/// An unflagged invocation resolves against `--installation` config and
/// `FLATPAK_USER_DIR`, so "the default" is the host's answer rather than
/// temper's. Removal names its scope explicitly for the same reason, and this is
/// the pairing no single-direction test can see: a converge writing to one
/// installation while `prune`/`undo` removed from another passed the whole suite
/// while removing nothing on a real machine.
#[test]
fn flatpak_install_names_its_installation() {
    let e = Env::new();
    e.stub("flatpak", "");
    e.spec(&format!(
        "[[machine]]\nname = \"t\"\nos = \"{}\"\n\
         packages = [\"flatpak \\\"org.declared.App\\\"\"]\n",
        os()
    ));

    e.temper()
        .args(["install", "--packages-only"])
        .output()
        .unwrap();

    let called = e.log("flatpak");
    let install = called
        .lines()
        .find(|l| l.starts_with("install"))
        .unwrap_or_else(|| panic!("flatpak install never ran; calls were:\n{called}"));
    assert!(
        install.contains("--system"),
        "install must name its installation explicitly: {install:?}"
    );
}

/// Removal spans both installations, as **one batched call each** — not one call
/// per app, and not one combined call.
///
/// `flatpak uninstall --user --system <app>` refuses when an app is in both
/// ("Multiple installed refs match … unable to proceed in non-interactive
/// mode"), so the scope flags do not compose the way `list`'s do. Two calls, each
/// carrying every item in its scope, is the shape that satisfies both facts —
/// and Principle #4, which is about items per call, not calls per run.
#[test]
fn prune_removes_from_both_installations_one_batched_call_each() {
    let e = Env::new();
    // Two extras in the system installation, one in the user installation, and
    // the declared app which must survive both calls.
    e.stub(
        "flatpak",
        "case \" $* \" in \
         *\"--columns=application,installation\"*) \
         printf 'org.declared.App\\tsystem\\norg.a.Stray\\tsystem\\n\
org.b.Stray\\tsystem\\norg.c.Stray\\tuser\\n' ;; \
         *\" list \"*) echo org.declared.App; echo org.a.Stray; echo org.b.Stray; echo org.c.Stray ;; \
         esac",
    );
    e.spec(&format!(
        "[[machine]]\nname = \"t\"\nos = \"{}\"\n\
         packages = [\"flatpak \\\"org.declared.App\\\"\"]\n",
        os()
    ));

    e.temper().args(["prune", "--yes"]).output().unwrap();

    let called = e.log("flatpak");
    let uninstalls: Vec<&str> = called
        .lines()
        .filter(|l| l.starts_with("uninstall"))
        .collect();
    assert_eq!(
        uninstalls.len(),
        2,
        "one batched call per installation — no more, no fewer: {uninstalls:?}"
    );
    let system = uninstalls
        .iter()
        .find(|l| l.contains("--system"))
        .unwrap_or_else(|| panic!("nothing removed from the system installation: {uninstalls:?}"));
    let user = uninstalls
        .iter()
        .find(|l| l.contains("--user"))
        .unwrap_or_else(|| panic!("nothing removed from the user installation: {uninstalls:?}"));
    assert!(
        system.contains("org.a.Stray") && system.contains("org.b.Stray"),
        "both system extras belong in ONE call, not one call each: {system:?}"
    );
    assert!(
        user.contains("org.c.Stray"),
        "the user-installation extra must be removed too: {user:?}"
    );
    assert!(
        !called.contains("org.declared.App\n") || !system.contains("org.declared.App"),
        "the declared app must survive: {called:?}"
    );
}
