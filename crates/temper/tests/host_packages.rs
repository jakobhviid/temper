//! The host package managers — dnf, apt, pacman — end to end against fakes.
//!
//! Each fake keeps a real installed set and a real asked-for set on disk, so
//! the assertions are about what the machine ends up holding rather than about
//! which commands were typed. Writes are logged in order; reads are not, because
//! the log is about what changed the machine.
//!
//! What is pinned here is the part a stub can honestly answer: the baseline is
//! recorded before the first install, so the image is never an extra; prune
//! removes exactly what it previewed and holds back a package something else
//! depends on; undo takes back what install added; and a manager that is not
//! this host's — or that cannot install here — never pretends otherwise. That
//! the real managers behave like the fakes is the Erebus matrix's job.

use std::fs;
use std::path::PathBuf;

use assert_cmd::Command;
use tempfile::TempDir;

struct Env {
    home: TempDir,
    fake_home: TempDir,
    state: TempDir,
    bin: TempDir,
    pm: TempDir,
}

impl Env {
    fn new() -> Env {
        let e = Env {
            home: TempDir::new().unwrap(),
            fake_home: TempDir::new().unwrap(),
            state: TempDir::new().unwrap(),
            bin: TempDir::new().unwrap(),
            pm: TempDir::new().unwrap(),
        };
        // Escalation is real in production and pass-through here: the fake
        // manager behind it is what records the effect. `-n -v` / `-v` are the
        // credential checks, answered "held".
        // A root-owned file write (`sudo install …`) is recorded, never run: the
        // assertion is on its order, and a test must not write to /etc.
        e.script(
            "sudo",
            "case \"$1\" in -n|-v) exit 0;; install) echo \"sudo $*\" >> \"$LOG\"; \
             [ \"$2\" = -d ] && exit 0; src=$(eval echo \\\"\\${$(($# - 1))}\\\"); \
             [ -e \"$src\" ] || exit 1; exit 0;; esac\nexec \"$@\"\n",
        );
        e
    }

