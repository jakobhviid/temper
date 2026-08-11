//! `prune` must not confirm work its executor is going to decline.
//!
//! An undeclared flatpak is an extra wherever it sits, so prune removes from the
//! system *and* user installations. What it cannot reach is a **custom**
//! installation (`/etc/flatpak/installations.d/`), which needs
//! `--installation=NAME` — and an installation it could not read at all. Those
//! are reported; if the plan counted them anyway, prune would ask "remove 2
//! item(s)?" and then remove none.
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

    /// A `flatpak` whose merged `list --app` reports the declared app plus three
    /// extras, and whose `installation`-column listing places each — that second
    /// call is what `scopes` answers. The columns arm comes first because both
    /// invocations contain `list`.
    fn flatpak(&self, scopes: &str) {
        let bin = self.bin.path().join("flatpak");
        fs::write(
            &bin,
            format!(
                "#!/bin/sh\n\
                 case \" $* \" in\n\
                 \x20 *\"--columns=application,installation\"*) {scopes} ;;\n\
                 \x20 *\" list \"*) echo org.declared.App; echo org.sys.Extra; echo org.usr.Extra; echo org.elsewhere.Extra ;;\n\
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

/// The split: system and user extras are both work; one in an installation
/// temper cannot name is reported instead.
#[test]
fn both_installations_are_planned_and_a_custom_one_is_reported() {
    let e = Env::new();
    e.flatpak(
        "printf 'org.declared.App\\tsystem\\norg.sys.Extra\\tsystem\\n\
         org.usr.Extra\\tuser\\norg.elsewhere.Extra\\tmy-extra-install\\n'",
    );

    let v = e.plan();
    let planned = extras(&v);
    assert!(
        planned.contains(&"org.sys.Extra".to_string())
            && planned.contains(&"org.usr.Extra".to_string()),
        "an undeclared app is an extra in either installation temper can name: {v}"
    );
    assert!(
        !planned.contains(&"org.elsewhere.Extra".to_string()),
        "a custom installation needs --installation=NAME, so it may not be planned: {v}"
    );
    assert_eq!(
        unremovable(&v),
        vec!["org.elsewhere.Extra"],
        "…and it must be reported rather than silently dropped: {v}"
    );

    // …and the human is told, or the only trace of it is in the JSON.
    let text = String::from_utf8(e.temper().args(["prune", "--dry-run"]).output().unwrap().stdout)
        .unwrap();
    assert!(
        text.contains("org.elsewhere.Extra") && text.contains("cannot remove"),
        "the terminal never mentioned the extra prune walked past:\n{text}"
    );
}

/// Every extra is somewhere temper cannot name, so the plan is empty. It must
/// not read as a converged machine, and it must not ask to remove anything.
#[test]
fn a_plan_of_only_unreachable_flatpaks_asks_to_remove_nothing() {
    let e = Env::new();
    e.flatpak(
        "printf 'org.sys.Extra\\tother-install\\norg.usr.Extra\\tother-install\\n\
         org.elsewhere.Extra\\tother-install\\n'",
    );

    let v = e.plan();
    assert!(
        extras(&v).is_empty(),
        "nothing here is in an installation temper can name: {v}"
    );
    assert_eq!(
        unremovable(&v).len(),
        3,
        "every extra must be accounted for rather than vanishing: {v}"
    );

    let text = String::from_utf8(e.temper().args(["prune", "--dry-run"]).output().unwrap().stdout)
        .unwrap();
    assert!(
        !text.contains("item(s)"),
        "an empty plan must not offer a count to confirm:\n{text}"
    );
}

/// Three-valued, like every other probe: "could not read it" is not "empty".
/// The executor removes nothing when the installations cannot be enumerated, so
/// the plan must not promise removals either.
#[test]
fn an_unreadable_installation_map_plans_no_removals() {
    let e = Env::new();
    // The `installation`-column listing fails; the merged one still answers.
    e.flatpak("exit 1");

    let v = e.plan();
    assert!(
        extras(&v).is_empty(),
        "a scope we could not read must not yield removals: {v}"
    );
    let un = unremovable(&v);
    assert!(
        un.contains(&"org.sys.Extra".to_string()) && un.len() == 3,
        "…and every extra must still be reported, with the reason: {v}"
    );
}
