//! A repo must be on disk before the package that resolves from it, and must
//! outlive the package on the way out.
//!
//! This is the whole point of `rpm_repos` being a category rather than a
//! `sysfile` step, so it is the thing a test has to pin. A `.repo` file could
//! only ever be expressed as config, and config runs in phase 2 while packages
//! converge in phase 1 — so on a machine without the repo, `rpm-ostree install`
//! resolved against a directory the repo had not been written to yet. Every
//! package from that repo failed, and because a failed batch is retried per
//! item, failed once each: a wall of expected failures with any genuine one
//! buried in it, and a clean machine that took two converges and two staged
//! deployments to reach a state one should have reached.
//!
//! Layering still stages a deployment and still wants a reboot — that is
//! rpm-ostree's model, not something to design around. What is asserted here is
//! that it takes ONE converge to get there.
//!
//! The mirror direction is asserted too, because getting only the first half
//! right is the defect AGENTS.md question 1 exists to catch. `rpm-ostree
//! uninstall` recomposes a deployment, which re-resolves every package still
//! layered, so `prune` must un-layer BEFORE deleting a repo file — otherwise it
//! strands a package nobody asked it to touch.
//!
//! Every tool is faked and appends to ONE shared log, because the assertion is
//! about *sequence* across tools and per-tool logs cannot express that.

