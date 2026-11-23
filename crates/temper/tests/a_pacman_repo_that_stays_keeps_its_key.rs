//! A pacman repo file that stays on disk keeps the keys `prune` would remove.
//!
//! pacman verifies a repo against its own keyring, so a repo still configured
//! with its key gone fails every sync — not just that repo's packages, the
//! whole `pacman -Syu`. `prune` removes a dropped repo's file and then the keys
//! temper imported for it, which is safe only when the file went. Two ways it
//! does not: the user edited it, so it is reported and left; or removing it
//! failed. Which dropped repo a key signed is not recorded, so in both cases no
//! key goes.
//!
//! pacman, pacman-key and sudo are faked, so this runs anywhere and the key
//! removal can be observed instead of performed.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use tempfile::TempDir;

const KEY: &str = "0123456789ABCDEF0123456789ABCDEF01234567";

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
    fn new() -> Env {
        let e = Env {
            home: TempDir::new().unwrap(),
            fake_home: TempDir::new().unwrap(),
            state: TempDir::new().unwrap(),
            bin: TempDir::new().unwrap(),
        };
        // pacman answers nothing; pacman-key records what it is asked; sudo
        // drops its own options and runs the rest as the user.
        e.stub("pacman", "exit 0");
        let log = e.key_log();
        e.stub("pacman-key", &format!("echo \"$@\" >> {}\nexit 0", log.display()));
        e.stub("sudo", "while [ \"${1#-}\" != \"$1\" ]; do shift; done\n[ $# -eq 0 ] || exec \"$@\"");
        // The key temper imported for a pacman repo the spec has since dropped.
        let keys = e.state.path().join("pacman-keys");
        fs::create_dir_all(&keys).unwrap();
        fs::write(keys.join("t.json"), format!("[\"{KEY}\"]")).unwrap();
        e
    }

    fn stub(&self, name: &str, body: &str) {
        let p = self.bin.path().join(name);
        fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn key_log(&self) -> std::path::PathBuf {
        self.fake_home.path().join("pacman-key.log")
    }

    fn spec(&self, steps: &str) {
        fs::write(
            self.home.path().join("temper.toml"),
            format!("[[machine]]\nname = \"t\"\nos = \"{}\"\napps = [\"a\"]\n", os()),
        )
        .unwrap();
        fs::create_dir_all(self.home.path().join("apps")).unwrap();
        fs::write(self.home.path().join("apps/a.toml"), steps).unwrap();
    }

    fn ledger(&self) -> std::path::PathBuf {
        self.state.path().join("deployed/t.json")
    }

    fn temper(&self) -> Command {
        let mut c = Command::cargo_bin("temper").unwrap();
        c.env("TEMPER_DIR", self.home.path())
            .env("HOME", self.fake_home.path())
            .env("TEMPER_STATE_DIR", self.state.path())
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.path().display()));
        c
    }

    fn plan(&self) -> serde_json::Value {
        let out = self.temper().args(["prune", "--json"]).output().unwrap();
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!("{e}: {}", String::from_utf8_lossy(&out.stdout))
        })
    }
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_default()
}

/// The control: with no repo file left behind, the key is planned for removal.
/// Without this, the tests below would pass on a machine where prune never saw
/// the key at all.
#[test]
fn a_dropped_repos_key_is_pruned() {
    let e = Env::new();
    e.spec("");
    let v = e.plan();
    assert_eq!(v["pacman_keys"], serde_json::json!([KEY]), "{v}");
}

/// An edited repo file is reported and left on disk, so its key stays too —
/// said, with the reason, under `kept`.
#[test]
fn an_edited_repo_file_keeps_the_key() {
    let e = Env::new();
    e.spec("");
    let repo = e.fake_home.path().join("extra.conf");
    fs::write(&repo, "[extra]\nServer = https://example.org/$arch\n").unwrap();
    fs::create_dir_all(e.ledger().parent().unwrap()).unwrap();
    // A hash that matches nothing: the file is not what temper wrote.
    fs::write(
        e.ledger(),
        serde_json::json!({ repo.display().to_string(): {
            "hash": "edited", "kind": "pacman-repo", "path": repo.display().to_string()
        }})
        .to_string(),
    )
    .unwrap();

    let v = e.plan();
    assert_eq!(v["residue_edited"], serde_json::json!([repo.display().to_string()]), "{v}");
    assert_eq!(v["pacman_keys"], serde_json::json!([]), "the key was planned for removal: {v}");
    let kept = v["kept"].as_array().unwrap();
    assert!(
        kept.iter().any(|k| k["list"] == "pacman_keys" && k["item"] == KEY && k["why"].is_string()),
        "the key stays, and the plan must say why: {v}"
    );

    e.temper().args(["prune", "--yes"]).assert().success();
    assert!(
        !read(&e.key_log()).contains("--delete"),
        "prune deleted the key of a repo still on disk: {}",
        read(&e.key_log())
    );
    assert!(repo.exists());
}

/// A repo file prune planned to remove and could not keeps the key as well, and
/// the key is reported as not removed rather than counted.
#[test]
fn a_repo_file_that_could_not_be_removed_keeps_the_key() {
    let e = Env::new();
    // Deploy a real file, so the ledger holds the hash of exactly what is there.
    let assets = e.home.path().join("assets");
    fs::create_dir_all(&assets).unwrap();
    fs::write(assets.join("extra.conf"), "[extra]\n").unwrap();
    e.spec("[[step]]\ncopy = \"assets/extra.conf\"\nto = \"~/repos/extra.conf\"\n");
    e.temper().arg("install").assert().success();
    // …recorded as a pacman repo, then dropped from the spec.
    let mut ledger: serde_json::Value = serde_json::from_str(&read(&e.ledger())).unwrap();
    for rec in ledger.as_object_mut().unwrap().values_mut() {
        rec["kind"] = serde_json::json!("pacman-repo");
    }
    fs::write(e.ledger(), ledger.to_string()).unwrap();
    e.spec("");

    let v = e.plan();
    assert_eq!(v["residue"].as_array().map(Vec::len), Some(1), "{v}");
    assert_eq!(v["pacman_keys"], serde_json::json!([KEY]), "{v}");

    // A directory the user cannot write: the removal fails.
    let dir = e.fake_home.path().join("repos");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    }
    if fs::write(dir.join("probe"), "").is_ok() {
        eprintln!("skipped: running as root, so no directory refuses a removal");
        return;
    }
    let out = e.temper().args(["prune", "--yes"]).output().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(dir.join("extra.conf").exists(), "the removal was meant to fail");
    assert!(
        !read(&e.key_log()).contains("--delete"),
        "prune deleted the key of a repo it could not remove: {}",
        read(&e.key_log())
    );
    assert!(stdout.contains(KEY), "the key must be reported as not removed: {stdout}");
}
