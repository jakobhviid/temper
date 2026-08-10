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

/// Everything `prune` counts, it also shows — and publishes.
///
/// The confirm reads "remove N item(s) **listed above**", so a list that is
/// counted but not printed makes the prompt a lie about the most destructive
/// thing in the tool. `retired` was exactly that: `items()` counted it, the
/// preview never printed it, `--json` never carried it, and `commit_prune`
/// deleted the file.
#[test]
fn prune_previews_every_item_it_counts() {
    let e = Env::new();
    let stray = e.target("stray.conf");
    fs::write(&stray, "left over\n").unwrap();
    fs::create_dir_all(e.h().join("assets")).unwrap();
    fs::write(e.h().join("assets/x.conf"), "deployed by temper\n").unwrap();

    // One retired path plus one piece of real residue, so the check is about the
    // preview covering the plan rather than about a single list.
    let spec = |retire: &str, steps: &str| {
        fs::write(
            e.h().join("temper.toml"),
            format!(
                "[[machine]]\nname = \"t\"\nos = \"{}\"\napps = [\"a\"]\n{retire}",
                os()
            ),
        )
        .unwrap();
        fs::create_dir_all(e.h().join("apps")).unwrap();
        fs::write(e.h().join("apps/a.toml"), steps).unwrap();
    };
    spec("", "[[step]]\ncopy = \"assets/x.conf\"\nto = \"~/x.conf\"\n");
    e.temper().arg("install").assert().success();
    spec("retire = [\"~/stray.conf\"]\n", "");

    let out = e.temper().args(["prune", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let retired: Vec<&str> = v["retired"]
        .as_array()
        .expect("--json must carry `retired`")
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert_eq!(retired.len(), 1, "got {v}");
    assert!(
        v["flatpak_remotes"].is_array(),
        "--json must carry every list prune acts on, got {v}"
    );

    // The human preview must print one line per counted item, and the count in
    // the dry-run summary must match what was printed.
    let out = e.temper().args(["prune", "--dry-run"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let listed = text.lines().filter(|l| l.trim_start().starts_with("- ")).count();
    let counted: usize = text
        .split_once(": ")
        .and_then(|(_, t)| t.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no count in: {text}"));
    assert_eq!(
        listed, counted,
        "prune counted {counted} item(s) and listed {listed}:\n{text}"
    );
    assert!(text.contains("stray.conf"), "the retired path was never shown:\n{text}");
}

/// A retired **directory** is removed, and a failure is never counted as work.
///
/// `remove_file` returns EISDIR on a directory, so the removal warned and prune
/// reported it removed anyway — while SPEC's own worked example for `retire` is
/// `~/.config/old-app`, a directory.
#[test]
fn retire_removes_a_directory_and_counts_only_what_went() {
    let e = Env::new();
    let dir = e.target("old-app");
    fs::create_dir_all(dir.join("nested")).unwrap();
    fs::write(dir.join("nested/state.json"), "{}").unwrap();

    fs::write(
        e.h().join("temper.toml"),
        format!(
            "[[machine]]\nname = \"t\"\nos = \"{}\"\nretire = [\"~/old-app\"]\n",
            os()
        ),
    )
    .unwrap();

    let out = e.temper().args(["prune", "--yes"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(!dir.exists(), "the retired directory is still here:\n{text}");
    assert!(
        text.contains("1 item(s) removed"),
        "and the count should say so:\n{text}"
    );
}

/// Residue that prune removed stops being reported.
///
/// Nothing dropped the record, so the next `drift` re-reported the deleted file
/// — and, because an absent file is not "untouched", described it as one the
/// user had edited. Red forever, answerable by no verb.
#[test]
fn removed_residue_does_not_come_back_as_drift() {
    let e = Env::new();
    fs::create_dir_all(e.h().join("assets")).unwrap();
    fs::write(e.h().join("assets/x.conf"), "deployed by temper\n").unwrap();
    e.spec("[[step]]\ncopy = \"assets/x.conf\"\nto = \"~/x.conf\"\n");
    e.temper().arg("install").assert().success();
    e.spec("");

    e.temper().args(["prune", "--yes"]).assert().success();
    assert!(!e.target("x.conf").exists());

    let out = e.temper().args(["drift", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let kinds: Vec<&str> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["kind"].as_str().unwrap())
        .collect();
    assert!(
        !kinds.contains(&"deployed-file-extra"),
        "a file prune deleted is still being reported: {v}"
    );
    assert_eq!(v["out_of_sync"], 0, "…and it counts as drift: {v}");
}

/// Two `block` steps in one file are two pieces of residue, not one.
///
/// The ledger keyed by path, so the second block overwrote the first and only
/// one region was ever tracked. Dropping either left an untracked region in the
/// user's file forever.
#[test]
fn two_blocks_in_one_file_are_tracked_separately() {
    let e = Env::new();
    fs::create_dir_all(e.h().join("assets")).unwrap();
    fs::write(e.h().join("assets/one"), "line one\n").unwrap();
    fs::write(e.h().join("assets/two"), "line two\n").unwrap();
    fs::write(e.target("rc"), "# the user's own\n").unwrap();
    e.spec(
        "[[step]]\nblock = \"assets/one\"\nin = \"~/rc\"\nmarker = \"one\"\n\n\
         [[step]]\nblock = \"assets/two\"\nin = \"~/rc\"\nmarker = \"two\"\n",
    );
    e.temper().arg("install").assert().success();
    let rc = fs::read_to_string(e.target("rc")).unwrap();
    assert!(rc.contains("line one") && rc.contains("line two"), "{rc}");

    // Drop only the first. The second is still declared and must survive.
    e.spec("[[step]]\nblock = \"assets/two\"\nin = \"~/rc\"\nmarker = \"two\"\n");
    let out = e.temper().args(["prune", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let residue: Vec<&str> = v["residue"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert_eq!(
        residue.len(),
        1,
        "exactly the dropped region is residue: {v}"
    );

    e.temper().args(["prune", "--yes"]).assert().success();
    let rc = fs::read_to_string(e.target("rc")).unwrap();
    assert!(!rc.contains("line one"), "the dropped region should be gone:\n{rc}");
    assert!(rc.contains("line two"), "the declared region must survive:\n{rc}");
    assert!(rc.contains("# the user's own"), "…and so must the user's file:\n{rc}");
}

/// Re-spelling a still-declared target does not make its file residue.
///
/// The ledger compared keys as written while everything else resolved them, so
/// changing `to = "~/x.conf"` to the absolute path made the old key look like
/// residue — and prune deleted a file the spec still declared.
#[test]
fn respelling_a_declared_target_is_not_residue() {
    let e = Env::new();
    fs::create_dir_all(e.h().join("assets")).unwrap();
    fs::write(e.h().join("assets/x.conf"), "deployed by temper\n").unwrap();
    e.spec("[[step]]\ncopy = \"assets/x.conf\"\nto = \"~/x.conf\"\n");
    e.temper().arg("install").assert().success();
    let deployed = e.target("x.conf");
    assert!(deployed.is_file());

    // Same file, spelled absolutely.
    e.spec(&format!(
        "[[step]]\ncopy = \"assets/x.conf\"\nto = \"{}\"\n",
        deployed.display()
    ));
    let out = e.temper().args(["prune", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["residue"].as_array().map(|a| a.len()).unwrap_or(0),
        0,
        "a re-spelled target is the same file, not residue: {v}"
    );
    e.temper().args(["prune", "--yes"]).assert().success();
    assert!(
        deployed.exists(),
        "prune deleted a file the spec still declares"
    );
}
