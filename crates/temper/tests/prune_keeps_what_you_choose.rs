//! `prune` removes what you choose and leaves the rest in place, for this run.
//!
//! The choice is made per item — `c` at the confirm, or `--keep` — and the
//! half that matters is the executor, not the preview: `brew bundle cleanup`
//! decides for itself what to remove, so a kept formula the file it reads does
//! not name is uninstalled anyway, outside the plan the user confirmed.
//!
//! brew is faked so this measures temper rather than the developer's machine,
//! and so the destructive call can be observed instead of performed.

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

    fn cleanup_log(&self) -> std::path::PathBuf {
        self.fake_home.path().join("cleanup-args")
    }
    fn brewfile_seen(&self) -> std::path::PathBuf {
        self.fake_home.path().join("cleanup-brewfile")
    }

    /// A `brew` with one declared formula and two orphans, that records what
    /// `bundle cleanup --force` was handed instead of removing anything.
    fn fake_brew(&self) {
        let bin = self.bin.path().join("brew");
        fs::write(
            &bin,
            format!(
                r#"#!/bin/sh
case "$1 $2" in
  "list --formula") echo keeper; echo trying-out; echo done-with ;;
  "trust --json")   echo '{{"taps":[]}}' ;;
  "bundle cleanup")
      case " $* " in
        *" --force "*)
            echo "$@" > {log}
            for a in "$@"; do [ -f "$a" ] && cp "$a" {seen}; done
            ;;
        *)  # The dry listing exits non-zero when it finds orphans, as brew does.
            echo "Would uninstall formulae:"
            echo "trying-out"
            echo "done-with"
            exit 1
            ;;
      esac
      ;;
  *) : ;;
esac
exit 0
"#,
                log = self.cleanup_log().display(),
                seen = self.brewfile_seen().display(),
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn spec(&self, extra: &str) {
        fs::write(
            self.home.path().join("temper.toml"),
            format!(
                "[[machine]]\nname = \"t\"\nos = \"{}\"\npackages = [\"brew \\\"keeper\\\"\"]\n{extra}",
                os()
            ),
        )
        .unwrap();
    }

    fn temper(&self) -> Command {
        let mut c = Command::cargo_bin("temper").unwrap();
        c.env("TEMPER_DIR", self.home.path())
            .env("HOME", self.fake_home.path())
            .env("TEMPER_STATE_DIR", self.state.path())
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.path().display()));
        c
    }

    /// The cleanup file's formula lines — what brew was told may stay.
    fn handed(&self) -> Vec<String> {
        read(&self.brewfile_seen())
            .lines()
            .filter_map(|l| l.strip_prefix("brew \"").and_then(|r| r.split('"').next()))
            .map(String::from)
            .collect()
    }
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_default()
}

