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

/// One declared dconf snapshot's reconcile candidates, grouped into the
/// **sections** the dump itself defines. For a snapshot rooted at
/// `/org/gnome/shell/extensions/` each section is one extension, so
/// per-extension prompts fall out of dconf's own structure — the engine never
/// learns what an extension is. A single-key change (`enabled-extensions`) is
/// one section with one key, hence exactly one prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DconfPlan {
    /// Display name — the snapshot's `label`, else its dconf path.
    pub name: String,
    /// The snapshot file, relative to the temper-home.
    pub file_rel: String,
    /// Drifted keys grouped by section header, both sorted.
    pub sections: Vec<(String, Vec<crate::dconf::KeyDiff>)>,
}

/// The computed reconcile plan: what could be added to / dropped from the
/// machine's Brewfile, the fleet-level `[brew].trust` tap-trust drift, and the
/// machine's dconf snapshots. The CLI turns each into a per-item prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcilePlan {
    /// The machine's Brewfile, relative to the temper-home. `None` when the
    /// machine declares none — there is nowhere to write an absorbed package, so
    /// the package half is skipped (the desktop half still runs).
    pub brewfile_rel: Option<String>,
    /// Installed, not declared — candidates to add (or, for flatpak, ignore).
    pub adds: Vec<AddItem>,
    /// Brewfile lines that are declared-but-absent — candidates to drop.
    pub drops: Vec<String>,
    /// Taps trusted on the machine but not in `[brew].trust` (and not in
    /// `[ignore].tap`) — candidates to absorb into `[brew].trust` (or ignore).
    pub trust_adds: Vec<String>,
    /// Taps in `[brew].trust` that aren't currently trusted — candidates to drop
    /// from `[brew].trust`. (Keeping one instead is the `install`/`update` fix.)
    pub trust_drops: Vec<String>,
    /// User-installed GNOME extensions no bundle or machine declares —
    /// candidates to absorb into THIS machine's own `extensions` list.
    pub gext_adds: Vec<String>,
    /// Extensions in THIS machine's own `extensions` list that aren't installed
    /// — candidates to drop. The other half of `gext_adds`, without which
    /// reconcile could only ever grow the list: absorb an extension, uninstall
    /// it later, and every converge reinstalled it with no verb to say
    /// otherwise. Machine-scope only; a bundle's list is shared.
    pub gext_drops: Vec<String>,
    /// Per-snapshot desktop-key candidates (empty off a dconf host). Absorbing
    /// is spec←machine only — pushing a key back OUT is `restore`'s direction,
    /// which drift names separately.
    pub dconf: Vec<DconfPlan>,
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
/// `brew_trust` is the declared fleet-level `[brew].trust`, reconciled against
/// what Homebrew actually trusts (both directions, mirroring packages).
pub fn plan(
    home: &Path,
    machine: &Machine,
    ignore: &manifest::Ignore,
    brew_trust: &[String],
) -> Result<ReconcilePlan> {
    // No `brewfile` → nowhere to write an absorbed package, so the package half
    // is skipped rather than fatal: a desktop machine whose packages all come
    // from bundles can still reconcile its dconf snapshots. The CLI says so.
    let brewfile_rel = machine.brewfile.clone();
    let Some(brewfile_rel) = brewfile_rel else {
        return Ok(ReconcilePlan {
            brewfile_rel: None,
            adds: Vec::new(),
            drops: Vec::new(),
            trust_adds: Vec::new(),
            trust_drops: Vec::new(),
            gext_adds: providers::gext_extras(
                &providers::effective_extensions(home, machine)?,
                ignore,
            ),
            gext_drops: providers::gext_machine_absent(&machine.extensions),
            dconf: dconf_plans(home, machine)?,
        });
    };

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
    // A tap orphan (its formulae migrated to core, so nothing uses it) is
    // absorbed verbatim as a `tap "user/repo"` line — NOT split to a bogus short
    // formula name. Each formula/cask extra is resolved to its FULLY-QUALIFIED
    // token so a tap formula round-trips (a bare short token can be re-offered
    // forever) — resolved in ONE batched `brew info` (per-extra calls made this
    // hang for tens of seconds); fall back to a classified short name if brew
    // can't resolve it.
    let brew_extras = providers::brew_extras(&effective, ignore)?;
    let to_resolve: Vec<&str> = brew_extras
        .iter()
        .filter(|(kind, _)| *kind != Manager::Tap)
        .map(|(_, name)| name.as_str())
        .collect();
    let identities = providers::brew_identities(&to_resolve);
    for (kind, name) in &brew_extras {
        let (m, full) = if *kind == Manager::Tap {
            (Manager::Tap, name.clone())
        } else {
            identities
                .get(name)
                .cloned()
                .unwrap_or_else(|| (classify_brew(name, &installed), name.clone()))
        };
        adds.push(AddItem {
            manager: m,
            token: token_for(m, &full),
            is_flatpak: false,
            name: full,
        });
    }
    adds.sort_by(|a, b| a.token.cmp(&b.token));

    // DROP candidates: Brewfile lines whose package isn't installed. A declared
    // Brewfile that doesn't exist yet reads as EMPTY rather than erroring — that
    // is the seed case (`init`, or declaring `brewfile` then running
    // `--current-state-wins`), where the file is what we're about to create.
    let bf_path = home.join(&brewfile_rel);
    let content = match std::fs::read_to_string(&bf_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading brewfile {}", bf_path.display())),
    };
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

    // Tap-trust reconcile (fleet-level `[brew].trust`). Skipped without brew
    // (`trusted_taps` → None), so a declared trust on a non-brew host neither
    // offers spurious adds nor drops what it can't verify.
    let (mut trust_adds, mut trust_drops) = (Vec::new(), Vec::new());
    if let Some(trusted) = providers::trusted_taps()? {
        // Trusted but not declared (and not ignored) → absorb into `[brew].trust`.
        for tap in &trusted {
            if !brew_trust.iter().any(|t| t == tap) && !ignore.tap.iter().any(|t| t == tap) {
                trust_adds.push(tap.clone());
            }
        }
        // Declared but not trusted → offer to drop from `[brew].trust`.
        for tap in brew_trust {
            if !trusted.iter().any(|t| t == tap) {
                trust_drops.push(tap.clone());
            }
        }
    }
    trust_adds.sort();
    trust_drops.sort();

    Ok(ReconcilePlan {
        brewfile_rel: Some(brewfile_rel),
        adds,
        drops,
        trust_adds,
        trust_drops,
        gext_adds: providers::gext_extras(
            &providers::effective_extensions(home, machine)?,
            ignore,
        ),
        gext_drops: providers::gext_machine_absent(&machine.extensions),
        dconf: dconf_plans(home, machine)?,
    })
}

