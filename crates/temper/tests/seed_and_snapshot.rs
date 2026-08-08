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
        .env("TEMPER_STATE_DIR", state);
    c
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

    // On a dconf host this previews; without dconf it fails loudly rather than
    // pretending. Either way a dry run must leave no undo entry behind.
    let _ = temper(home.path(), fake_home.path(), state.path())
        .args(["--json", "restore", "--dry-run"])
        .assert();
    assert!(
        !state.path().join("runs").exists(),
        "a dry run must never journal"
    );
}

#[test]
fn a_labelled_snapshot_is_named_in_drift_output() {
    let home = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    home_with_snapshot(home.path());

    // No snapshot file written yet. On a dconf host that is real drift ("never
    // captured"); off one it degrades silently. Either way drift must succeed,
    // and when it does speak it uses the label, not a raw dconf path.
    let assert = temper(home.path(), fake_home.path(), state.path())
        .args(["--json", "drift"])
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    if out.contains("dconf-uncaptured") {
        assert!(out.contains("dconf/shell"), "label not used: {out}");
    }
}
