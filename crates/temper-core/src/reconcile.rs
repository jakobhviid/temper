//! reconcile — interactive spec←machine capture (the RIS `reconcile`): absorb
//! installed-but-undeclared **extras** INTO the machine's Brewfile, and DROP
//! declared-but-absent entries FROM it, plus route a flatpak extra to the
//! machine's `[ignore]` instead of tracking it.
//!
//! temper's spec is layered (bundles + loose + a per-machine Brewfile), so
//! reconcile only edits the machine's **own** `brewfile` — never a shared
//! bundle (which would silently change other machines). Entries declared in a
//! bundle can't be dropped here; that stays a hand edit, reported by drift.
//!
//! The planning + file-edit logic here is pure and unit-tested; the prompts,
//! preview, and final confirm live in the CLI.

use std::path::Path;

use anyhow::{anyhow, Context, Result};

use crate::manifest::{self, Machine};
use crate::packages::{self, Installed, Manager};
use crate::providers;

/// An installed-but-undeclared package the user may absorb into the Brewfile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddItem {
    pub manager: Manager,
    pub name: String,
    /// The Brewfile line to append if accepted.
    pub token: String,
    /// Flatpak extras additionally offer an "ignore" choice (→ `[ignore]`).
    pub is_flatpak: bool,
}

/// The computed reconcile plan: what could be added to / dropped from the
/// machine's Brewfile. The CLI turns each into a per-item prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcilePlan {
    /// The machine's Brewfile, relative to the temper-home.
    pub brewfile_rel: String,
    /// Installed, not declared — candidates to add (or, for flatpak, ignore).
    pub adds: Vec<AddItem>,
    /// Brewfile lines that are declared-but-absent — candidates to drop.
    pub drops: Vec<String>,
}

/// The Brewfile token (line) for a `(manager, name)` pair.
pub fn token_for(manager: Manager, name: &str) -> String {
    format!("{} \"{}\"", manager.as_str(), name)
}

/// Classify a dependency-aware brew-extra name into its manager using the live
/// installed snapshot: a cask if it's in the cask set, else a formula. (Taps
/// are rare as extras and brew's cleanup output already strips their prefix, so
/// they fall through to `Brew` — a known v1 edge, reported honestly.)
fn classify_brew(name: &str, installed: &Installed) -> Manager {
    if installed.contains(Manager::Cask, name) {
        Manager::Cask
    } else {
        Manager::Brew
    }
}

/// Compute the reconcile plan for a machine. Read-only — mutates nothing.
pub fn plan(home: &Path, machine: &Machine, ignore: &manifest::Ignore) -> Result<ReconcilePlan> {
    let brewfile_rel = machine.brewfile.clone().ok_or_else(|| {
        anyhow!(
            "machine '{}' has no `brewfile` — reconcile edits the machine's own \
             Brewfile, so declare `brewfile = \"...\"` first (or use `adopt`)",
            machine.name
        )
    })?;

    let effective = packages::effective_set(home, machine)?;
    let installed = providers::probe(&effective)?;

    // ADD candidates. flatpak/vscode/mas use exact naive extras; brew-family
    // uses the dependency-aware cleanup so transitive deps aren't offered.
    let mut adds = Vec::new();
    for (m, name) in packages::extras(&effective, &installed, ignore) {
        if matches!(m, Manager::Flatpak | Manager::Vscode | Manager::Mas) {
            adds.push(AddItem {
                manager: m,
                token: token_for(m, &name),
                is_flatpak: m == Manager::Flatpak,
                name,
            });
        }
    }
    for name in providers::brew_extras(&effective, ignore)? {
        let m = classify_brew(&name, &installed);
        adds.push(AddItem {
            manager: m,
            token: token_for(m, &name),
            is_flatpak: false,
            name,
        });
    }
    adds.sort_by(|a, b| a.token.cmp(&b.token));

    // DROP candidates: Brewfile lines whose package isn't installed.
    let bf_path = home.join(&brewfile_rel);
    let content = std::fs::read_to_string(&bf_path)
        .with_context(|| format!("reading brewfile {}", bf_path.display()))?;
    let mut drops = Vec::new();
    for line in content.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if let Ok(pkg) = packages::parse(l) {
            if installed.probed(pkg.manager) && !installed.contains(pkg.manager, &pkg.match_name()) {
                drops.push(line.to_string());
            }
        }
    }

    Ok(ReconcilePlan {
        brewfile_rel,
        adds,
        drops,
    })
}

