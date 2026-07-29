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
    // mas extras come back as numeric ids; resolve them to app names so the
    // prompt is legible and the written token is well-formed grammar.
    let mas_names = providers::mas_names();
    for (m, name) in packages::extras(&effective, &installed, ignore) {
        match m {
            Manager::Mas => {
                let app = mas_names
                    .get(&name)
                    .cloned()
                    .unwrap_or_else(|| name.clone());
                adds.push(AddItem {
                    manager: m,
                    // `mas "App Name", id: 12345` — the Brewfile grammar mas needs.
                    token: format!("mas \"{app}\", id: {name}"),
                    is_flatpak: false,
                    name: app,
                });
            }
            Manager::Flatpak | Manager::Vscode => {
                adds.push(AddItem {
                    manager: m,
                    token: token_for(m, &name),
                    is_flatpak: m == Manager::Flatpak,
                    name,
                });
            }
            _ => {} // brew-family handled dependency-aware below
        }
    }
    for (kind, name) in providers::brew_extras(&effective, ignore)? {
        // A tap orphan (its formulae migrated to core, so nothing uses it) is
        // absorbed verbatim as a `tap "user/repo"` line — NOT split to a bogus
        // short formula name. A formula/cask extra is resolved to its
        // FULLY-QUALIFIED token via `brew info` so a tap formula round-trips (a
        // bare short token can be re-offered forever); fall back to a classified
        // short name if brew can't resolve it.
        let (m, full) = if kind == Manager::Tap {
            (Manager::Tap, name)
        } else {
            providers::brew_identity(&name)
                .unwrap_or_else(|| (classify_brew(&name, &installed), name.clone()))
        };
        adds.push(AddItem {
            manager: m,
            token: token_for(m, &full),
            is_flatpak: false,
            name: full,
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
            if installed.probed(pkg.manager) && !installed.contains(pkg.manager, &pkg.match_name())
            {
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

/// Canonically re-sort a Brewfile: entries grouped by manager in brew-bundle's
/// order (tap, brew, cask, mas, vscode, flatpak) and sorted case-insensitively
/// by name within each group, one blank line between groups. Each entry keeps
/// the comment line(s) directly above it (they move with it). A leading comment
/// block set off from the first entry by a blank line is pinned at the top as a
/// file header; comments after the last entry are kept at the end. Lines that
/// don't parse are preserved (grouped last, original order) — nothing is dropped.
pub fn sort_brewfile(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let Some(first) = lines.iter().position(|l| packages::parse(l).is_ok()) else {
        return content.to_string(); // no package entries — leave it exactly as-is
    };

    // Comments directly above the first entry attach to it; everything before
    // that (including blanks) is pinned at the top as the file header.
    let mut a = first;
    while a > 0 && lines[a - 1].trim_start().starts_with('#') {
        a -= 1;
    }
    let mut header: Vec<&str> = lines[..a].to_vec();
    while header.last().is_some_and(|l| l.trim().is_empty()) {
        header.pop();
    }

    fn rank(m: Manager) -> u8 {
        match m {
            Manager::Tap => 0,
            Manager::Brew => 1,
            Manager::Cask => 2,
            Manager::Mas => 3,
            Manager::Vscode => 4,
            Manager::Flatpak => 5,
        }
    }
    struct Item<'a> {
        comments: Vec<&'a str>,
        line: &'a str,
        rank: u8,
        key: String,
    }

    let mut items: Vec<Item> = Vec::new();
    let mut pending: Vec<&str> = Vec::new();
    for &line in &lines[a..] {
        let t = line.trim();
        if t.is_empty() {
            continue; // blanks are normalized away
        }
        if t.starts_with('#') {
            pending.push(line);
            continue;
        }
        let (rank, key) = match packages::parse(line) {
            Ok(pkg) => (rank(pkg.manager), pkg.name.to_lowercase()),
            Err(_) => (u8::MAX, String::new()), // unparseable → last, stable order
        };
        items.push(Item {
            comments: std::mem::take(&mut pending),
            line,
            rank,
            key,
        });
    }
    let trailing = pending; // comments after the last entry

    // Stable sort: equal (rank, key) keep input order; unparseable lines stay put.
    items.sort_by(|x, y| x.rank.cmp(&y.rank).then_with(|| x.key.cmp(&y.key)));

    let mut out = String::new();
    for l in &header {
        out.push_str(l);
        out.push('\n');
    }
    let mut prev_rank: Option<u8> = None;
    for it in &items {
        match prev_rank {
            Some(pr) if pr != it.rank => out.push('\n'), // blank between groups
            None if !header.is_empty() => out.push('\n'), // header ↔ first group
            _ => {}
        }
        prev_rank = Some(it.rank);
        for c in &it.comments {
            out.push_str(c);
            out.push('\n');
        }
        out.push_str(it.line);
        out.push('\n');
    }
    if !trailing.is_empty() {
        out.push('\n');
        for l in &trailing {
            out.push_str(l);
            out.push('\n');
        }
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
        assert_eq!(
            token_for(Manager::Flatpak, "org.x.App"),
            "flatpak \"org.x.App\""
        );
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
    fn sort_groups_taps_brews_casks_mas_alphabetically() {
        let input = "cask \"zoom\"\nbrew \"jq\"\ntap \"a/b\"\nbrew \"bat\"\nmas \"App\", id: 1\ncask \"alfred\"\n";
        assert_eq!(
            sort_brewfile(input),
            "tap \"a/b\"\n\nbrew \"bat\"\nbrew \"jq\"\n\ncask \"alfred\"\ncask \"zoom\"\n\nmas \"App\", id: 1\n"
        );
    }

    #[test]
    fn sort_keeps_a_comment_with_its_entry() {
        let input = "brew \"zebra\"\n# my jq\nbrew \"jq\"\n";
        assert_eq!(
            sort_brewfile(input),
            "# my jq\nbrew \"jq\"\nbrew \"zebra\"\n"
        );
    }

    #[test]
    fn sort_pins_leading_header_block() {
        let input = "# machine header\n# line 2\n\nbrew \"zed\"\nbrew \"bat\"\n";
        assert_eq!(
            sort_brewfile(input),
            "# machine header\n# line 2\n\nbrew \"bat\"\nbrew \"zed\"\n"
        );
    }

    #[test]
    fn sort_is_idempotent() {
        let input = "cask \"zoom\"\nbrew \"jq\"\n# c\ntap \"a/b\"\nbrew \"bat\"\n";
        let once = sort_brewfile(input);
        assert_eq!(sort_brewfile(&once), once);
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
    fn mas_add_token_is_well_formed_grammar() {
        // The token plan() builds for a mas extra must re-parse (carry an id),
        // else it bricks the brewfile with "mas entry needs id" on next read.
        let token = format!("mas \"{}\", id: {}", "Xcode", "497799835");
        let pkg = crate::packages::parse(&token).unwrap();
        assert_eq!(pkg.manager, Manager::Mas);
        assert_eq!(pkg.match_name(), "497799835");
        // even the name-lookup-failed fallback (name == id) is valid grammar
        assert!(crate::packages::parse("mas \"497799835\", id: 497799835").is_ok());
        // the OLD bare form is (correctly) rejected — what we no longer emit
        assert!(crate::packages::parse("mas \"497799835\"").is_err());
    }

    #[test]
    fn ignore_edit_creates_missing_section() {
        let out = append_ignore(
            "[[machine]]\nname = \"m\"\nos = \"linux\"\n",
            "flatpak",
            "org.x",
        )
        .unwrap();
        let doc: toml_edit::DocumentMut = out.parse().unwrap();
        assert_eq!(doc["ignore"]["flatpak"][0].as_str(), Some("org.x"));
    }
}