/// Per-snapshot desktop-key candidates. Empty where dconf is absent (a Mac) or
/// a snapshot has never been captured — a never-captured snapshot is `drift`'s
/// story and `snapshot`'s job, not a wall of per-key prompts.
fn dconf_plans(home: &Path, machine: &Machine) -> Result<Vec<DconfPlan>> {
    let mut out = Vec::new();
    for snap in &machine.dconf {
        if let crate::dconf::SnapshotState::Diffs(diffs) = crate::dconf::snapshot_state(home, snap)?
        {
            if diffs.is_empty() {
                continue;
            }
            out.push(DconfPlan {
                name: snap.name().to_string(),
                file_rel: snap.file.clone(),
                sections: crate::dconf::group_by_section(&diffs),
            });
        }
    }
    Ok(out)
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

/// Append `tap` to `[brew].trust` in a `temper.toml`, preserving comments +
/// formatting (toml_edit). Idempotent — a no-op if already present. The
/// spec←machine "absorb a trusted tap" edit, mirroring `append_ignore`.
/// Whether `temper.toml` already declares a machine by this name.
pub fn has_machine(temper_toml: &str, name: &str) -> Result<bool> {
    let doc: toml_edit::DocumentMut = temper_toml
        .parse()
        .context("parsing temper.toml to look for the machine")?;
    Ok(doc
        .get("machine")
        .and_then(|m| m.as_array_of_tables())
        .is_some_and(|arr| {
            arr.iter()
                .any(|t| t.get("name").and_then(|v| v.as_str()) == Some(name))
        }))
}

/// Append a `[[machine]]` block for a new machine, preserving comments +
/// formatting (toml_edit). `init` uses this to scaffold a machine into the
/// folder; it refuses to touch one that already exists (that's `reconcile`'s
/// job, and silently rewriting a hand-authored block would lose intent).
pub fn append_machine(
    temper_toml: &str,
    name: &str,
    os: &str,
    role: Option<&str>,
    brewfile: &str,
) -> Result<String> {
    if has_machine(temper_toml, name)? {
        return Err(anyhow!(
            "temper.toml already declares a machine named '{name}' — \
             use `temper reconcile` to absorb its current state instead"
        ));
    }
    let mut doc: toml_edit::DocumentMut = temper_toml
        .parse()
        .context("parsing temper.toml for the machine edit")?;
    let arr = doc
        .as_table_mut()
        .entry("machine")
        .or_insert(toml_edit::Item::ArrayOfTables(
            toml_edit::ArrayOfTables::new(),
        ))
        .as_array_of_tables_mut()
        .ok_or_else(|| anyhow!("[[machine]] in temper.toml is not an array of tables"))?;
    let mut t = toml_edit::Table::new();
    t["name"] = toml_edit::value(name);
    t["os"] = toml_edit::value(os);
    if let Some(r) = role {
        t["role"] = toml_edit::value(r);
    }
    t["brewfile"] = toml_edit::value(brewfile);
    t["apps"] = toml_edit::Item::Value(toml_edit::Value::Array(toml_edit::Array::new()));
    arr.push(t);
    Ok(doc.to_string())
}

/// Append a GNOME extension UUID to a machine's own `extensions` list,
/// preserving comments + formatting. Idempotent.
///
/// Machine-scoped on purpose: a bundle's `extensions` is *shared*, so absorbing
/// there would install the extension on every machine composing that bundle.
/// This is the same containment rule that keeps package absorbs in the machine's
/// own Brewfile.
pub fn append_machine_extension(temper_toml: &str, machine: &str, uuid: &str) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = temper_toml
        .parse()
        .context("parsing temper.toml for the extension edit")?;
    let arr = doc
        .as_table_mut()
        .entry("machine")
        .or_insert(toml_edit::Item::ArrayOfTables(
            toml_edit::ArrayOfTables::new(),
        ))
        .as_array_of_tables_mut()
        .ok_or_else(|| anyhow!("[[machine]] in temper.toml is not an array of tables"))?;
    let t = arr
        .iter_mut()
        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(machine))
        .ok_or_else(|| anyhow!("temper.toml declares no machine named '{machine}'"))?;
    let list = t
        .entry("extensions")
        .or_insert(toml_edit::Item::Value(toml_edit::Value::Array(
            toml_edit::Array::new(),
        )))
        .as_array_mut()
        .ok_or_else(|| anyhow!("[[machine]].extensions is not an array"))?;
    if !list.iter().any(|v| v.as_str() == Some(uuid)) {
        list.push(uuid);
    }
    Ok(doc.to_string())
}

