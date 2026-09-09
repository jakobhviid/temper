//! A managed region inside a file root owns.
//!
//! The case this exists for: a vendor package installs a config file and owns
//! most of it, while the spec owns a few lines in the middle. 1Password's
//! `/etc/1password/custom_allowed_browsers` is the concrete one — the cask
//! writes its own header and entries, and a spec appends browser basenames. A
//! whole-file `sysfile` would clobber the vendor's content and fight the cask on
//! every update, so `block`'s marker-region semantics are the ones needed; what
//! was missing was doing it as root.
//!
//! `owner`/`group`/`mode` on a `block` step already PARSED before this — and
//! were ignored completely. The step loaded, drift reported it, and `install`
//! attempted an unprivileged `fs::write` to `/etc` with no password even
//! requested. A declaration that looks present and does nothing is worse than
//! one that is absent, because nothing tells the author.
//!
//! `sudo` is stubbed to record its argv and perform the copy, so the assertions
//! are on what temper actually asked for and on the bytes that landed.

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
        // Records the argv, then emulates `install … SRC DEST` by copying the
        // last two arguments. Enough to prove both the request and the result.
        e.stub(
            "sudo",
            &format!(
                "echo \"$@\" >> {}\nfor a in \"$@\"; do prev=\"$last\"; last=\"$a\"; done\ncp \"$prev\" \"$last\" 2>/dev/null\nexit 0",
                e.fake_home.path().join("sudo.log").display()
            ),
        );
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

    fn sudo_log(&self) -> String {
        fs::read_to_string(self.fake_home.path().join("sudo.log")).unwrap_or_default()
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

fn os() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    }
}

/// The vendor's content survives and the spec's region is inserted.
#[test]
fn a_root_owned_block_keeps_the_vendors_lines_and_adds_its_own() {
    let e = Env::new();
    let target = e.fake_home.path().join("custom_allowed_browsers");
    // What the vendor package installed: a header it owns, and an entry.
    fs::write(&target, "# Installed by the vendor. Do not edit.\nflatpak-session-helper\n").unwrap();

    fs::create_dir_all(e.home.path().join("apps")).unwrap();
    fs::create_dir_all(e.home.path().join("assets")).unwrap();
    fs::write(e.home.path().join("assets/browsers"), "vivaldi-bin\nzen-bin\n").unwrap();
    fs::write(
        e.home.path().join("apps/a.toml"),
        format!(
            "[[step]]\nblock = \"assets/browsers\"\nin = \"{}\"\nmarker = \"temper\"\n\
             owner = \"root\"\ngroup = \"root\"\nmode = \"0644\"\n",
            target.display()
        ),
    )
    .unwrap();
    fs::write(
        e.home.path().join("temper.toml"),
        format!("[[machine]]\nname = \"m\"\nos = \"{}\"\napps = [\"a\"]\n", os()),
    )
    .unwrap();

    e.temper().args(["install", "--yes"]).assert().success();

    let got = fs::read_to_string(&target).unwrap();
    // The vendor's lines are untouched — the whole reason this is a region and
    // not a whole-file write.
    assert!(
        got.contains("# Installed by the vendor. Do not edit.")
            && got.contains("flatpak-session-helper"),
        "the vendor's own content must survive:\n{got}"
    );
    assert!(
        got.contains("vivaldi-bin") && got.contains("zen-bin"),
        "the spec's region must be present:\n{got}"
    );

    // And it went through an escalated `install` carrying the declared
    // ownership, rather than an unprivileged write that would simply fail.
    let log = e.sudo_log();
    assert!(
        log.contains("install") && log.contains("-o root") && log.contains("0644"),
        "the write must be escalated with the declared owner and mode:\n{log}"
    );
}

/// An ordinary block still writes unprivileged and stays revertible.
///
/// The mirror that keeps the feature honest: escalation is opt-in via
/// `owner`/`group`/`mode`, so a plain block must not start asking for a
/// password or lose its journal entry.
#[test]
fn a_plain_block_does_not_escalate() {
    let e = Env::new();
    let target = e.fake_home.path().join("plain.conf");
    fs::write(&target, "existing\n").unwrap();

    fs::create_dir_all(e.home.path().join("apps")).unwrap();
    fs::create_dir_all(e.home.path().join("assets")).unwrap();
    fs::write(e.home.path().join("assets/body"), "mine\n").unwrap();
    fs::write(
        e.home.path().join("apps/a.toml"),
        format!(
            "[[step]]\nblock = \"assets/body\"\nin = \"{}\"\nmarker = \"temper\"\n",
            target.display()
        ),
    )
    .unwrap();
    fs::write(
        e.home.path().join("temper.toml"),
        format!("[[machine]]\nname = \"m\"\nos = \"{}\"\napps = [\"a\"]\n", os()),
    )
    .unwrap();

    let out = e.temper().args(["install", "--yes"]).output().unwrap();
    let said = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);

    assert!(
        fs::read_to_string(&target).unwrap().contains("mine"),
        "the region must still be written"
    );
    assert!(
        e.sudo_log().is_empty(),
        "a plain block must not escalate:\n{}",
        e.sudo_log()
    );
    // …and it stays revertible, so it must not appear in the undo caveat.
    assert!(
        !said.contains("root-owned `block`"),
        "a plain block is journaled and revertible:\n{said}"
    );
}

