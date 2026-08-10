//! The destructive paths, actually executed.
//!
//! Everything here creates real files, runs a real converge, then runs a real
//! `prune`, and asserts what survived. That matters more than usual because
//! these paths *delete user data*: residue removal deletes a file, block residue
//! **edits a file temper does not own**, and `retire` removes a path on the
//! strength of a declaration. Unit tests cover the pure logic; nothing had
//! exercised the wiring end to end, and "the command surface is right" is not
//! the same claim as "the right thing happens".

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

struct Env {
    home: TempDir,
    fake_home: TempDir,
    state: TempDir,
}

impl Env {
    fn new() -> Env {
        Env {
            home: TempDir::new().unwrap(),
            fake_home: TempDir::new().unwrap(),
            state: TempDir::new().unwrap(),
        }
    }
    fn temper(&self) -> Command {
        let mut c = Command::cargo_bin("temper").unwrap();
        c.env("TEMPER_DIR", self.home.path())
            .env("HOME", self.fake_home.path())
            .env("TEMPER_STATE_DIR", self.state.path());
        c
    }
    fn h(&self) -> &Path {
        self.home.path()
    }
    fn target(&self, rel: &str) -> std::path::PathBuf {
        self.fake_home.path().join(rel)
    }
    /// A folder whose single bundle carries `steps`.
    fn spec(&self, steps: &str) {
        fs::write(
            self.h().join("temper.toml"),
            format!(
                "[[machine]]\nname = \"t\"\nos = \"{}\"\napps = [\"a\"]\n",
                os()
            ),
        )
        .unwrap();
        fs::create_dir_all(self.h().join("apps")).unwrap();
        fs::write(self.h().join("apps/a.toml"), steps).unwrap();
    }
}

/// A `copy` step's file becomes residue when the step goes, and `prune` removes
/// it — the whole point of the deployment ledger.
#[test]
fn a_dropped_copy_step_leaves_residue_that_prune_removes() {
    let e = Env::new();
    fs::create_dir_all(e.h().join("assets")).unwrap();
    fs::write(e.h().join("assets/x.conf"), "deployed by temper\n").unwrap();
    e.spec("[[step]]\ncopy = \"assets/x.conf\"\nto = \"~/x.conf\"\n");

    e.temper().arg("install").assert().success();
    let deployed = e.target("x.conf");
    assert!(deployed.is_file(), "install should have deployed it");

    // Drop the step. The file is now something the spec no longer declares —
    // and before the ledger, nothing could have known that.
    e.spec("");
    let out = e.temper().args(["drift", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let kinds: Vec<&str> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"deployed-file-extra"),
        "drift should report the residue, got {kinds:?}"
    );

    e.temper().args(["prune", "--yes"]).assert().success();
    assert!(!deployed.exists(), "prune should have removed the residue");
}

/// A file the user edited after temper deployed it is **reported, never
/// removed**. A ledger is a record, not a licence to delete someone's work.
#[test]
fn edited_residue_survives_prune() {
    let e = Env::new();
    fs::create_dir_all(e.h().join("assets")).unwrap();
    fs::write(e.h().join("assets/x.conf"), "deployed by temper\n").unwrap();
    e.spec("[[step]]\ncopy = \"assets/x.conf\"\nto = \"~/x.conf\"\n");
    e.temper().arg("install").assert().success();

    let deployed = e.target("x.conf");
    fs::write(&deployed, "I changed this myself\n").unwrap();
    e.spec("");

    e.temper().args(["prune", "--yes"]).assert().success();
    assert!(deployed.is_file(), "an edited file must not be removed");
    assert_eq!(
        fs::read_to_string(&deployed).unwrap(),
        "I changed this myself\n",
        "…and must not be rewritten either"
    );
}

/// A dropped `block` step's residue is its REGION. The file belongs to the user,
/// so retiring a block edits it — and everything around the markers survives.
#[test]
fn a_dropped_block_step_loses_its_region_but_not_the_file() {
    let e = Env::new();
    fs::create_dir_all(e.h().join("assets")).unwrap();
    fs::write(e.h().join("assets/b.txt"), "source ~/.image\n").unwrap();
    let rc = e.target("rc");
    fs::write(&rc, "# mine above\n").unwrap();
    e.spec("[[step]]\nblock = \"assets/b.txt\"\nin = \"~/rc\"\nmarker = \"img\"\n");

    e.temper().arg("install").assert().success();
    let after_install = fs::read_to_string(&rc).unwrap();
    assert!(after_install.contains("source ~/.image"), "{after_install}");
    assert!(after_install.contains("# mine above"), "{after_install}");

    e.spec("");
    e.temper().args(["prune", "--yes"]).assert().success();

    let after_prune = fs::read_to_string(&rc).expect("the file itself must survive");
    assert!(
        !after_prune.contains("temper:img"),
        "the region should be gone: {after_prune}"
    );
    assert!(
        !after_prune.contains("source ~/.image"),
        "the body should be gone: {after_prune}"
    );
    assert!(
        after_prune.contains("# mine above"),
        "the user's own content must survive: {after_prune}"
    );
}

/// `retire` removes a path the spec declares must not exist — including one
/// temper never deployed, which is the case the ledger structurally cannot cover.
#[test]
fn retire_removes_a_path_temper_never_deployed() {
    let e = Env::new();
    let stray = e.target("stray.conf");
    fs::write(&stray, "left over from something else\n").unwrap();

    fs::write(
        e.h().join("temper.toml"),
        format!(
            "[[machine]]\nname = \"t\"\nos = \"{}\"\nretire = [\"~/stray.conf\"]\n",
            os()
        ),
    )
    .unwrap();

    let out = e.temper().args(["drift", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let kinds: Vec<&str> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"retired-present"), "got {kinds:?}");

    e.temper().args(["prune", "--yes"]).assert().success();
    assert!(!stray.exists(), "prune should have removed the retired path");

    // …and once it is gone, it is not drift any more.
    let out = e.temper().args(["drift", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["out_of_sync"], 0, "a satisfied retirement is not drift");
}

/// `temper retired` lists the tombstones and marks which are still doing work.
#[test]
fn the_retired_verb_reports_what_is_still_present() {
    let e = Env::new();
    fs::write(e.target("here.conf"), "x").unwrap();
    fs::write(
        e.h().join("temper.toml"),
        format!(
            "[[machine]]\nname = \"t\"\nos = \"{}\"\n\
             retire = [\"~/here.conf\", \"~/already-gone.conf\"]\n",
            os()
        ),
    )
    .unwrap();

    let out = e.temper().args(["retired", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let rows = v["retired"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let present: Vec<bool> = rows.iter().map(|r| r["present"].as_bool().unwrap()).collect();
    assert_eq!(
        present,
        vec![true, false],
        "one still doing work, one already done"
    );
}