/// Remove a GNOME extension UUID from a machine's own `extensions` list,
/// preserving comments + formatting. A no-op if the machine, the list, or the
/// entry is absent — the machine may legitimately declare none.
///
/// The mirror of `append_machine_extension`, and machine-scoped for the same
/// reason: a bundle's `extensions` is shared, so a drop there from one box
/// would un-declare the extension for every machine composing that bundle.
pub fn remove_machine_extension(temper_toml: &str, machine: &str, uuid: &str) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = temper_toml
        .parse()
        .context("parsing temper.toml for the extension edit")?;
    if let Some(arr) = doc
        .as_table_mut()
        .get_mut("machine")
        .and_then(|m| m.as_array_of_tables_mut())
    {
        if let Some(t) = arr
            .iter_mut()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(machine))
        {
            if let Some(list) = t.get_mut("extensions").and_then(|e| e.as_array_mut()) {
                list.retain(|v| v.as_str() != Some(uuid));
            }
        }
    }
    Ok(doc.to_string())
}

pub fn append_trust(temper_toml: &str, tap: &str) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = temper_toml
        .parse()
        .context("parsing temper.toml for trust edit")?;
    let brew = doc
        .as_table_mut()
        .entry("brew")
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| anyhow!("[brew] in temper.toml is not a table"))?;
    let arr = brew
        .entry("trust")
        .or_insert(toml_edit::Item::Value(toml_edit::Value::Array(
            toml_edit::Array::new(),
        )))
        .as_array_mut()
        .ok_or_else(|| anyhow!("[brew].trust is not an array"))?;
    if !arr.iter().any(|v| v.as_str() == Some(tap)) {
        arr.push(tap);
    }
    Ok(doc.to_string())
}