fn extras(v: &serde_json::Value) -> Vec<&str> {
    v["extras"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect()
}

/// `--keep` takes the formula out of the plan, says so in `--json`, and hands
/// it to brew as one that stays.
#[test]
fn a_kept_formula_is_left_installed() {
    let e = Env::new();
    e.fake_brew();
    e.spec("");

    let out = e.temper().args(["prune", "--json", "--keep", "trying-out"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(extras(&v), vec!["done-with"], "{v}");
    assert_eq!(
        v["kept"],
        serde_json::json!([{ "list": "packages", "item": "brew trying-out", "why": null }]),
        "{v}"
    );

    e.temper().args(["prune", "--yes", "--keep", "trying-out"]).assert().success();
    let handed = e.handed();
    assert!(
        handed.contains(&"trying-out".to_string()),
        "the kept formula was handed to brew as an orphan — cleanup would \
         uninstall it. Formulae named: {handed:?}"
    );
    assert!(
        !handed.contains(&"done-with".to_string()),
        "the formula nobody kept was protected too: {handed:?}"
    );
}

/// `c` asks about each item; Enter keeps it. The answers are the confirm: a plan
/// that only lost what was kept runs without a second question, and the summary
/// says what keeping means next time.
#[test]
fn choosing_per_item_prunes_only_what_you_said_yes_to() {
    let e = Env::new();
    e.fake_brew();
    e.spec("");

    // c to choose; Enter keeps trying-out; y prunes done-with. Nothing else.
    let out = e
        .temper()
        .arg("prune")
        .write_stdin("c\n\ny\n")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("uninstall brew trying-out?"), "{stdout}");
    assert_eq!(
        stdout.matches("prune the ").count(),
        1,
        "the walk's answers were asked for again: {stdout}"
    );
    assert!(!stdout.contains("changes the rest of the plan"), "nothing changed: {stdout}");
    // The fake still lists what cleanup was told to remove, so the count reads
    // it back as not removed; what is asserted here is what was kept.
    assert!(stdout.contains(", 1 left in place"), "{stdout}");
    assert!(
        stdout.contains("brew trying-out stays this run")
            && stdout.contains("the next prune asks again")
            && stdout.contains("`temper reconcile` declares it"),
        "the summary must say what keeping means and how to settle it: {stdout}"
    );

    let handed = e.handed();
    assert!(handed.contains(&"trying-out".to_string()), "{handed:?}");
    assert!(!handed.contains(&"done-with".to_string()), "{handed:?}");
}

/// A flatpak is listed by the name it shows the user, not only its id.
#[test]
fn a_flatpak_is_named_as_the_user_knows_it() {
    let e = Env::new();
    let bin = e.bin.path().join("flatpak");
    fs::write(
        &bin,
        "#!/bin/sh\n\
         case \"$*\" in\n\
           *columns=application,name*) printf 'org.declared.App\\tDeclared\\ncom.github.tchx84.Flatseal\\tFlatseal\\n' ;;\n\
           *columns=application,installation*) printf 'org.declared.App\\tsystem\\ncom.github.tchx84.Flatseal\\tsystem\\n' ;;\n\
           *columns=application*) printf 'org.declared.App\\ncom.github.tchx84.Flatseal\\n' ;;\n\
         esac\n\
         exit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(
        e.home.path().join("temper.toml"),
        format!(
            "[[machine]]\nname = \"t\"\nos = \"{}\"\npackages = [\"flatpak \\\"org.declared.App\\\"\"]\n",
            os()
        ),
    )
    .unwrap();

    let out = e.temper().args(["prune", "--dry-run"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("flatpak \"Flatseal\" (com.github.tchx84.Flatseal)"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Keeping everything is a prune with nothing to do, and brew is not called.
#[test]
fn keeping_every_item_removes_nothing() {
    let e = Env::new();
    e.fake_brew();
    e.spec("");

    let out = e.temper().arg("prune").write_stdin("c\n\n\n").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("nothing to remove — 2 item(s) left in place"), "{stdout}");
    assert!(stdout.contains("stay this run"), "{stdout}");
    assert!(read(&e.cleanup_log()).is_empty(), "cleanup ran: {}", read(&e.cleanup_log()));
}

/// A `--keep` that matches nothing stops the run: `--keep tryingout --yes`
/// would otherwise remove the formula it was written to protect.
#[test]
fn a_keep_that_matches_nothing_removes_nothing() {
    let e = Env::new();
    e.fake_brew();
    e.spec("");

    let out = e
        .temper()
        .args(["prune", "--yes", "--keep", "tryingout"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("names nothing this prune would act on"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(read(&e.cleanup_log()).is_empty(), "cleanup ran: {}", read(&e.cleanup_log()));
}

/// Files are choices too. A retired path has no ignore list, so the plan takes
/// it out directly — by the path as the spec writes it, or as the shell
/// expanded it.
#[test]
fn a_kept_path_stays() {
    let e = Env::new();
    e.fake_brew();
    e.spec("retire = [\"~/.config/old-a\", \"~/.config/old-b\", \"~/.config/old-c\"]\n");
    let dir = e.fake_home.path().join(".config");
    for f in ["old-a", "old-b", "old-c"] {
        fs::create_dir_all(dir.join(f)).unwrap();
    }

    let expanded = dir.join("old-b").display().to_string();
    e.temper()
        .args(["prune", "--yes", "--keep", "~/.config/old-a", "--keep", &expanded])
        .args(["--keep", "trying-out", "--keep", "done-with"])
        .assert()
        .success();
    assert!(dir.join("old-a").exists(), "kept by the spec's spelling, and removed");
    assert!(dir.join("old-b").exists(), "kept by the expanded path, and removed");
    assert!(!dir.join("old-c").exists(), "not kept, and still here");
}

/// Keeping one item can keep another the user was never asked about: a kept
/// pacman repo file keeps the keys, since a repo on disk without its key fails
/// every sync. That is said, with why — and asked about only if it added work.
#[test]
fn what_keeping_holds_back_is_said() {
    const KEY: &str = "0123456789ABCDEF0123456789ABCDEF01234567";
    let e = Env::new();
    let stub = |name: &str, body: &str| {
        let p = e.bin.path().join(name);
        fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
    };
    let key_log = e.fake_home.path().join("pacman-key.log");
    stub("pacman", "exit 0");
    stub("pacman-key", &format!("echo \"$@\" >> {}", key_log.display()));
    let keys = e.state.path().join("pacman-keys");
    fs::create_dir_all(&keys).unwrap();
    fs::write(keys.join("t.json"), format!("[\"{KEY}\"]")).unwrap();

    // A repo file temper deployed, untouched since, and dropped from the spec.
    let assets = e.home.path().join("assets");
    fs::create_dir_all(&assets).unwrap();
    fs::write(assets.join("extra.conf"), "[extra]\n").unwrap();
    let spec = |steps: &str| {
        fs::write(
            e.home.path().join("temper.toml"),
            format!("[[machine]]\nname = \"t\"\nos = \"{}\"\napps = [\"a\"]\n", os()),
        )
        .unwrap();
        fs::create_dir_all(e.home.path().join("apps")).unwrap();
        fs::write(e.home.path().join("apps/a.toml"), steps).unwrap();
    };
    spec("[[step]]\ncopy = \"assets/extra.conf\"\nto = \"~/repos/extra.conf\"\n");
    e.temper().arg("install").assert().success();
    let ledger = e.state.path().join("deployed/t.json");
    let mut l: serde_json::Value = serde_json::from_str(&read(&ledger)).unwrap();
    for rec in l.as_object_mut().unwrap().values_mut() {
        rec["kind"] = serde_json::json!("pacman-repo");
    }
    fs::write(&ledger, l.to_string()).unwrap();
    spec("");

    // c to choose; Enter keeps the repo file — the only item asked about.
    let out = e.temper().arg("prune").write_stdin("c\n\n").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("What you kept changes the rest of the plan:"), "{stdout}");
    assert!(
        stdout.contains(&format!("pacman key {KEY} stays — a pacman repo you kept")),
        "{stdout}"
    );
    assert_eq!(stdout.matches("prune the ").count(), 1, "it added no work: {stdout}");
    assert!(stdout.contains("nothing to remove — 2 item(s) left in place"), "{stdout}");
    assert!(!read(&key_log).contains("--delete"), "{}", read(&key_log));
    assert!(e.fake_home.path().join("repos/extra.conf").exists());
}
