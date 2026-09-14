//! A repo finding names the bundle that declared it.
//!
//! `rpm_repos` is declared per bundle and gated with it, and its drift is
//! file-shaped: a destination, a source, a three-valued state, exactly like the
//! `sysfile` steps beside it. But it converges as one whole-machine batch
//! before packages, and that ordering — a converge concern — is what decided
//! its reporting, so every repo was attributed to the provider kind instead.
//!
//! Two things followed, and neither is cosmetic. Two bundles' repos merged
//! under one label, so a drifted repo could not be traced to the file that
//! declares it. And a bundle whose *only* declaration is a repo vanished from
//! the report, replaced by a name matching nothing in the folder — the count
//! stayed right, one real bundle out and one label in, which is precisely why
//! nobody caught it.
//!
//! ARCHITECTURE states the rule this crossed: scope is a property of the
//! declaration, not of the kind, so a finding carries the file where its
//! declaration actually lives.

#![cfg(target_os = "linux")]

use std::fs;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

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
        // The repo provider gates on rpm-ness, not atomic-ness — without this
        // the state is `unavailable` and the attribution is never exercised.
        e.stub("rpm");
        e
    }

    fn stub(&self, name: &str) {
        let p = self.bin.path().join(name);
        fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn write(&self, rel: &str, body: &str) {
        let p = self.home.path().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    /// A vendor `.repo` asset plus a bundle whose ONLY declaration is that repo.
    fn repo_only_bundle(&self, bundle: &str, vendor: &str) {
        self.write(
            &format!("assets/rpm-repos/{vendor}.repo"),
            &format!("[{vendor}]\nname={vendor}\nbaseurl=https://example.invalid/rpm\n"),
        );
        self.write(
            &format!("apps/{bundle}.toml"),
            &format!(
                "rpm_repos = [{{ repo = \"assets/rpm-repos/{vendor}.repo\" }}]\n"
            ),
        );
    }

    fn drift(&self) -> Vec<Value> {
        let out = Command::cargo_bin("temper")
            .unwrap()
            .args(["drift", "--json"])
            .env("TEMPER_DIR", self.home.path())
            .env("HOME", self.fake_home.path())
            .env("XDG_CONFIG_HOME", self.fake_home.path().join(".config"))
            .env_remove("DCONF_PROFILE")
            .env("TEMPER_STATE_DIR", self.state.path())
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.path().display()))
            .output()
            .unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "drift did not emit one document ({e}):\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        v["items"].as_array().cloned().unwrap_or_default()
    }
}

/// The app of every `rpm-repo` finding whose target names `vendor`.
fn app_of(items: &[Value], vendor: &str) -> String {
    let hits: Vec<&Value> = items
        .iter()
        .filter(|f| {
            f["kind"] == "rpm-repo"
                && f["target"].as_str().unwrap_or_default().contains(vendor)
        })
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one finding for `{vendor}`: {items:?}"
    );
    hits[0]["app"].as_str().unwrap_or_default().to_string()
}

#[test]
fn a_bundle_whose_only_declaration_is_a_repo_is_the_name_in_the_report() {
    let e = Env::new();
    e.repo_only_bundle("proton-vpn", "protonvpn");
    e.write(
        "temper.toml",
        "[[machine]]\nname = \"m\"\nos = \"linux\"\napps = [\"proton-vpn\"]\n",
    );

    let items = e.drift();
    assert_eq!(
        app_of(&items, "protonvpn"),
        "proton-vpn",
        "the repo must be attributed to the bundle that declares it: {items:?}"
    );
    // And the kind must not moonlight as an app — that is the name a reader
    // cannot find a file for.
    assert!(
        !items.iter().any(|f| f["app"] == "rpm-repo"),
        "no finding may be filed under the provider kind: {items:?}"
    );
}

#[test]
fn two_bundles_repos_do_not_merge_under_one_label() {
    let e = Env::new();
    e.repo_only_bundle("proton-vpn", "protonvpn");
    e.repo_only_bundle("rpm-layered", "brave");
    e.write(
        "temper.toml",
        "[[machine]]\nname = \"m\"\nos = \"linux\"\napps = [\"proton-vpn\", \"rpm-layered\"]\n",
    );

    let items = e.drift();
    assert_eq!(app_of(&items, "protonvpn"), "proton-vpn");
    assert_eq!(app_of(&items, "brave"), "rpm-layered");
}

#[test]
fn a_repo_the_machine_declares_itself_answers_to_no_bundle() {
    let e = Env::new();
    e.write(
        "assets/rpm-repos/vendor.repo",
        "[vendor]\nname=Vendor\nbaseurl=https://example.invalid/rpm\n",
    );
    e.write(
        "temper.toml",
        "[[machine]]\nname = \"m\"\nos = \"linux\"\n\
         rpm_repos = [{ repo = \"assets/rpm-repos/vendor.repo\" }]\n",
    );

    let items = e.drift();
    // No bundle declared it, so the provider label is the honest group — and
    // the machine's name is not an app, so it must not be borrowed as one.
    assert_eq!(app_of(&items, "vendor"), "rpm-repo");
    assert!(
        !items.iter().any(|f| f["app"] == "m"),
        "a machine is not an app: {items:?}"
    );
}

/// A bundle gated away declares nothing here — including its repos.
#[test]
fn a_gated_bundles_repo_is_not_attributed_to_it_because_it_is_not_declared() {
    let e = Env::new();
    e.write(
        "assets/rpm-repos/macos-only.repo",
        "[macos-only]\nname=nope\nbaseurl=https://example.invalid/rpm\n",
    );
    e.write(
        "apps/mac-thing.toml",
        "os = \"mac\"\nrpm_repos = [{ repo = \"assets/rpm-repos/macos-only.repo\" }]\n",
    );
    e.write(
        "temper.toml",
        "[[machine]]\nname = \"m\"\nos = \"linux\"\napps = [\"mac-thing\"]\n",
    );

    let items = e.drift();
    assert!(
        !items.iter().any(|f| f["kind"] == "rpm-repo"),
        "a gated-away bundle contributes no repos: {items:?}"
    );
}