    fn script(&self, name: &str, body: &str) {
        let p = self.bin.path().join(name);
        let prelude = format!(
            "#!/bin/sh\nS={s}\nLOG={log}\n\
             add() {{ for p in \"$@\"; do \
               grep -qx -e \"$p\" \"$S/installed\" 2>/dev/null || echo \"$p\" >> \"$S/installed\"; \
               grep -qx -e \"$p\" \"$S/explicit\" 2>/dev/null || echo \"$p\" >> \"$S/explicit\"; done; }}\n\
             del() {{ for p in \"$@\"; do case \"$p\" in -*) continue;; esac; for f in installed explicit; do \
               grep -vx -e \"$p\" \"$S/$f\" > \"$S/t\"; mv \"$S/t\" \"$S/$f\"; done; done; }}\n\
             needed() {{ grep -qx \"$1\" \"$S/needed\" 2>/dev/null; }}\n",
            s = self.pm.path().display(),
            log = self.log_path().display(),
        );
        fs::write(&p, format!("{prelude}{body}")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// The machine as it stands: what is installed, what of that was asked for
    /// by name, and what another installed package depends on.
    fn machine(&self, installed: &[&str], explicit: &[&str], needed: &[&str]) {
        let w = |f: &str, xs: &[&str]| {
            let body: String = xs.iter().map(|x| format!("{x}\n")).collect();
            fs::write(self.pm.path().join(f), body).unwrap();
        };
        w("installed", installed);
        w("explicit", explicit);
        w("needed", needed);
    }

    /// A package installed by hand, outside temper.
    fn hand_install(&self, pkg: &str) {
        for f in ["installed", "explicit"] {
            let p = self.pm.path().join(f);
            let mut body = fs::read_to_string(&p).unwrap_or_default();
            body.push_str(&format!("{pkg}\n"));
            fs::write(p, body).unwrap();
        }
    }

    fn installed(&self) -> Vec<String> {
        fs::read_to_string(self.pm.path().join("installed"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    fn dnf(&self) {
        self.script(
            "rpm",
            "case \"$1\" in\n\
               -qa) cat \"$S/installed\"; exit 0;;\n\
               -e) shift; shift; for p in \"$@\"; do if needed \"$p\"; then \
                   echo \"$p is needed by (installed) other-1.0-1.x86_64\" >&2; exit 1; fi; done; exit 0;;\n\
             esac\nexit 0\n",
        );
        self.script(
            "dnf",
            "case \"$*\" in *repoquery*) cat \"$S/explicit\"; exit 0;; esac\n\
             echo \"dnf $*\" >> \"$LOG\"\n\
             case \"$1\" in\n\
               install) shift; shift; add \"$@\";;\n\
               remove) shift; shift; shift; del \"$@\";;\n\
             esac\nexit 0\n",
        );
    }

    fn apt(&self) {
        self.script(
            "dpkg-query",
            "while read -r p; do printf '%s\\tii \\n' \"$p\"; done < \"$S/installed\"\n",
        );
        self.script("apt-mark", "cat \"$S/explicit\"\n");
        self.script("dpkg", "echo amd64\n");
        self.script(
            "apt-get",
            "if [ \"$1\" = -s ]; then shift; shift; for p in \"$@\"; do echo \"Remv $p [1.0]\"; \
               if needed \"$p\"; then echo \"Remv needs-$p [1.0]\"; fi; done; exit 0; fi\n\
             echo \"apt-get $*\" >> \"$LOG\"\n\
             case \"$1\" in\n\
               install|remove) verb=$1; shift; shift; shift; shift; \
                 if [ \"$verb\" = install ]; then add \"$@\"; else del \"$@\"; fi;;\n\
             esac\nexit 0\n",
        );
    }

    /// pacman, with groups: a line `grp a b` in `$S/groups` makes `grp` a name
    /// that installs `a` and `b` and is never itself a package.
    fn pacman(&self) {
        self.script(
            "pacman",
            "expand() { for p in \"$@\"; do m=$(grep \"^$p \" \"$S/groups\" 2>/dev/null | cut -d' ' -f2-); \
               if [ -n \"$m\" ]; then echo $m; else echo \"$p\"; fi; done; }\n\
             case \"$1\" in\n\
               -Qq) cat \"$S/installed\"; exit 0;;\n\
               -Qqe) cat \"$S/explicit\"; exit 0;;\n\
               -Rp) shift; shift; shift; for p in \"$@\"; do if needed \"$p\"; then \
                 echo 'error: failed to prepare transaction (could not satisfy dependencies)' >&2; \
                 exit 1; fi; done; for t in $(expand \"$@\"); do echo \"$t\"; done; exit 0;;\n\
             esac\n\
             echo \"pacman $*\" >> \"$LOG\"\n\
             case \"$1\" in\n\
               -S) shift; shift; shift; add $(expand \"$@\");;\n\
               -R) shift; shift; del \"$@\";;\n\
             esac\nexit 0\n",
        );
    }

    /// A PATH holding the fakes and only the shell tools they use — nothing
    /// from the host's own package managers.
    fn isolated_path(&self) -> String {
        let tools = self.fake_home.path().join("tools");
        fs::create_dir_all(&tools).unwrap();
        for t in ["sh", "cat", "grep", "mv", "printf", "env"] {
            let real = ["/usr/bin", "/bin"]
                .iter()
                .map(|d| PathBuf::from(d).join(t))
                .find(|p| p.exists())
                .unwrap_or_else(|| panic!("no {t} on this host"));
            #[cfg(unix)]
            std::os::unix::fs::symlink(real, tools.join(t)).unwrap();
        }
        format!("{}:{}", self.bin.path().display(), tools.display())
    }

    fn log_path(&self) -> PathBuf {
        self.fake_home.path().join("writes.log")
    }

    fn log(&self) -> String {
        fs::read_to_string(self.log_path()).unwrap_or_default()
    }

    fn spec(&self, body: &str) {
        fs::write(self.home.path().join("temper.toml"), body).unwrap();
    }

    fn asset(&self, rel: &str, body: &str) {
        let p = self.home.path().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    fn bundle(&self, name: &str, body: &str) {
        let p = self.home.path().join("apps").join(format!("{name}.toml"));
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    fn baseline(&self) -> serde_json::Value {
        let p = self.state.path().join("baseline/m.json");
        serde_json::from_str(&fs::read_to_string(p).unwrap_or_else(|_| "null".into())).unwrap()
    }

    fn temper(&self) -> Command {
        let mut c = Command::cargo_bin("temper").unwrap();
        c.env("TEMPER_DIR", self.home.path())
            .env("HOME", self.fake_home.path())
            .env("XDG_CONFIG_HOME", self.fake_home.path().join(".config"))
            .env_remove("DCONF_PROFILE")
            .env("TEMPER_STATE_DIR", self.state.path())
            // A mutable host, whatever the machine running the suite is.
            .env("TEMPER_OSTREE_BOOTED", "0")
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.path().display()));
        c
    }

    fn drift_json(&self) -> serde_json::Value {
        let out = self.temper().args(["--json", "drift"]).output().unwrap();
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "drift --json is not one JSON document ({e}):\n{}",
                String::from_utf8_lossy(&out.stdout)
            )
        })
    }
}

/// `(kind, target, ok)` for every drift finding of the given kinds.
fn findings(doc: &serde_json::Value, kinds: &[&str]) -> Vec<(String, String, bool)> {
    let items = doc["items"]
        .as_array()
        .unwrap_or_else(|| panic!("no items array in drift --json:\n{doc}"));
    items
        .iter()
        .filter(|f| kinds.contains(&f["kind"].as_str().unwrap_or_default()))
        .map(|f| {
            (
                f["kind"].as_str().unwrap().to_string(),
                f["target"].as_str().unwrap().to_string(),
                f["ok"].as_bool().unwrap_or(false),
            )
        })
        .collect()
}

const MACHINE: &str = "[[machine]]\nname = \"m\"\nos = \"linux\"\n";

/// The whole loop on dnf: install what is missing, record the image first,
/// report only what is installed afterwards, and prune exactly that.
#[cfg(target_os = "linux")]
#[test]
fn dnf_counts_extras_from_the_baseline_and_prunes_only_those() {
    let e = Env::new();
    e.dnf();
    // A stock image: its base counts as asked-for, which is the whole problem.
    e.machine(&["bash", "vim-minimal", "glibc"], &["bash", "vim-minimal"], &[]);
    e.spec(&format!("{MACHINE}dnf = [\"jq\", \"vim-minimal\"]\n"));

    // Before any converge there is no baseline, so nothing is an extra — and
    // drift says why rather than reporting the base system.
    let before = e.drift_json();
    assert!(
        findings(&before, &["dnf-extra"])
            .iter()
            .all(|(_, t, ok)| *ok && !t.contains("bash")),
        "with no baseline the image must not be reported as extras: {before}"
    );

    e.temper().args(["install", "--yes"]).assert().success();
    let log = e.log();
    assert!(log.contains("dnf install -y jq"), "jq is missing and must be installed:\n{log}");
    assert!(!log.contains("vim-minimal"), "an installed package is not reinstalled:\n{log}");
    // The baseline is what was asked for BEFORE the install — never what temper added.
    let base = e.baseline();
    let base: Vec<&str> = base["dnf"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(base, vec!["bash", "vim-minimal"]);

    // Converged: nothing missing, nothing extra.
    let after = e.drift_json();
    assert!(
        findings(&after, &["dnf", "dnf-extra"]).iter().all(|(_, _, ok)| *ok),
        "a converged machine drifts on nothing: {after}"
    );

    // Someone installs a package by hand. It is reported; the base still is not.
    e.hand_install("cowsay");
    let hand = e.drift_json();
    let extras: Vec<String> = findings(&hand, &["dnf-extra"])
        .into_iter()
        .filter(|(_, _, ok)| !ok)
        .map(|(_, t, _)| t)
        .collect();
    assert_eq!(extras, vec!["cowsay"], "only the hand install is an extra: {hand}");

    // prune previews it, removes it, and nothing else.
    let out = e.temper().args(["--json", "prune", "--yes"]).output().unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["dnf"], serde_json::json!(["cowsay"]), "{doc}");
    assert_eq!(doc["removed"], true);
    assert!(!e.installed().contains(&"cowsay".to_string()));
    for kept in ["bash", "vim-minimal", "glibc", "jq"] {
        assert!(e.installed().contains(&kept.to_string()), "prune removed {kept}");
    }
}

/// A removal that would take a dependant with it is held back, named, and not
/// counted — the preview is the effect.
#[cfg(target_os = "linux")]
#[test]
fn apt_prune_holds_back_a_package_another_depends_on() {
    let e = Env::new();
    e.apt();
    e.machine(&["apt", "bash"], &["apt", "bash"], &["libfoo"]);
    e.spec(&format!("{MACHINE}apt = [\"jq\"]\n"));
    e.temper().args(["install", "--yes"]).assert().success();
    assert!(e.log().contains("apt-get update"), "lists are refreshed before an install");

    e.hand_install("libfoo");
    e.hand_install("cowsay");
    let out = e.temper().args(["--json", "prune", "--yes"]).output().unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["apt"], serde_json::json!(["cowsay"]), "{doc}");
    let held = doc["unremovable"].as_array().unwrap();
    assert!(
        held.iter().any(|h| h["manager"] == "apt" && h["name"] == "libfoo"),
        "libfoo is needed by another package and must be reported, not removed: {doc}"
    );
    assert!(e.installed().contains(&"libfoo".to_string()));
    assert!(!e.installed().contains(&"cowsay".to_string()));
    assert!(
        !e.log().lines().any(|l| l.starts_with("apt-get remove") && l.contains("libfoo")),
        "a held-back package must never reach the removal:\n{}",
        e.log()
    );
}

/// `undo` takes back exactly what the install added.
#[cfg(target_os = "linux")]
#[test]
fn pacman_undo_removes_what_install_added() {
    let e = Env::new();
    e.pacman();
    e.machine(&["base", "sudo"], &["base", "sudo"], &[]);
    e.spec(&format!("{MACHINE}pacman = [\"jq\", \"tree\"]\n"));
    let out = e.temper().args(["install", "--yes"]).output().unwrap();
    assert!(out.status.success());
    assert!(e.log().contains("pacman -S --needed --noconfirm jq tree"), "{}", e.log());
    // The headline counts them: "0 package(s) declared" on the run that
    // installed two is the summary contradicting the machine.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("2 package(s) declared"), "{stdout}");
    assert!(e.installed().contains(&"jq".to_string()));

    e.temper().arg("undo").assert().success();
    // Exactly the named packages: `-R`, never `-Rs`, which would also take
    // dependencies the preview never named.
    assert!(e.log().contains("pacman -R --noconfirm jq tree"), "{}", e.log());
    assert!(!e.installed().contains(&"jq".to_string()));
    assert!(e.installed().contains(&"base".to_string()), "undo must not touch the base");
}

/// On an atomic host dnf reads fine and cannot install. That is reported as
/// unavailable, with the field that does work there, and nothing is run.
#[cfg(target_os = "linux")]
#[test]
fn an_atomic_host_reports_dnf_unavailable_and_installs_nothing() {
    let e = Env::new();
    e.dnf();
    e.machine(&["bash"], &["bash"], &[]);
    e.spec(&format!("{MACHINE}dnf = [\"jq\"]\n"));

    let out = e
        .temper()
        .env("TEMPER_OSTREE_BOOTED", "1")
        .args(["--json", "drift"])
        .output()
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let jq = findings(&doc, &["dnf"]);
    assert_eq!(jq.len(), 1, "{doc}");
    assert!(jq[0].2, "status-only, not drift — there is nothing install could do: {doc}");
    assert!(doc.to_string().contains("rpm_ostree"), "names the field that works here: {doc}");

    let out = e
        .temper()
        .env("TEMPER_OSTREE_BOOTED", "1")
        .args(["install", "--yes"])
        .output()
        .unwrap();
    assert!(!e.log().contains("dnf install"), "nothing may run: {}", e.log());
    // The run carries on, as with any provider that cannot finish, and says why.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("rpm_ostree"), "the warning names what works here:\n{stderr}");
}

/// A bundle's list for another family is inert, so one bundle serves a mixed
/// fleet. The machine's own list for a manager it lacks is a mistake, and said.
#[cfg(target_os = "linux")]
#[test]
fn a_list_for_another_family_is_inert_unless_the_machine_declares_it() {
    let e = Env::new();
    e.pacman();
    e.machine(&["base"], &["base"], &[]);
    e.bundle(
        "cli",
        "os = \"linux\"\ndnf = [\"fd-find\"]\napt = [\"fd-find\"]\npacman = [\"fd\"]\n",
    );
    e.spec(&format!("{MACHINE}apps = [\"cli\"]\n"));
    // The suite's PATH reaches the real /usr/bin, where a dnf or apt host has
    // those managers for real. This test needs a host with pacman alone, so its
    // PATH is the fakes plus the handful of tools they are written in.
    let only_pacman = e.isolated_path();
    let out = e
        .temper()
        .env("PATH", &only_pacman)
        .args(["--json", "drift"])
        .output()
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(findings(&doc, &["dnf", "dnf-extra", "apt", "apt-extra"]).is_empty(), "{doc}");
    assert_eq!(findings(&doc, &["pacman"]).len(), 1, "fd is missing here: {doc}");

    e.spec(&format!("{MACHINE}apps = [\"cli\"]\napt = [\"htop\"]\n"));
    let out = e
        .temper()
        .env("PATH", &only_pacman)
        .args(["--json", "drift"])
        .output()
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let apt = findings(&doc, &["apt"]);
    assert_eq!(apt.len(), 1, "{doc}");
    assert!(apt[0].1.contains("[[machine]].apt") && apt[0].2, "{doc}");
}

/// `reconcile` absorbs a hand install into the machine's own list, which is
/// the only scope one machine may write.
#[cfg(target_os = "linux")]
#[test]
fn reconcile_absorbs_a_hand_install_into_the_machine() {
    let e = Env::new();
    e.apt();
    e.machine(&["apt", "bash"], &["apt", "bash"], &[]);
    e.bundle("cli", "apt = [\"jq\"]\n");
    e.spec(&format!("{MACHINE}apps = [\"cli\"]\n"));
    e.temper().args(["install", "--yes"]).assert().success();
    e.hand_install("cowsay");

    e.temper()
        .args(["reconcile", "--current-state-wins", "--yes"])
        .assert()
        .success();
    let toml = fs::read_to_string(e.home.path().join("temper.toml")).unwrap();
    assert!(toml.contains("apt = [\"cowsay\"]"), "absorbed into [[machine]].apt:\n{toml}");
    let bundle = fs::read_to_string(e.home.path().join("apps/cli.toml")).unwrap();
    assert!(!bundle.contains("cowsay"), "a shared bundle is never written:\n{bundle}");
    assert!(!toml.contains("bash"), "the baseline is never absorbed:\n{toml}");
}

/// A name the manager accepts without installing a package of that name — a
/// pacman group — is not journaled; only what the run actually added is, so
/// `undo` cannot take a member that was there before.
#[cfg(target_os = "linux")]
#[test]
fn undo_takes_back_what_the_run_added_and_not_what_a_group_expands_to() {
    let e = Env::new();
    e.pacman();
    // `old` is a group member installed long before temper.
    e.machine(&["base", "old"], &["base", "old"], &[]);
    fs::write(e.pm.path().join("groups"), "fonts old new\n").unwrap();
    e.spec(&format!("{MACHINE}pacman = [\"fonts\"]\n"));

    let out = e.temper().args(["install", "--yes"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(e.installed().contains(&"new".to_string()));
    assert!(stderr.contains("fonts"), "a name that installed no package of that name is said:\n{stderr}");

    e.temper().arg("undo").assert().success();
    assert!(!e.installed().contains(&"new".to_string()), "the run added `new`; undo removes it");
    assert!(e.installed().contains(&"old".to_string()), "`old` predates the run and must stay");
    assert!(
        !e.log().lines().any(|l| l.starts_with("pacman -R") && l.contains("fonts")),
        "the group name must never reach a removal:\n{}",
        e.log()
    );
}

/// On an atomic host a bundle's `dnf` list is inert, like another family's —
/// one bundle serves a Silverblue box and a plain Fedora one — so neither drift
/// nor install says anything about it.
#[cfg(target_os = "linux")]
#[test]
fn a_bundles_dnf_list_is_inert_on_an_atomic_host() {
    let e = Env::new();
    e.dnf();
    e.machine(&["bash"], &["bash"], &[]);
    e.bundle("cli", "dnf = [\"jq\"]\n");
    e.spec(&format!("{MACHINE}apps = [\"cli\"]\n"));
    let out = e
        .temper()
        .env("TEMPER_OSTREE_BOOTED", "1")
        .args(["--json", "drift"])
        .output()
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(findings(&doc, &["dnf", "dnf-extra"]).is_empty(), "{doc}");
    let out = e
        .temper()
        .env("TEMPER_OSTREE_BOOTED", "1")
        .args(["install", "--yes"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!String::from_utf8_lossy(&out.stdout).contains("could not finish"));
    assert!(!e.log().contains("dnf install"));
}

/// An apt source lands before the install that resolves from it — key, then
/// source, then the list refresh, then the install — so a vendor package
/// installs on a clean machine's first run.
#[cfg(target_os = "linux")]
#[test]
fn an_apt_source_lands_key_first_and_before_the_install_that_needs_it() {
    let e = Env::new();
    e.apt();
    e.machine(&["apt"], &["apt"], &[]);
    e.asset(
        "assets/apt/vendor.sources",
        "Types: deb\nURIs: https://example.invalid/apt\nSuites: stable\nComponents: main\n\
         Signed-By: /etc/apt/keyrings/vendor.asc\n",
    );
    e.asset("assets/apt/vendor.asc", "not a real key\n");
    e.spec(&format!(
        "{MACHINE}apt = [\"vendor-tool\"]\n\
         apt_sources = [{{ source = \"assets/apt/vendor.sources\", key = \"assets/apt/vendor.asc\" }}]\n"
    ));
    let out = e.temper().args(["install", "--yes"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let log = e.log();
    let at = |needle: &str| {
        log.lines()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("`{needle}` never ran:\n{log}"))
    };
    assert!(at("/etc/apt/keyrings/vendor.asc") < at("/etc/apt/sources.list.d/vendor.sources"), "{log}");
    assert!(at("/etc/apt/sources.list.d/vendor.sources") < at("apt-get update"), "{log}");
    assert!(at("apt-get update") < at("apt-get install"), "{log}");
    // A source is root-owned state outside the journal, and the run says so.
    let said = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("apt-source"), "undo cannot take a source back, and must say so:\n{said}");
}

/// On a host without apt, a declared source is `unavailable` — never missing,
/// which would invite a converge to write it — and nothing is written.
#[cfg(target_os = "linux")]
#[test]
fn an_apt_source_on_a_host_without_apt_is_unavailable() {
    let e = Env::new();
    e.pacman();
    e.machine(&["base"], &["base"], &[]);
    e.asset("assets/apt/vendor.sources", "Types: deb\n");
    e.spec(&format!(
        "{MACHINE}apt_sources = [{{ source = \"assets/apt/vendor.sources\" }}]\n"
    ));
    let only_pacman = e.isolated_path();
    let out = e
        .temper()
        .env("PATH", &only_pacman)
        .args(["--json", "drift"])
        .output()
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let src: Vec<_> = doc["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["kind"] == "apt-source")
        .collect();
    assert_eq!(src.len(), 1, "{doc}");
    assert_eq!(src[0]["status"], "unavailable", "{doc}");
    e.temper()
        .env("PATH", &only_pacman)
        .args(["install", "--yes"])
        .assert()
        .success();
    assert!(!e.log().contains("sources.list.d"), "{}", e.log());
}

/// A source whose key could not be installed is not written: it would cite a
/// key that is not there, and every package from it would fail its signature
/// check instead of the source reporting itself missing.
#[cfg(target_os = "linux")]
#[test]
fn a_source_whose_key_failed_is_not_written() {
    let e = Env::new();
    e.apt();
    e.machine(&["apt"], &["apt"], &[]);
    // The key asset is missing — a partial sync, a renamed file.
    e.asset("assets/apt/vendor.sources", "Types: deb\nSigned-By: /etc/apt/keyrings/vendor.asc\n");
    e.spec(&format!(
        "{MACHINE}apt_sources = [{{ source = \"assets/apt/vendor.sources\", key = \"assets/apt/vendor.asc\" }}]\n"
    ));
    let out = e.temper().args(["install", "--yes"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !e.log().contains("sources.list.d/vendor.sources"),
        "the source must not be written without its key:\n{}",
        e.log()
    );
    assert!(stderr.contains("cites that key"), "{stderr}");
}
