//! The `temper configure` surface — a small, validated set of *scalar* settings
//! in `temper.toml`, exposed as get/set/unset/list/keys so they're discoverable
//! (and shell-completable) instead of hand-edited guesswork.
//!
//! Only the fleet-wide automation scalars live here: `[git]` toggles and
//! `[update].mode`. Structured/array config (`[brew].trust`, `[ignore]`,
//! `[vars]`, `[[machine]]`) stays hand-edited or managed by `reconcile`/`prune`.
//!
//! `SETTINGS` is the single source of truth — it drives validation, `list`,
//! `keys`, and (via clap's `PossibleValuesParser` in the CLI) shell completion of
//! every key. Keep it in lockstep with the serde structs in `manifest`.

use std::path::Path;

use anyhow::{anyhow, Context, Result};

use crate::manifest;

/// One settable scalar: its dotted key (mirrors the `temper.toml` path), value
/// domain, one-line description, and the display default when unset.
pub struct Setting {
    pub key: &'static str,
    pub kind: Kind,
    pub desc: &'static str,
    pub default: &'static str,
}

/// The value domain of a setting — decides how a value is parsed/validated and
/// how it's displayed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `on` | `off` (stored as a TOML bool; accepts true/false/yes/no/1/0).
    Bool,
    /// `off` | `warn` | `prompt` | `auto` (the `[update].mode` enum).
    Mode,
}

/// Every key `temper configure` knows.
pub const SETTINGS: &[Setting] = &[
    Setting {
        key: "git.remind",
        kind: Kind::Bool,
        desc: "hint when the git-backed home has uncommitted spec changes",
        default: "on",
    },
    Setting {
        key: "git.auto_commit",
        kind: Kind::Bool,
        desc: "commit automatically after a spec-writing verb",
        default: "off",
    },
    Setting {
        key: "git.auto_push",
        kind: Kind::Bool,
        desc: "also push after an auto-commit / on `save`",
        default: "off",
    },
    Setting {
        key: "git.auto_pull",
        kind: Kind::Bool,
        desc: "`git pull` the home before a run (warn, never abort)",
        default: "off",
    },
    Setting {
        key: "git.auto_rebase",
        kind: Kind::Bool,
        desc: "when auto_pull runs, use `--rebase` instead of `--ff-only`",
        default: "off",
    },
    Setting {
        key: "update.mode",
        kind: Kind::Mode,
        desc: "self-update policy on a newer-temper folder (off|warn|prompt|auto)",
        default: "prompt",
    },
];

/// The settable keys — the source clap uses to validate + complete the `key` arg.
pub fn keys() -> Vec<&'static str> {
    SETTINGS.iter().map(|s| s.key).collect()
}

fn find(key: &str) -> Result<&'static Setting> {
    SETTINGS.iter().find(|s| s.key == key).ok_or_else(|| {
        anyhow!("unknown setting `{key}` — run `temper configure keys` for the full list")
    })
}

fn parse_bool(v: &str) -> Result<bool> {
    match v.trim().to_lowercase().as_str() {
        "on" | "true" | "yes" | "1" => Ok(true),
        "off" | "false" | "no" | "0" => Ok(false),
        _ => anyhow::bail!("expected on/off (or true/false), got `{v}`"),
    }
}

fn on_off(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

/// Validate + normalize a value, returning its display form and the toml_edit
/// item to write.
fn normalize(setting: &Setting, value: &str) -> Result<(String, toml_edit::Item)> {
    match setting.kind {
        Kind::Bool => {
            let b = parse_bool(value)?;
            Ok((on_off(b).to_string(), toml_edit::value(b)))
        }
        Kind::Mode => {
            let v = value.trim().to_lowercase();
            if !matches!(v.as_str(), "off" | "warn" | "prompt" | "auto") {
                anyhow::bail!("`{}` expects off | warn | prompt | auto, got `{value}`", setting.key);
            }
            Ok((v.clone(), toml_edit::value(v)))
        }
    }
}

/// Set `key` to `value` in temper.toml (comment-preserving), returning the
/// normalized display value. Also (re)stamps the version.
pub fn set(home: &Path, key: &str, value: &str) -> Result<String> {
    let setting = find(key)?;
    let (display, item) = normalize(setting, value)?;
    let (table, leaf) = setting.key.split_once('.').expect("SETTINGS keys are dotted");
    let p = home.join("temper.toml");
    let s = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    let mut doc: toml_edit::DocumentMut = s.parse().context("parsing temper.toml")?;
    let tbl = doc
        .as_table_mut()
        .entry(table)
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| anyhow!("[{table}] in temper.toml is not a table"))?;
    tbl[leaf] = item;
    let out = manifest::stamp_version(&doc.to_string())?;
    std::fs::write(&p, out).with_context(|| format!("writing {}", p.display()))?;
    Ok(display)
}