use std::fs;

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
        let e = Env {
            home: TempDir::new().unwrap(),
            fake_home: TempDir::new().unwrap(),
            state: TempDir::new().unwrap(),
            bin: TempDir::new().unwrap(),
        };
        // `rpm` is what the repo provider gates on — rpm-ness, not atomic-ness,
        // so a plain dnf host would answer here too.
        e.stub("rpm", "");
        // A `sudo install …` of a repo file. Recorded, never executed: the
        // assertion is on the argv, and a test must not write to /etc.
        e.stub("sudo", "");
        e
    }

    /// An executable stub that appends `<name> $@` to one ordered log.
    fn stub(&self, name: &str, body: &str) {
        let log = self.fake_home.path().join("order.log");
        let p = self.bin.path().join(name);
        fs::write(
            &p,
            format!(
                "#!/bin/sh\necho \"{name} $@\" >> {}\n{body}\nexit 0\n",
                log.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// `rpm-ostree`, answering `status --json` with the shape the real tool
    /// emits and logging every other subcommand.
    ///
    /// `requested` is what the machine already has layered, so an empty list is
    /// the clean-machine case this file is about.
    fn stub_rpm_ostree(&self, requested: &[&str]) {
        let pkgs = requested
            .iter()
            .map(|p| format!("\"{p}\""))
            .collect::<Vec<_>>()
            .join(",");
        let json = format!(
            "{{\"deployments\":[{{\"booted\":true,\"requested-packages\":[{pkgs}]}}]}}"
        );
        // `status --json` must not pollute the order log: it is a read, and the
        // log is about writes. Everything else is recorded.
        self.stub(
            "rpm-ostree",
            &format!(
                "if [ \"$1\" = status ]; then echo '{json}'; fi\n"
            ),
        );
        // Re-write the stub so `status` short-circuits BEFORE the log line.
        let log = self.fake_home.path().join("order.log");
        let p = self.bin.path().join("rpm-ostree");
        fs::write(
            &p,
            format!(
                "#!/bin/sh\nif [ \"$1\" = status ]; then echo '{json}'; exit 0; fi\n\
                 echo \"rpm-ostree $@\" >> {}\nexit 0\n",
                log.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn order(&self) -> String {
        fs::read_to_string(self.fake_home.path().join("order.log")).unwrap_or_default()
    }

    fn asset(&self, rel: &str, body: &str) {
        let p = self.home.path().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
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

/// Index of the first line mentioning `needle`, for sequence assertions.
fn at(log: &str, needle: &str) -> usize {
    log.lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("`{needle}` never ran.\n--- order log ---\n{log}"))
}

fn spec_with_repo(e: &Env) {
    e.asset(
        "assets/rpm-repos/vendor.repo",
        "[vendor]\nname=Vendor\nbaseurl=https://example.invalid/rpm\ngpgcheck=1\ngpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-vendor\n",
    );
    e.asset("assets/rpm-repos/RPM-GPG-KEY-vendor", "not a real key\n");
    e.spec(
        "[[machine]]\nname = \"m\"\nos = \"linux\"\n\
         rpm_ostree = [\"vendor-app\"]\n\
         rpm_repos = [{ repo = \"assets/rpm-repos/vendor.repo\", key = \"assets/rpm-repos/RPM-GPG-KEY-vendor\" }]\n",
    );
}

/// The headline guarantee: ONE converge, with the repo written first.
#[cfg(target_os = "linux")]
#[test]
fn a_clean_machine_layers_from_a_declared_repo_in_one_pass() {
    let e = Env::new();
    e.stub_rpm_ostree(&[]); // nothing layered yet — a clean machine
    spec_with_repo(&e);

    e.temper().args(["install", "--yes"]).assert().success();

    let log = e.order();
    // The key before the repo that cites it: a `gpgkey=file://…` reference is
    // checked when the repo is first consulted, so a key arriving later is the
    // same ordering bug one level down.
    assert!(
        at(&log, "RPM-GPG-KEY-vendor") < at(&log, "yum.repos.d/vendor.repo"),
        "the GPG key must be installed before the repo that cites it.\n{log}"
    );
    // …and both before the package that resolves from them. This is the
    // assertion the whole category exists for.
    assert!(
        at(&log, "yum.repos.d/vendor.repo") < at(&log, "rpm-ostree install"),
        "the repo must be on disk before `rpm-ostree install` resolves against it.\n{log}"
    );
    // One converge means ONE layering call, so one staged deployment and one
    // reboot — not two.
    assert_eq!(
        log.lines().filter(|l| l.contains("rpm-ostree install")).count(),
        1,
        "a single converge must stage a single deployment.\n{log}"
    );
    // And the destinations are the inferred ones, root-owned.
    assert!(
        log.contains("/etc/yum.repos.d/vendor.repo")
            && log.contains("/etc/pki/rpm-gpg/RPM-GPG-KEY-vendor"),
        "repo and key must land in dnf's directories.\n{log}"
    );
    assert!(
        log.contains("-o root") && log.contains("0644"),
        "a repo dnf reads as root must be installed root-owned 0644.\n{log}"
    );
}

/// The mirror: `prune` un-layers before it deletes the repo.
///
/// Un-layering recomposes a deployment and re-resolves everything still
/// layered, so a repo deleted first can strand a package nobody asked to
/// remove. Asserted on sequence, because both operations succeed either way and
/// only the order distinguishes them.
#[cfg(target_os = "linux")]
#[test]
fn prune_unlayers_before_it_removes_the_repo_that_provided_it() {
    let e = Env::new();
    // Two packages layered. `keeper` stays declared — which is what keeps the
    // category probed at all, since temper only enumerates a manager whose
    // packages the spec mentions — while `vendor-app` becomes an extra.
    e.stub_rpm_ostree(&["keeper", "vendor-app"]);
    e.asset("assets/rpm-repos/vendor.repo", "[vendor]\nname=Vendor\n");
    // The repo is declared for the install, so the ledger records that temper
    // wrote it…
    e.spec(
        "[[machine]]\nname = \"m\"\nos = \"linux\"\n\
         rpm_ostree = [\"keeper\", \"vendor-app\"]\n\
         rpm_repos = [{ repo = \"assets/rpm-repos/vendor.repo\" }]\n",
    );
    e.temper().args(["install", "--yes"]).assert().success();

    // …and then the spec drops both the package and the repo it came from. The
    // package is now an extra and the file is now residue — only what temper
    // deployed is ever a candidate for removal.
    e.spec("[[machine]]\nname = \"m\"\nos = \"linux\"\nrpm_ostree = [\"keeper\"]\n");
    e.temper().args(["prune", "--yes"]).assert().success();

    let log = e.order();
    let unlayer = at(&log, "rpm-ostree uninstall");
    // `prune` may report the file rather than remove it — the ledger refuses to
    // delete what it cannot prove it wrote — but whichever it does, the
    // un-layer has to have happened first.
    if let Some(rm) = log.lines().position(|l| l.contains("vendor.repo") && l.contains("rm")) {
        assert!(
            unlayer < rm,
            "prune must un-layer before removing the repo the package resolved from.\n{log}"
        );
    }
}

/// A failed un-layer stages nothing, so it must not claim a reboot.
///
/// Found while testing the ordering above, and the mirror of a defect the
/// layering side had already fixed: `rpm-ostree uninstall`'s exit status was
/// discarded and success returned unconditionally, so a failed un-layer was
/// counted as items removed and asked the user to reboot into a deployment that
/// was never staged.
///
/// It matters to the repo ordering because that is what the retention guard
/// keys off: if the un-layer silently "succeeded", `prune` would go on to
/// delete the repo a still-layered package needs to re-resolve. Making the
/// status honest is what makes the guard possible at all.
///
/// A `copy` stands in for the repo, because it deploys without root and so
/// really exists. Note it is still removed here, deliberately: the guard is
/// scoped to repos, since letting one failed un-layer block every file cleanup
/// would trade this bug for a broader one.
#[cfg(target_os = "linux")]
#[test]
fn a_failed_unlayer_neither_stages_nor_claims_a_reboot() {
    let e = Env::new();
    e.stub_rpm_ostree(&["keeper", "vendor-app"]);
    // `uninstall` fails; `status` still answers, so the extra is still found.
    let log = e.fake_home.path().join("order.log");
    let p = e.bin.path().join("rpm-ostree");
    fs::write(
        &p,
        format!(
            "#!/bin/sh
             if [ \"$1\" = status ]; then                echo '{{\"deployments\":[{{\"booted\":true,\"requested-packages\":[\"keeper\",\"vendor-app\"]}}]}}'; exit 0; fi
             echo \"rpm-ostree $@\" >> {}
             if [ \"$1\" = uninstall ]; then exit 1; fi
             exit 0
",
            log.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }

    e.asset("assets/vendor.conf", "deployed by temper\n");
    let dest = e.fake_home.path().join("vendor.conf");
    e.spec(
        "[[machine]]\nname = \"m\"\nos = \"linux\"\napps = [\"a\"]\n\
         rpm_ostree = [\"keeper\", \"vendor-app\"]\n",
    );
    fs::create_dir_all(e.home.path().join("apps")).unwrap();
    fs::write(
        e.home.path().join("apps/a.toml"),
        format!(
            "[[step]]\ncopy = \"assets/vendor.conf\"\nto = \"{}\"\n",
            dest.display()
        ),
    )
    .unwrap();
    e.temper().args(["install", "--yes"]).assert().success();
    assert!(dest.exists(), "the copy step should have deployed the file");

    // Drop the step (making the file residue) and the package (making it an
    // extra), then prune. The un-layer fails.
    fs::write(e.home.path().join("apps/a.toml"), "").unwrap();
    e.spec("[[machine]]\nname = \"m\"\nos = \"linux\"\napps = [\"a\"]\nrpm_ostree = [\"keeper\"]\n");
    let out = e.temper().args(["prune", "--yes"]).output().unwrap();

    let order = e.order();
    let said = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
    assert!(
        order.contains("rpm-ostree uninstall"),
        "the un-layer must have been attempted.\n{order}"
    );
    // The failure has to reach the user. Reporting "removed" and "reboot
    // required" for a deployment that was never staged is the defect this
    // half fixes.
    assert!(
        !said.contains("reboot required"),
        "a failed un-layer stages no deployment, so it must not ask for a reboot.\n{said}"
    );
}

/// A repo the IMAGE provides is never reported and never touched.
///
/// This is why there is no `[ignore].rpm_repo` list: only repos temper wrote
/// are candidates, so `/etc/yum.repos.d` full of fedora, updates and rpmfusion
/// stays silent on its own. An ignore list here would be maintenance for a
/// report that should never have existed.
#[cfg(target_os = "linux")]
#[test]
fn an_undeclared_repo_is_the_images_and_stays_out_of_the_report() {
    let e = Env::new();
    e.stub_rpm_ostree(&[]);
    e.spec("[[machine]]\nname = \"m\"\nos = \"linux\"\n");

    let out = e.temper().args(["drift", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let findings = v["items"].as_array().cloned().unwrap_or_default();
    assert!(
        !findings.iter().any(|f| f["kind"] == "rpm-repo"),
        "a spec declaring no repos must report none: {findings:?}"
    );
}

/// A declared repo that is absent is drift, and it names the file, not the id.
#[cfg(target_os = "linux")]
#[test]
fn a_declared_repo_that_is_absent_is_reported_as_drift() {
    let e = Env::new();
    e.stub_rpm_ostree(&[]);
    spec_with_repo(&e);

    let out = e.temper().args(["drift", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let findings = v["items"].as_array().cloned().unwrap_or_default();
    let repo: Vec<_> = findings.iter().filter(|f| f["kind"] == "rpm-repo").collect();
    assert!(
        repo.iter().any(|f| f["target"]
            .as_str()
            .unwrap_or_default()
            .contains("/etc/yum.repos.d/vendor.repo")),
        "a declared repo that is not on disk must be reported: {findings:?}"
    );
}

/// Writing a repo cannot be undone, and the user hears so from the run itself.
///
/// A repo is root-owned state outside the journal, so `undo` does not take it
/// back — `interface.rs` scores it `revertible: No` in as many words. AGENTS.md
/// question 7 is about *when* the user learns that: a run reporting success
/// while `undo` would revert less than it appeared to promise is the failure,
/// and it is only avoided if the warning arrives with the run.
///
/// The mirror case — staying quiet on a machine whose repos are already in
/// place, since warning about work that will not happen is its own defect — is
/// keyed on `rpm_repos_pending` and not covered here: the stubbed `sudo` never
/// really writes, so an in-sync repo is not reachable in this fixture.
#[cfg(target_os = "linux")]
#[test]
fn writing_a_repo_says_up_front_that_undo_will_not_take_it_back() {
    let e = Env::new();
    e.stub_rpm_ostree(&[]);
    spec_with_repo(&e);

    let out = e.temper().args(["install", "--yes"]).output().unwrap();
    let said = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("rpm-repo"),
        "the run must name the repo write as something undo cannot revert.\n{said}"
    );
}

/// A spec declaring no repos costs nothing — no `rpm` probe, no root ask.
#[test]
fn declaring_no_repos_touches_nothing() {
    let e = Env::new();
    e.spec(&format!(
        "[[machine]]\nname = \"m\"\nos = \"{}\"\n",
        if cfg!(target_os = "macos") { "mac" } else { "linux" }
    ));
    e.temper().arg("drift").assert().success();
    assert!(
        !e.order().contains("sudo"),
        "an empty `rpm_repos` must not escalate: {}",
        e.order()
    );
}
