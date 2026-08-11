//! `prune` must not confirm work its executor is going to decline.
//!
//! `install` writes flatpaks to the **system** installation; removal is scoped
//! to the **user's**, because a system app belongs to the image or to root, needs
//! polkit, and over ssh hangs. The executor has always known that and refused.
//! The *plan* did not — so on a machine whose extras were all system-installed,
//! prune counted them, asked "remove 2 item(s)?", and then removed none.
//!
//! That is the failure this file pins, and it is the destructive verb's version
//! of "report effects, not intentions": a confirm that overstates what will
//! happen spends the user's trust on the one prompt standing between them and an
//! irreversible removal.

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
    /// A machine declaring one flatpak, so the flatpak manager is probed at all
    /// (a manager nothing declares is never asked).
    fn new() -> Env {
        let e = Env {
            home: TempDir::new().unwrap(),
            fake_home: TempDir::new().unwrap(),
            state: TempDir::new().unwrap(),
            bin: TempDir::new().unwrap(),
        };
        fs::write(
            e.home.path().join("temper.toml"),
            "[[machine]]\nname = \"box\"\nos = \"linux\"\n\
             packages = [\"flatpak \\\"org.declared.App\\\"\"]\n",
        )
        .unwrap();
        e
    }

    /// A `flatpak` where `list --app` reports the declared app plus two extras,
    /// and `list --app --user` reports only one of them. So one extra is
    /// user-installed and removable; the other is system-installed and is not.
    fn flatpak(&self, user_scope: &str) {
        let bin = self.bin.path().join("flatpak");
        fs::write(
            &bin,
            format!(
                "#!/bin/sh\n\
                 case \" $* \" in\n\
                 \x20 *\" --user \"*) {user_scope} ;;\n\
                 \x20 *\" list \"*) echo org.declared.App; echo org.mine.Removable; echo org.image.Baked ;;\n\
                 esac\nexit 0\n"
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
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

    fn plan(&self) -> serde_json::Value {
        let out = self.temper().args(["prune", "--json"]).output().unwrap();
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "prune did not emit one document ({e}): {}",
                String::from_utf8_lossy(&out.stdout)
            )
        })
    }
}

fn extras(v: &serde_json::Value) -> Vec<String> {
    v["extras"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| x["name"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn unremovable(v: &serde_json::Value) -> Vec<String> {
    v["unremovable"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| x["name"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The split: a user-installed extra is work, a system-installed one is not.
#[test]
fn a_system_flatpak_is_reported_but_never_planned() {
    let e = Env::new();
    e.flatpak("echo org.mine.Removable");

    let v = e.plan();
    assert_eq!(
        extras(&v),
        vec!["org.mine.Removable"],
        "only the user-installed extra may be planned for removal: {v}"
    );
    assert_eq!(
        unremovable(&v),
        vec!["org.image.Baked"],
        "the system-installed extra must be reported, not silently dropped: {v}"
    );

    // …and the human is told, or the only trace of it is in the JSON.
    let text = String::from_utf8(e.temper().args(["prune", "--dry-run"]).output().unwrap().stdout)
        .unwrap();
    assert!(
        text.contains("org.image.Baked") && text.contains("cannot remove"),
        "the terminal never mentioned the extra prune walked past:\n{text}"
    );
}

/// The case that made the defect visible: EVERY extra is system-installed, so
/// the plan is empty. It must not read as a converged machine, and it must not
/// ask to remove anything.
#[test]
fn a_plan_of_only_system_flatpaks_asks_to_remove_nothing() {
    let e = Env::new();
    // No user-scope installs at all — the real shape of the Bazzite desktops.
    e.flatpak("true");

    let v = e.plan();
    assert!(
        extras(&v).is_empty(),
        "nothing here is removable by this user, so nothing may be counted: {v}"
    );
    let un = unremovable(&v);
    assert!(
        un.contains(&"org.image.Baked".to_string())
            && un.contains(&"org.mine.Removable".to_string()),
        "both extras must be accounted for rather than vanishing: {v}"
    );

    let text = String::from_utf8(e.temper().args(["prune", "--dry-run"]).output().unwrap().stdout)
        .unwrap();
    assert!(
        !text.contains("item(s)"),
        "an empty plan must not offer a count to confirm:\n{text}"
    );
}

/// Three-valued, like every other probe: "could not tell" is not "all yours".
/// The executor removes nothing when the user scope cannot be enumerated, so the
/// plan must not promise removals either.
#[test]
fn an_unenumerable_user_scope_plans_no_removals() {
    let e = Env::new();
    // `list --app --user` fails; the merged listing still answers.
    let bin = e.bin.path().join("flatpak");
    fs::write(
        &bin,
        "#!/bin/sh\ncase \" $* \" in\n  *\" --user \"*) exit 1 ;;\n\
         \x20 *\" list \"*) echo org.declared.App; echo org.mine.Removable ;;\nesac\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let v = e.plan();
    assert!(
        extras(&v).is_empty(),
        "a scope we could not read must not yield removals: {v}"
    );
    assert_eq!(
        unremovable(&v),
        vec!["org.mine.Removable"],
        "…and the extra must still be reported, with the reason: {v}"
    );
}