/// Remove `key`'s override (revert to its default). A no-op if it isn't set.
pub fn unset(home: &Path, key: &str) -> Result<()> {
    let setting = find(key)?;
    let (table, leaf) = setting.key.split_once('.').expect("SETTINGS keys are dotted");
    let p = home.join("temper.toml");
    let s = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    let mut doc: toml_edit::DocumentMut = s.parse().context("parsing temper.toml")?;
    if let Some(tbl) = doc.get_mut(table).and_then(|t| t.as_table_mut()) {
        tbl.remove(leaf);
    }
    let out = manifest::stamp_version(&doc.to_string())?;
    std::fs::write(&p, out).with_context(|| format!("writing {}", p.display()))?;
    Ok(())
}

/// The effective (file value, else default) display value of one setting.
pub fn get(home: &Path, key: &str) -> Result<String> {
    let setting = find(key)?;
    let s = std::fs::read_to_string(home.join("temper.toml")).unwrap_or_default();
    Ok(effective(&s, setting))
}

/// Every setting with its effective value — backs `list` and `status`.
pub fn list(home: &Path) -> Vec<(&'static str, String)> {
    let s = std::fs::read_to_string(home.join("temper.toml")).unwrap_or_default();
    SETTINGS.iter().map(|st| (st.key, effective(&s, st))).collect()
}

/// Read a setting's value from a raw temper.toml (lenient), falling back to its
/// default. Reads the fleet-level table only (per-machine `[machine.git]`
/// overrides are a runtime concern, surfaced by `status`, not `configure`).
fn effective(src: &str, setting: &Setting) -> String {
    let (table, leaf) = setting.key.split_once('.').expect("SETTINGS keys are dotted");
    let raw = src
        .parse::<toml::Value>()
        .ok()
        .and_then(|v| v.get(table).and_then(|t| t.get(leaf)).cloned());
    match setting.kind {
        Kind::Bool => on_off(raw.and_then(|x| x.as_bool()).unwrap_or(setting.default == "on")).to_string(),
        Kind::Mode => raw
            .and_then(|x| x.as_str().map(str::to_string))
            .unwrap_or_else(|| setting.default.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn home_with(body: &str) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        let mut f = std::fs::File::create(dir.path().join("temper.toml")).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        dir
    }

    #[test]
    fn set_get_unset_roundtrip_bool() {
        let dir = home_with("# fleet\n[vars]\nEDITOR = \"hx\"\n");
        // default (unset) reads as the setting's default
        assert_eq!(get(dir.path(), "git.auto_push").unwrap(), "off");
        // set accepts on/true/yes/1 and displays on
        assert_eq!(set(dir.path(), "git.auto_push", "true").unwrap(), "on");
        assert_eq!(get(dir.path(), "git.auto_push").unwrap(), "on");
        let written = std::fs::read_to_string(dir.path().join("temper.toml")).unwrap();
        assert!(written.contains("[git]") && written.contains("auto_push = true"));
        assert!(written.contains("# fleet") && written.contains("EDITOR = \"hx\"")); // preserved
        assert!(written.contains("temper_version =")); // stamped
        // unset reverts to default
        unset(dir.path(), "git.auto_push").unwrap();
        assert_eq!(get(dir.path(), "git.auto_push").unwrap(), "off");
    }

    #[test]
    fn mode_validates_and_bool_validates() {
        let dir = home_with("");
        assert_eq!(set(dir.path(), "update.mode", "auto").unwrap(), "auto");
        assert_eq!(get(dir.path(), "update.mode").unwrap(), "auto");
        assert!(set(dir.path(), "update.mode", "sometimes").is_err());
        assert!(set(dir.path(), "git.auto_commit", "maybe").is_err());
        assert!(set(dir.path(), "nonsense.key", "on").is_err()); // unknown key
    }

    #[test]
    fn defaults_match_the_git_and_update_defaults() {
        // The display defaults here must mirror manifest's Default impls, or
        // `status` would lie about an unset value.
        let dir = home_with("");
        let got: std::collections::BTreeMap<_, _> = list(dir.path()).into_iter().collect();
        assert_eq!(got["git.remind"], "on"); // GitConfig::default remind = true
        assert_eq!(got["git.auto_commit"], "off");
        assert_eq!(got["update.mode"], "prompt"); // UpdateMode::default
    }
}
