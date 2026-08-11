//! The seed path (`init`, `reconcile --current-state-wins`) and the desktop
//! capture/restore pair. These are deterministic without a live GNOME session:
//! they exercise the guard rails, not the dconf round-trip itself (which needs
//! a real desktop — see the ROADMAP VM checklist).

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

fn temper(home: &Path, fake_home: &Path, state: &Path) -> Command {
    let mut c = Command::cargo_bin("temper").unwrap();
    c.env("TEMPER_DIR", home)
        .env("HOME", fake_home)
        // `dconf::user_db()` reads XDG_CONFIG_HOME *before* HOME, and returns
        // nothing at all when DCONF_PROFILE is set — so pinning HOME alone
        // leaves the answer to whatever session runs the suite.
        .env("XDG_CONFIG_HOME", fake_home.join(".config"))
        .env_remove("DCONF_PROFILE")
        // Anything `with_dconf` installed goes first; the rest is for /bin/sh.
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", fake_home.join("bin").display()),
        )
        .env("TEMPER_STATE_DIR", state);
    c
}

/// Give this fake home a working dconf: a database so the observability guard
/// passes, and a `dconf` that answers the three verbs temper drives it with.
///
/// Without this, every dconf assertion in this file was conditional on the
/// developer having a desktop session — two of them said nothing at all on a
/// runner, while still reporting as passing tests.
fn with_dconf(fake_home: &Path) {
    let db = fake_home.join(".config/dconf");
    fs::create_dir_all(&db).unwrap();
    fs::write(db.join("user"), b"\0").unwrap();

    let bin = fake_home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let p = bin.join("dconf");
    // `load` and `reset` record that they were asked, so a test can tell a
    // preview from a write. `dump` answers with one key so a capture has
    // something to write and an empty file means it never ran.
    fs::write(
        &p,
        format!(
            r#"#!/bin/sh
log={}
case "$1" in
  dump)  echo '[/]'; echo "k='v'" ;;
  load)  cat > /dev/null; echo "load $2" >> "$log" ;;
  reset) echo "reset $2" >> "$log" ;;
  *) : ;;
esac
exit 0
"#,
            fake_home.join("dconf-calls").display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A machine with one declared, labelled dconf snapshot.
fn home_with_snapshot(home: &Path) {
    fs::write(
        home.join("temper.toml"),
        format!(
            "[[machine]]\nname = \"t\"\nos = \"{}\"\n\n\
             [[machine.dconf]]\npath = \"/org/gnome/shell/\"\n\
             file = \"assets/shell.dconf\"\nlabel = \"shell\"\n",
            os()
        ),
    )
    .unwrap();
}

/// The dconf verbs are named for the MECHANISM (`snapshot-dconf`), not the
/// desktop.
///
/// They were once named for the desktop, on the reasoning that a KDE or macOS
/// equivalent would not be dconf. That had it backwards: dconf is the GSettings
/// backend and is present under KDE too, so `snapshot-gnome` was describing the
/// desktop it was *usually* run on rather than the store it actually reads. A
/// second backend becomes `snapshot-kconfig` — a sibling, not a desktop variant.
///
/// Both older spellings stay working as aliases. `snapshot`/`restore` are
/// everyday muscle memory and `snapshot-gnome`/`restore-gnome` are in scripts;
/// renaming a verb should never be the thing that breaks someone's afternoon.
#[test]
fn the_old_bare_verb_names_still_work() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    home_with_snapshot(home.path());

    for name in [
        "snapshot",
        "snapshot-gnome",
        "snapshot-dconf",
        "restore",
        "restore-gnome",
        "restore-dconf",
    ] {
        temper(home.path(), fake_home.path(), state.path())
            .args([name, "--help"])
            .assert()
            .success();
    }
    // …and the new names are the ones advertised.
    let assert = temper(home.path(), fake_home.path(), state.path())
        .arg("--help")
        .assert()
        .success();
    let help = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(help.contains("snapshot-dconf"), "{help}");
    assert!(help.contains("restore-dconf"), "{help}");
    // `reconcile` is untouched — it is not a dconf verb.
    assert!(help.contains("reconcile"), "{help}");
}

#[test]
fn dump_is_gone_and_init_replaces_it() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    home_with_snapshot(home.path());

    // `backup` and `dump` were both cut (the breaking change).
    for gone in ["backup", "dump"] {
        temper(home.path(), fake_home.path(), state.path())
            .arg(gone)
            .assert()
            .failure();
    }
    for live in ["init", "snapshot", "reconcile"] {
        temper(home.path(), fake_home.path(), state.path())
            .args([live, "--help"])
            .assert()
            .success();
    }
}

#[test]
fn include_trust_cannot_be_used_without_current_state_wins() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    home_with_snapshot(home.path());

    // Absorbing fleet-scope trust must never happen as a side effect of an
    // interactive run — clap enforces the pairing.
    temper(home.path(), fake_home.path(), state.path())
        .args(["reconcile", "--include-trust"])
        .assert()
        .failure();

    // The documented spelling and its alias both parse.
    for flag in ["--current-state-wins", "--csw"] {
        temper(home.path(), fake_home.path(), state.path())
            .args(["--json", "reconcile", flag])
            .assert()
            .success();
    }
}

