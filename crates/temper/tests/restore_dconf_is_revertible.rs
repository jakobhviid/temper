//! `restore-dconf` writes live desktop state, and `undo` must put it back.
//!
//! This pair had no automated coverage at all. The suite reached `restore` only
//! under `--dry-run`, which returns before `dconf load` ever runs — so the undo
//! payload capture and `journal::dconf_load_tree` were exercised by nothing, on
//! the one verb that overwrites a desktop wholesale.
//!
//! The load is the reason it matters. **`dconf load` MERGES**: it writes the keys
//! it is given and leaves every other key alone. So replaying the prior dump on
//! its own does not revert a restore — every key the restore *introduced* is
//! absent from that dump, and survives. `dconf_load_tree` exists to reset the
//! subtree first, and nothing checked that it does.
//!
//! The fake `dconf` here therefore models the merge rather than an overwrite. A
//! stub whose `load` replaced the store would pass whether or not temper resets,
//! which is the shape of a test that cannot fail.

use std::fs;

use assert_cmd::Command;
use tempfile::TempDir;

const PRIOR: &str = "[/]\nkept-key='was-here-before'\n";
const SNAPSHOT: &str = "[/]\nintroduced-key='from-the-snapshot'\n";

struct Env {
    home: TempDir,
    fake_home: TempDir,
    state: TempDir,
    bin: TempDir,
}

impl Env {
    fn new() -> Env {
        let e = Env {
            home: TempDir::new().unwrap(),
            fake_home: TempDir::new().unwrap(),
            state: TempDir::new().unwrap(),
            bin: TempDir::new().unwrap(),
        };
        fs::write(
            e.home.path().join("temper.toml"),
            "[[machine]]\nname = \"box\"\nos = \"linux\"\n\n\
             [[machine.dconf]]\npath = \"/org/test/app/\"\nfile = \"assets/app.dconf\"\n",
        )
        .unwrap();
        fs::create_dir_all(e.home.path().join("assets")).unwrap();
        fs::write(e.home.path().join("assets/app.dconf"), SNAPSHOT).unwrap();

        // `observe()` refuses to restore without a real user database — the
        // check that stops an unreadable store from recording an empty (and
        // therefore lying) undo payload.
        let db = e.fake_home.path().join(".config/dconf");
        fs::create_dir_all(&db).unwrap();
        fs::write(db.join("user"), b"").unwrap();

        // The store starts holding only what was there before the restore.
        fs::write(e.dconf_state(), PRIOR).unwrap();
        e.fake_dconf();
        e
    }

    fn dconf_state(&self) -> std::path::PathBuf {
        self.fake_home.path().join("dconf-store")
    }
    fn dconf_log(&self) -> std::path::PathBuf {
        self.fake_home.path().join("dconf.log")
    }

    /// A `dconf` that keeps a store on disk and honours the three verbs temper
    /// uses. `load` **appends**, because the real one merges.
    fn fake_dconf(&self) {
        let p = self.bin.path().join("dconf");
        fs::write(
            &p,
            format!(
                r#"#!/bin/sh
echo "$@" >> {log}
case "$1" in
  dump)  cat {store} 2>/dev/null ;;
  load)  cat >> {store} ;;
  reset) : > {store} ;;
  *)     : ;;
esac
exit 0
"#,
                log = self.dconf_log().display(),
                store = self.dconf_state().display(),
            ),
        )
        .unwrap();
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

    fn store(&self) -> String {
        fs::read_to_string(self.dconf_state()).unwrap_or_default()
    }
    fn log(&self) -> String {
        fs::read_to_string(self.dconf_log()).unwrap_or_default()
    }
}

/// The round trip: restore puts the snapshot into the store, undo takes it back
/// out — including the key the snapshot introduced, which is the one a bare
/// `load` would strand.
#[test]
fn undo_reverts_a_restore_including_the_keys_it_introduced() {
    let e = Env::new();

    e.temper()
        .args(["restore-dconf", "--yes"])
        .output()
        .unwrap();
    let after_restore = e.store();
    assert!(
        after_restore.contains("introduced-key"),
        "the restore never loaded the snapshot, so there is nothing to revert:\n{after_restore}"
    );
    assert!(
        after_restore.contains("kept-key"),
        "`dconf load` merges — the prior key must still be there:\n{after_restore}"
    );

    let out = e.temper().args(["undo"]).output().unwrap();
    assert!(
        out.status.success(),
        "undo failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let after_undo = e.store();
    assert!(
        !after_undo.contains("introduced-key"),
        "the key the restore INTRODUCED survived the undo — this is exactly what a \
         `load` without a preceding `reset` leaves behind:\n{after_undo}"
    );
    assert!(
        after_undo.contains("kept-key"),
        "the undo went too far and dropped state that predated the restore:\n{after_undo}"
    );
}

/// The mechanism, asserted directly: the revert resets the subtree *before*
/// loading. Ordering is the whole correctness argument, so it is pinned rather
/// than inferred from the end state alone.
#[test]
fn the_revert_resets_the_subtree_before_loading_it() {
    let e = Env::new();
    e.temper()
        .args(["restore-dconf", "--yes"])
        .output()
        .unwrap();

    let before_undo = e.log().lines().count();
    e.temper().args(["undo"]).output().unwrap();

    let undo_calls: Vec<String> = e
        .log()
        .lines()
        .skip(before_undo)
        .map(|l| l.to_string())
        .collect();

    let reset = undo_calls.iter().position(|l| l.starts_with("reset"));
    let load = undo_calls.iter().position(|l| l.starts_with("load"));
    assert!(
        reset.is_some() && load.is_some(),
        "undo did not reset-then-load the subtree at all: {undo_calls:?}"
    );
    assert!(
        reset < load,
        "the revert loaded before resetting, so every key the restore added \
         survives it: {undo_calls:?}"
    );
    assert!(
        undo_calls.iter().any(|l| l.contains("/org/test/app/")),
        "the revert did not name the declared subtree: {undo_calls:?}"
    );
}

/// `--dry-run` is the path the suite already reached, kept here as the control:
/// it must touch the store not at all, or the two tests above prove nothing
/// about the real one.
#[test]
fn a_dry_run_restore_writes_nothing() {
    let e = Env::new();
    e.temper()
        .args(["restore-dconf", "--dry-run", "--yes"])
        .output()
        .unwrap();

    assert_eq!(
        e.store(),
        PRIOR,
        "a dry run changed live dconf — it must return before the load"
    );
    assert!(
        !e.log().lines().any(|l| l.starts_with("load")),
        "a dry run ran `dconf load`:\n{}",
        e.log()
    );
}