/// Brewfile content with `tokens` appended (one per line), separated from any
/// existing content by a newline. Absorbs the extras direction of reconcile.
pub fn brewfile_with_adds(content: &str, tokens: &[String]) -> String {
    let mut out = content.to_string();
    if !tokens.is_empty() && !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    for t in tokens {
        out.push_str(t);
        out.push('\n');
    }
    out
}

/// Brewfile content with each exact line in `drop_lines` removed (trimmed
/// match, so leading/trailing whitespace differences don't defeat the drop).
pub fn brewfile_without(content: &str, drop_lines: &[String]) -> String {
    let drop: std::collections::HashSet<&str> = drop_lines.iter().map(|s| s.trim()).collect();
    let mut out = String::with_capacity(content.len());
    for line in content.lines() {
        if drop.contains(line.trim()) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Append `appid` to `[ignore].<manager>` in a `temper.toml`, preserving
/// comments + formatting (toml_edit). Idempotent — a no-op if already present.
pub fn append_ignore(temper_toml: &str, manager: &str, appid: &str) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = temper_toml
        .parse()
        .context("parsing temper.toml for ignore edit")?;
    let ignore = doc
        .as_table_mut()
        .entry("ignore")
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| anyhow!("[ignore] in temper.toml is not a table"))?;
    let arr = ignore
        .entry(manager)
        .or_insert(toml_edit::Item::Value(toml_edit::Value::Array(
            toml_edit::Array::new(),
        )))
        .as_array_mut()
        .ok_or_else(|| anyhow!("[ignore].{manager} is not an array"))?;
    let present = arr.iter().any(|v| v.as_str() == Some(appid));
    if !present {
        arr.push(appid);
    }
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_grammar() {
        assert_eq!(token_for(Manager::Brew, "jq"), "brew \"jq\"");
        assert_eq!(token_for(Manager::Flatpak, "org.x.App"), "flatpak \"org.x.App\"");
    }

    #[test]
    fn adds_append_after_newline() {
        assert_eq!(
            brewfile_with_adds("brew \"a\"\n", &["brew \"b\"".into()]),
            "brew \"a\"\nbrew \"b\"\n"
        );
        // no trailing newline in source → one is inserted
        assert_eq!(
            brewfile_with_adds("brew \"a\"", &["brew \"b\"".into()]),
            "brew \"a\"\nbrew \"b\"\n"
        );
        // nothing to add → unchanged
        assert_eq!(brewfile_with_adds("brew \"a\"\n", &[]), "brew \"a\"\n");
    }

    #[test]
    fn drops_remove_exact_lines() {
        let content = "brew \"a\"\nbrew \"gone\"\ncask \"c\"\n";
        assert_eq!(
            brewfile_without(content, &["brew \"gone\"".into()]),
            "brew \"a\"\ncask \"c\"\n"
        );
    }

    #[test]
    fn ignore_edit_preserves_comments_and_dedups() {
        let src = "# my fleet\n[ignore]\nflatpak = [\"org.keep\"] # baseline\n";
        let out = append_ignore(src, "flatpak", "org.new").unwrap();
        assert!(out.contains("# my fleet"), "lost top comment: {out}");
        assert!(out.contains("# baseline"), "lost inline comment: {out}");
        assert!(out.contains("org.new"));
        // idempotent
        let again = append_ignore(&out, "flatpak", "org.new").unwrap();
        assert_eq!(again.matches("org.new").count(), 1);
    }

    #[test]
    fn ignore_edit_creates_missing_section() {
        let out = append_ignore("[[machine]]\nname = \"m\"\nos = \"linux\"\n", "flatpak", "org.x").unwrap();
        let doc: toml_edit::DocumentMut = out.parse().unwrap();
        assert_eq!(doc["ignore"]["flatpak"][0].as_str(), Some("org.x"));
    }
}