#[test]
fn init_infers_the_machine_name_from_the_hostname() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    // No temper.toml at all — init bootstraps the folder named by TEMPER_DIR.

    temper(home.path(), fake_home.path(), state.path())
        .args(["init", "--yes"])
        .assert()
        .success();

    let tt = fs::read_to_string(home.path().join("temper.toml")).unwrap();
    let host = String::from_utf8(
        std::process::Command::new("hostname")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let host = host.trim().split('.').next().unwrap().to_lowercase();

    assert!(tt.contains("[[machine]]"), "no machine block: {tt}");
    assert!(
        tt.contains(&format!("name = \"{host}\"")),
        "name not inferred from hostname ({host}): {tt}"
    );
    assert!(tt.contains(&format!("os = \"{}\"", os())), "{tt}");
    // The Brewfile it wired up must actually exist, or the spec points at air.
    assert!(
        home.path().join(format!("brewfiles/{host}")).is_file(),
        "brewfile not created"
    );
}

#[test]
fn init_appends_to_a_populated_manifest_without_disturbing_it() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let before = format!(
        "# hand-authored\n[vars]\nEDITOR = \"nvim\"\n\n\
         [brew]\ntrust = [\"ublue-os/tap\"]\n\n\
         [[machine]]\nname = \"other\"\nos = \"{}\"\napps = [\"shell\"]\n\n\
         [[machine.dconf]]\npath = \"/org/gnome/shell/\"\nfile = \"assets/a.dconf\"\n",
        os()
    );
    fs::write(home.path().join("temper.toml"), &before).unwrap();

    temper(home.path(), fake_home.path(), state.path())
        .args(["init", "newbox", "--yes"])
        .assert()
        .success();

    let after = fs::read_to_string(home.path().join("temper.toml")).unwrap();
    // Everything that was there is still there — comments, vars, the other
    // machine, and its nested dconf block.
    for kept in [
        "# hand-authored",
        "EDITOR = \"nvim\"",
        "name = \"other\"",
        "apps = [\"shell\"]",
        "[[machine.dconf]]",
        "file = \"assets/a.dconf\"",
    ] {
        assert!(after.contains(kept), "init disturbed `{kept}`:\n{after}");
    }
    // …and the new machine was APPENDED, not substituted.
    assert!(after.contains("name = \"newbox\""), "{after}");
    assert_eq!(after.matches("[[machine]]").count(), 2, "{after}");

    // A declared tap this fresh machine hasn't trusted yet must SURVIVE: the
    // machine's "current state" here is absence-of-setup, and `[brew].trust` is
    // fleet-scope, so dropping it would break every other machine.
    assert!(
        after.contains("ublue-os/tap"),
        "init dropped a fleet-scope tap this new machine simply hasn't trusted yet:\n{after}"
    );
}

#[test]
fn init_refuses_to_overwrite_a_machine_that_already_exists() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    fs::write(
        home.path().join("temper.toml"),
        format!("[[machine]]\nname = \"mine\"\nos = \"{}\"\n# hand-authored\n", os()),
    )
    .unwrap();

    temper(home.path(), fake_home.path(), state.path())
        .args(["init", "mine", "--yes"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("reconcile"));

    // The hand-authored file is untouched — including its comment.
    let tt = fs::read_to_string(home.path().join("temper.toml")).unwrap();
    assert!(tt.contains("# hand-authored"), "{tt}");
}

#[test]
fn a_declared_but_missing_brewfile_reads_as_empty() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    // `brewfile` points at a file that doesn't exist yet — the seed case.
    fs::write(
        home.path().join("temper.toml"),
        format!(
            "[[machine]]\nname = \"t\"\nos = \"{}\"\nbrewfile = \"brewfiles/t\"\n",
            os()
        ),
    )
    .unwrap();

    temper(home.path(), fake_home.path(), state.path())
        .args(["--json", "reconcile"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"drops\":[]"));
}

#[test]
fn snapshot_without_declared_subtrees_is_a_clean_no_op() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    fs::write(
        home.path().join("temper.toml"),
        format!("[[machine]]\nname = \"t\"\nos = \"{}\"\n", os()),
    )
    .unwrap();

    temper(home.path(), fake_home.path(), state.path())
        .args(["--json", "snapshot"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"captured\":[]"));
}

#[test]
fn restore_dry_run_never_journals() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    home_with_snapshot(home.path());
    fs::create_dir_all(home.path().join("assets")).unwrap();
    fs::write(home.path().join("assets/shell.dconf"), "[/]\nk='v'\n").unwrap();

    with_dconf(fake_home.path());

    // The exit status is the point, not decoration: discarding it let this pass
    // on a host with no dconf, where `restore` bails at the observability guard
    // long before any journal code — so "no journal was written" was true for
    // the wrong reason and proved nothing about dry-run.
    temper(home.path(), fake_home.path(), state.path())
        .args(["--json", "restore", "--dry-run"])
        .assert()
        .success();

    assert!(
        !state.path().join("runs").exists(),
        "a dry run must never journal"
    );
    // …and it must not have touched live dconf either, which only an observation
    // of the child can settle.
    assert!(
        !fake_home.path().join("dconf-calls").exists(),
        "a dry run ran dconf: {}",
        fs::read_to_string(fake_home.path().join("dconf-calls")).unwrap_or_default()
    );
}

#[test]
fn a_labelled_snapshot_is_named_in_drift_output() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    home_with_snapshot(home.path());

    with_dconf(fake_home.path());

    // No snapshot file written yet, and dconf is readable — so this is real
    // drift, and the finding must appear. Guarding the assertion behind `if
    // out.contains("dconf-uncaptured")` meant the whole check evaporated on any
    // host without a desktop session, which is every CI runner.
    let assert = temper(home.path(), fake_home.path(), state.path())
        .args(["--json", "drift"])
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        out.contains("dconf-uncaptured"),
        "a declared snapshot that was never captured is drift: {out}"
    );
    assert!(
        out.contains("dconf/shell"),
        "the label names the snapshot, not a raw dconf path: {out}"
    );
}
