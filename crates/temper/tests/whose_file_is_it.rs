//! Absorption may only write files that belong to the machine doing it.
//!
//! An extension's `settings = "…"` inherits the scope of whatever declared the
//! extension, so a bundle several machines compose carries its settings at group
//! scope. `snapshot-dconf` then captured one box's live desktop straight into
//! that shared file — 17 keys, on the machine this was found on — and every
//! other machine in the group silently started converging towards whatever that
//! one box happened to hold. No preview, no confirm, no mention.
//!
//! It is the `--include-trust` lesson with a different noun, which is why the
//! rule is written down: anything absorbed from one machine's live state has to
//! land somewhere that belongs to *that machine*, and a shared file needs an
//! explicit opt-in plus a report of what was skipped.

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
        e.fake_dconf();
        // The observability guard fails closed on a host with no dconf database
        // — correctly, and it runs before any of this. Give the fake home one.
        let db = e.fake_home.path().join(".config/dconf");
        fs::create_dir_all(&db).unwrap();
        fs::write(db.join("user"), b"\0").unwrap();
        e
    }

    /// A `dconf` with one key under the extension's subtree, so a capture that
    /// runs has something to write and an empty file means it did not run.
    fn fake_dconf(&self) {
        let bin = self.bin.path().join("dconf");
        fs::write(
            &bin,
            "#!/bin/sh\n\
             case \"$1\" in\n\
             \x20 dump) echo '[/]'; echo \"captured-from=$(hostname 2>/dev/null || echo box)\" ;;\n\
             \x20 *) : ;;\n\
             esac\n\
             exit 0\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn write(&self, rel: &str, body: &str) {
        let p = self.home.path().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.home.path().join(rel)).unwrap_or_default()
    }

    fn temper(&self) -> Command {
        let mut c = Command::cargo_bin("temper").unwrap();
        c.env("TEMPER_DIR", self.home.path())
            .env("HOME", self.fake_home.path())
            .env("TEMPER_STATE_DIR", self.state.path())
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.path().display()));
        c
    }
}

/// The extension is declared in a bundle, so its settings file is the group's.
fn shared_folder() -> Env {
    let e = Env::new();
    e.write(
        "temper.toml",
        "[[machine]]\nname = \"box\"\nos = \"linux\"\napps = [\"shared\"]\n",
    );
    e.write(
        "apps/shared.toml",
        "gnome_extensions = [{ uuid = \"a@x\", settings = \"assets/shared.dconf\", \
         settings_path = \"/org/gnome/shell/extensions/a/\" }]\n",
    );
    e.write("assets/shared.dconf", "");
    e
}

#[test]
fn snapshot_will_not_capture_one_machine_into_a_bundles_file() {
    let e = shared_folder();

    let out = e.temper().args(["snapshot-dconf", "box"]).output().unwrap();

    assert_eq!(
        e.read("assets/shared.dconf"),
        "",
        "a bundle's settings file was overwritten with this one machine's live \
         desktop — every machine composing that bundle now converges towards it"
    );

    // Skipped, not hidden. A capture that quietly does nothing is the failure
    // mode this whole rule exists to avoid.
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("assets/shared.dconf") && err.contains("--include-shared"),
        "the skip must name the file and how to override it; stderr was:\n{err}"
    );
}

/// …and the opt-in works, or the rule is a wall rather than a default.
#[test]
fn include_shared_captures_it_on_purpose() {
    let e = shared_folder();

    e.temper()
        .args(["snapshot-dconf", "box", "--include-shared"])
        .output()
        .unwrap();

    assert!(
        e.read("assets/shared.dconf").contains("captured-from"),
        "`--include-shared` is the way to author a group's settings from one \
         box; without it working, the default is not a default but a block"
    );
}

/// The control: the machine's own declaration is its own file, and nothing about
/// this rule may stop it being captured.
#[test]
fn a_machines_own_extension_settings_are_still_captured() {
    let e = Env::new();
    e.write(
        "temper.toml",
        "[[machine]]\nname = \"box\"\nos = \"linux\"\n\
         gnome_extensions = [{ uuid = \"a@x\", settings = \"assets/mine.dconf\", \
         settings_path = \"/org/gnome/shell/extensions/a/\" }]\n",
    );
    e.write("assets/mine.dconf", "");

    e.temper().args(["snapshot-dconf", "box"]).output().unwrap();

    assert!(
        e.read("assets/mine.dconf").contains("captured-from"),
        "the machine's own settings file is exactly what snapshot exists to write"
    );
}

/// Reading is not writing. A group's settings are meant to be restored onto and
/// compared against every machine that composes the bundle — scoping the *read*
/// would turn a shared declaration into one that does nothing.
#[test]
fn a_shared_file_is_still_restored_and_compared() {
    let e = shared_folder();
    e.write("assets/shared.dconf", "[/]\nshared-value='yes'\n");

    let out = e
        .temper()
        .args(["restore-dconf", "box", "--dry-run", "--json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|err| panic!("not one JSON document ({err}):\n{stdout}"));
    let restored = v["restored"].as_array().cloned().unwrap_or_default();
    assert!(
        restored.iter().any(|p| p.as_str().unwrap_or("").contains("shared.dconf")),
        "a bundle's settings must still restore onto the machines that compose \
         it — only absorbing INTO the file is scoped: {v}"
    );
}