/// A root-owned block says `undo` will not cover it, before it is applied.
///
/// `undo` reverts a file write with a plain `fs::write`, which cannot touch a
/// root-owned target — so it is not journaled, and AGENTS.md question 7 says the
/// user learns that at plan time rather than from a revert that quietly does
/// less than the run promised.
#[test]
fn a_root_owned_block_says_undo_will_not_cover_it() {
    let e = Env::new();
    let target = e.fake_home.path().join("owned.conf");
    fs::write(&target, "vendor\n").unwrap();

    fs::create_dir_all(e.home.path().join("apps")).unwrap();
    fs::create_dir_all(e.home.path().join("assets")).unwrap();
    fs::write(e.home.path().join("assets/body"), "mine\n").unwrap();
    fs::write(
        e.home.path().join("apps/a.toml"),
        format!(
            "[[step]]\nblock = \"assets/body\"\nin = \"{}\"\nmarker = \"temper\"\nowner = \"root\"\n",
            target.display()
        ),
    )
    .unwrap();
    fs::write(
        e.home.path().join("temper.toml"),
        format!("[[machine]]\nname = \"m\"\nos = \"{}\"\napps = [\"a\"]\n", os()),
    )
    .unwrap();

    let out = e.temper().args(["install", "--dry-run"]).output().unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        said.contains("would not be able to revert") && said.contains("root-owned `block`"),
        "the limit has to be stated before the run, not after:\n{said}"
    );
}

/// A file under a root-only parent reads as `unavailable`, never as absent.
///
/// `Path::exists()` answers any stat error as `false`, so a `0750` parent made a
/// present, byte-identical file report `missing`: drift never showed it in sync
/// and every converge rewrote it, escalating each time. Verified locally that
/// `Path::exists()` is `false` while `symlink_metadata` yields
/// `PermissionDenied`, which is the distinction the fix rests on.
///
/// Reproduced without root by taking away the owner's own search permission —
/// DAC denies the owner too, so an unprivileged stat inside gets EACCES exactly
/// as it would under a root-owned `/etc/sudoers.d`.
#[cfg(target_os = "linux")]
#[test]
fn a_file_under_an_unreadable_parent_is_unavailable_not_missing() {
    use std::os::unix::fs::PermissionsExt;

    let e = Env::new();
    let dir = e.fake_home.path().join("locked");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("thing.conf");
    fs::write(&target, "body\n").unwrap();

    fs::create_dir_all(e.home.path().join("apps")).unwrap();
    fs::create_dir_all(e.home.path().join("assets")).unwrap();
    fs::write(e.home.path().join("assets/thing"), "body\n").unwrap();
    fs::write(
        e.home.path().join("apps/a.toml"),
        format!(
            "[[step]]\nsysfile = \"assets/thing\"\nto = \"{}\"\nowner = \"root\"\n",
            target.display()
        ),
    )
    .unwrap();
    fs::write(
        e.home.path().join("temper.toml"),
        format!("[[machine]]\nname = \"m\"\nos = \"{}\"\napps = [\"a\"]\n", os()),
    )
    .unwrap();

    // No search permission on the parent: stat inside now fails with EACCES.
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).unwrap();
    let out = e.temper().args(["drift", "--json"]).output().unwrap();
    // Restore before asserting, so a failure cannot leave an unreadable tempdir.
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let items = v["items"].as_array().cloned().unwrap_or_default();
    let f = items
        .iter()
        .find(|f| f["kind"] == "sysfile")
        .unwrap_or_else(|| panic!("no sysfile finding: {items:?}"));
    assert!(
        f["status"].as_str().unwrap_or_default().starts_with("unavailable"),
        "a probe that was refused is not evidence of absence: {f}"
    );
}