/// Remove `tap` from `[brew].trust` in a `temper.toml`, preserving comments +
/// formatting (toml_edit). A no-op if absent or `[brew].trust` doesn't exist.
pub fn remove_trust(temper_toml: &str, tap: &str) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = temper_toml
        .parse()
        .context("parsing temper.toml for trust edit")?;
    if let Some(arr) = doc
        .get_mut("brew")
        .and_then(|b| b.get_mut("trust"))
        .and_then(|t| t.as_array_mut())
    {
        arr.retain(|v| v.as_str() != Some(tap));
    }
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The undeclare cell round-trips: absorbing an extension and later dropping
    /// it returns the file to where it started, comments and all. Without the
    /// drop half, reconcile could only ever GROW a machine's list — absorb an
    /// extension, uninstall it, and every converge put it back with no verb to
    /// say otherwise.
    #[test]
    fn a_machine_extension_can_be_absorbed_and_dropped_again() {
        let before = "# fleet\n[[machine]]\nname = \"atlas\"\nos = \"linux\"\nextensions = [\"keep@x\"]\n";
        let added = append_machine_extension(before, "atlas", "gone@x").unwrap();
        assert!(added.contains("gone@x"));
        let dropped = remove_machine_extension(&added, "atlas", "gone@x").unwrap();
        assert!(!dropped.contains("gone@x"));
        assert!(dropped.contains("keep@x"), "dropped a sibling it was not asked about");
        assert!(dropped.contains("# fleet"), "lost a comment");
    }

    /// Dropping is machine-scoped and forgiving: an unknown machine, a machine
    /// with no list, or a uuid that is not there all leave the file alone rather
    /// than erroring or touching another machine's block.
    #[test]
    fn dropping_an_extension_never_reaches_another_machine() {
        let src = "[[machine]]\nname = \"atlas\"\nextensions = [\"a@x\"]\n\n[[machine]]\nname = \"helios\"\nextensions = [\"a@x\"]\n";
        let out = remove_machine_extension(src, "atlas", "a@x").unwrap();
        // helios keeps its own declaration.
        assert_eq!(out.matches("a@x").count(), 1);
        for (machine, uuid) in [("nope", "a@x"), ("helios", "absent@x")] {
            assert_eq!(remove_machine_extension(&out, machine, uuid).unwrap(), out);
        }
    }

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

    #[test]
    fn trust_add_preserves_comments_and_dedups() {
        let src = "# my fleet\n[brew]\ntrust = [\"user/keep\"] # baseline\n";
        let out = append_trust(src, "user/new").unwrap();
        assert!(out.contains("# my fleet"), "lost top comment: {out}");
        assert!(out.contains("# baseline"), "lost inline comment: {out}");
        assert!(out.contains("user/new"));
        // idempotent — a second add is a no-op
        let again = append_trust(&out, "user/new").unwrap();
        assert_eq!(again.matches("user/new").count(), 1);
    }

    #[test]
    fn trust_add_creates_missing_section() {
        let out = append_trust("[[machine]]\nname = \"m\"\nos = \"linux\"\n", "user/tap").unwrap();
        let doc: toml_edit::DocumentMut = out.parse().unwrap();
        assert_eq!(doc["brew"]["trust"][0].as_str(), Some("user/tap"));
    }

    #[test]
    fn trust_remove_drops_only_the_named_tap() {
        let src = "[brew]\ntrust = [\"a/one\", \"b/two\"] # keep b\n";
        let out = remove_trust(src, "a/one").unwrap();
        assert!(!out.contains("a/one"), "did not drop: {out}");
        assert!(out.contains("b/two"), "dropped the wrong one: {out}");
        assert!(out.contains("# keep b"), "lost inline comment: {out}");
        // absent tap / missing section → no-op, never an error
        assert_eq!(remove_trust(&out, "nope/gone").unwrap(), out);
        assert_eq!(
            remove_trust("[[machine]]\nname=\"m\"\nos=\"linux\"\n", "x/y").unwrap(),
            "[[machine]]\nname=\"m\"\nos=\"linux\"\n"
        );
    }
}
