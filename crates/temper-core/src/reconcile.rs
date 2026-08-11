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
    /// The `[ignore]` list this extra belongs to, and the value that list must
    /// hold to match it.
    ///
    /// Every manager gets the ignore choice now, not just flatpak. `[ignore]`
    /// had seven lists and a verb could write two of them, while drift honoured
    /// all seven and the status line for a GNOME extension told the user to edit
    /// one by hand. The value matters as much as the key: `[ignore].mas` is
    /// matched against the numeric id, not the app name shown in the prompt.
    pub ignore_key: &'static str,
    pub ignore_value: String,
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
    /// Entries in THIS machine's own loose `packages` list that aren't
    /// installed — the undeclare cell for machine-scope packages. Brewfile lines
    /// have had this since the beginning via `drops`; the loose list is equally
    /// machine scope and had no way to remove an entry at all.
    pub package_drops: Vec<String>,
    /// Flatpak remotes configured but declared nowhere — absorb into THIS
    /// machine's own `flatpak_remotes`.
    pub remote_adds: Vec<String>,
    /// Entries in THIS machine's own `flatpak_remotes` that are not configured.
    pub remote_drops: Vec<String>,
    /// Layered rpms no bundle or machine declares — absorb into THIS machine's
    /// own `rpm_ostree` list.
    pub rpm_adds: Vec<String>,
    /// Entries in THIS machine's own `rpm_ostree` list that are not layered.
    pub rpm_drops: Vec<String>,
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

/// One candidate in a plan, tagged with what it is and where accepting it
/// writes.
///
/// The plans are a field per kind, and each new provider used to mean touching
/// every place that aggregates over them — the `--json` document, the "is there
/// anything to do" check, the "did you pick anything" check, the counts. That is
/// about ten sites related by nothing but attention, and three providers in a
/// row shipped with one or two of them missed.
///
/// Deriving those four from a single list makes adding a provider a matter of
/// extending `items()` and nothing else. The typed fields stay as the source of
/// truth for the *prompt* flow, which is genuinely per-kind: what to ask about a
/// tap is not what to ask about a desktop key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanItem {
    /// Which list it came from — also the `--json` key it appears under.
    pub list: &'static str,
    /// Adding to the spec, or removing from it.
    pub adding: bool,
    pub target: String,
}

impl ReconcilePlan {
    /// Every candidate this plan carries. The one place that enumerates them.
    pub fn items(&self) -> Vec<PlanItem> {
        let mut out = Vec::new();
        let mut push = |list: &'static str, adding: bool, target: String| {
            out.push(PlanItem { list, adding, target })
        };
        for a in &self.adds {
            push("adds", true, a.token.clone());
        }
        for d in &self.drops {
            push("drops", false, d.clone());
        }
        for t in &self.trust_adds {
            push("trust_adds", true, t.clone());
        }
        for t in &self.trust_drops {
            push("trust_drops", false, t.clone());
        }
        for g in &self.gext_adds {
            push("gext_adds", true, g.clone());
        }
        for g in &self.gext_drops {
            push("gext_drops", false, g.clone());
        }
        for p in &self.package_drops {
            push("package_drops", false, p.clone());
        }
        for r in &self.remote_adds {
            push("remote_adds", true, r.clone());
        }
        for r in &self.remote_drops {
            push("remote_drops", false, r.clone());
        }
        for r in &self.rpm_adds {
            push("rpm_adds", true, r.clone());
        }
        for r in &self.rpm_drops {
            push("rpm_drops", false, r.clone());
        }
        for d in &self.dconf {
            for (section, keys) in &d.sections {
                for k in keys {
                    push(
                        "dconf",
                        k.live.is_some(),
                        format!("{}/{}", d.name, crate::dconf::key_id(section, &k.key)),
                    );
                }
            }
        }
        out
    }

    /// Nothing to absorb or drop. Derived, so a new list cannot be forgotten
    /// here — which is how an undeclared GNOME extension once made the whole
    /// feature unreachable behind a "nothing to do" message.
    pub fn is_empty(&self) -> bool {
        self.items().is_empty()
    }

    /// Every candidate list a plan can carry, so an empty one still appears in
    /// `--json` as `[]` rather than vanishing — a consumer should not have to
    /// tell "no candidates" from "this temper does not have that list".
    pub const LISTS: &'static [&'static str] = &[
        "adds",
        "drops",
        "trust_adds",
        "trust_drops",
        "gext_adds",
        "gext_drops",
        "package_drops",
        "remote_adds",
        "remote_drops",
        "rpm_adds",
        "rpm_drops",
    ];

    /// The `--json` plan document, derived from `items()`.
    ///
    /// Derived, but deliberately the **same shape** as before: one key per list.
    /// Nesting them under a `lists` object would have been tidier and would have
    /// broken every consumer for no reason a consumer benefits from. `dconf`
    /// keeps its richer per-snapshot form, because a key-level diff is not a
    /// string.
    pub fn to_json(&self, machine: &str) -> serde_json::Value {
        let mut doc = serde_json::json!({
            "machine": machine,
            "brewfile": self.brewfile_rel,
        });
        for name in Self::LISTS {
            doc[*name] = serde_json::json!([]);
        }
        for i in self.items() {
            if let Some(arr) = doc.get_mut(i.list).and_then(|v| v.as_array_mut()) {
                arr.push(serde_json::json!(i.target));
            }
        }
        doc["dconf"] = serde_json::json!(self
            .dconf
            .iter()
            .map(|d| {
                let keys: Vec<_> = d
                    .sections
                    .iter()
                    .flat_map(|(s, ds)| {
                        ds.iter().map(move |k| {
                            serde_json::json!({
                                "section": s, "key": k.key,
                                "id": k.id(), "status": k.status(),
                                "change": crate::dconf::describe(k),
                            })
                        })
                    })
                    .collect();
                serde_json::json!({ "name": d.name, "file": d.file_rel, "keys": keys })
            })
            .collect::<Vec<_>>());
        doc["total"] = serde_json::json!(self.items().len());
        doc
    }
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

/// The uuids in the machine's own extension list — the candidate set for a drop,
/// which is machine scope only.
fn machine_extension_uuids(machine: &Machine) -> Vec<String> {
    machine
        .gnome_extensions
        .iter()
        .map(|e| e.uuid().to_string())
        .collect()
}

/// Remotes in the machine's own `flatpak_remotes` that are not configured.
/// Machine scope only; a group's remote is not this box's to un-declare.
fn machine_remote_drops(machine: &Machine) -> Vec<String> {
    if machine.flatpak_remotes.is_empty() {
        return Vec::new();
    }
    let Some(live) = providers::flatpak_remotes_installed() else {
        return Vec::new();
    };
    machine
        .flatpak_remotes
        .iter()
        .filter(|t| {
            providers::parse_remote(t)
                .map(|(n, _)| !live.iter().any(|(ln, _)| *ln == n))
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

/// Entries in the machine's own loose `packages` list that are declared but not
/// installed. Machine scope, so removing one is this machine's decision.
///
/// Probes only the machine's own tokens, so it works on a machine that declares
/// no `brewfile` — that early return exists to skip the *Brewfile*, and a loose
/// list is not a Brewfile.
fn machine_package_drops(machine: &Machine) -> Result<Vec<String>> {
    if machine.packages.is_empty() {
        return Ok(Vec::new());
    }
    let parsed: Vec<packages::Pkg> = machine
        .packages
        .iter()
        .filter_map(|t| packages::parse(t).ok())
        .collect();
    if parsed.is_empty() {
        return Ok(Vec::new());
    }
    let installed = providers::probe(&parsed)?;
    let mut out = Vec::new();
    for token in &machine.packages {
        if let Ok(pkg) = packages::parse(token) {
            // `probed` means the manager's tool answered. Without it, absence is
            // "I could not ask", which must never become a drop.
            if installed.probed(pkg.manager) && !installed.contains(pkg.manager, &pkg.match_name())
            {
                out.push(token.clone());
            }
        }
    }
    Ok(out)
}

/// Compute the reconcile plan for a machine. Read-only — mutates nothing.
/// `brew_trust` is the declared fleet-level `[brew].trust`, reconciled against
/// what Homebrew actually trusts (both directions, mirroring packages).
/// `seed` lifts the probe opt-in for discovery: `init` scaffolds an EMPTY
/// machine and then reconciles it, so every "declare at least one first" gate
/// answered "nothing to look at" and the seed absorbed nothing at all.
pub fn plan(
    home: &Path,
    machine: &Machine,
    ignore: &manifest::Ignore,
    brew_trust: &[String],
    seed: bool,
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
            package_drops: machine_package_drops(machine)?,
            gext_drops: providers::gext_machine_absent(&machine_extension_uuids(machine)),
            remote_adds: providers::remotes_extras(&providers::effective_remotes(home, machine)?, ignore),
            remote_drops: machine_remote_drops(machine),
        rpm_adds: providers::rpm_ostree_extras(&providers::effective_rpm(home, machine)?, ignore),
            rpm_drops: providers::rpm_ostree_machine_absent(&machine.rpm_ostree),
            dconf: dconf_plans(home, machine)?,
        });
    };

    let effective = packages::effective_set(home, machine)?;
    let installed = if seed {
        providers::probe_seeding()?
    } else {
        providers::probe(&effective)?
    };

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
                    ignore_key: m.as_str(),
                    // The id, not the app name: that is what drift matches on.
                    ignore_value: name.clone(),
                    name: app,
                });
            }
            Manager::Flatpak | Manager::Vscode => {
                adds.push(AddItem {
                    manager: m,
                    token: token_for(m, &name),
                    ignore_key: m.as_str(),
                    ignore_value: name.clone(),
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
    let brew_extras = if seed {
        providers::brew_extras_seeding(ignore)?
    } else {
        providers::brew_extras(&effective, ignore)?
    };
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
            ignore_key: m.as_str(),
            ignore_value: full.clone(),
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
        // Declared but not trusted → offer to drop, but ONLY from this
        // machine's own `brew_trust`. A tap the fleet declares is a group
        // decision; un-declaring it is a spec edit, and then every machine's
        // converge stops trusting it. Offering it here would let one box delete
        // what the rest of the fleet needs — the incident AGENTS.md records.
        for tap in &machine.brew_trust {
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
        package_drops: machine_package_drops(machine)?,
        gext_drops: providers::gext_machine_absent(&machine_extension_uuids(machine)),
        rpm_adds: providers::rpm_ostree_extras(&providers::effective_rpm(home, machine)?, ignore),
        rpm_drops: providers::rpm_ostree_machine_absent(&machine.rpm_ostree),
        remote_adds: providers::remotes_extras(&providers::effective_remotes(home, machine)?, ignore),
        remote_drops: machine_remote_drops(machine),
        dconf: dconf_plans(home, machine)?,
    })
}

/// Per-snapshot desktop-key candidates. Empty where dconf is absent (a Mac) or
/// a snapshot has never been captured — a never-captured snapshot is `drift`'s
/// story and `snapshot`'s job, not a wall of per-key prompts.
fn dconf_plans(home: &Path, machine: &Machine) -> Result<Vec<DconfPlan>> {
    let mut out = Vec::new();
    // Extension settings reconcile per section exactly like a machine subtree —
    // and because an extension's snapshot is rooted at its own subtree, each
    // section IS one of its settings groups.
    // Machine-scope files only. `reconcile` absorbs live state into the spec,
    // and a bundle's settings file is not this machine's to rewrite — the same
    // reason reconcile never edits a shared Brewfile. Nothing is silently
    // dropped: `drift` still reports these, it just does not offer to answer
    // them from here.
    let (mine, shared) = crate::dconf::snapshots_by_scope(home, machine)?;
    for s in &shared {
        eprintln!(
            "note: `{}` belongs to a bundle, not to {} — its keys are reported \
             by `drift` but not offered here, because absorbing them would \
             change every machine that composes it.",
            s.file, machine.name
        );
    }
    for snap in &mine {
        // Same ownership filter drift uses: reconcile must never offer to absorb
        // a key a `setkey` step already declares.
        let owned = crate::dconf::owned_elsewhere(home, machine, snap)?;
        if let crate::dconf::SnapshotState::Diffs(diffs) =
            crate::dconf::snapshot_state_owned(home, snap, &owned)?
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
    // Write the key the machine ALREADY uses, and the canonical name otherwise.
    //
    // This wrote `extensions` unconditionally — the pre-rename spelling. It
    // still parses, because the field carries a serde alias, so nothing failed
    // on a folder that had not been migrated. On one that HAS been, it appends a
    // second key beside `gnome_extensions`, and serde rejects the pair as a
    // duplicate field: the whole manifest stops parsing, on every machine, and
    // `[git].auto_commit` pushes that. The migration renames first and reconciles
    // afterwards, so this is reachable by following the documented order.
    let key = if t.contains_key(EXT_KEY_OLD) && !t.contains_key(EXT_KEY) {
        EXT_KEY_OLD
    } else {
        EXT_KEY
    };
    let list = t
        .entry(key)
        .or_insert(toml_edit::Item::Value(toml_edit::Value::Array(
            toml_edit::Array::new(),
        )))
        .as_array_mut()
        .ok_or_else(|| anyhow!("[[machine]].{key} is not an array"))?;
    if !list.iter().any(|v| v.as_str() == Some(uuid)) {
        list.push(uuid);
    }
    Ok(doc.to_string())
}

/// The canonical machine-scope extension key, and the spelling that still parses
/// through a serde alias. A folder mid-migration may carry either; a folder
/// carrying **both** does not parse at all, so nothing may ever create the
/// second one.
const EXT_KEY: &str = "gnome_extensions";
const EXT_KEY_OLD: &str = "extensions";

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
            // Both spellings, because a folder mid-migration may carry either
            // — and dropping from only one would leave the entry declared under
            // the other while reporting the removal as done.
            for key in [EXT_KEY, EXT_KEY_OLD] {
                if let Some(list) = t.get_mut(key).and_then(|e| e.as_array_mut()) {
                    list.retain(|v| v.as_str() != Some(uuid));
                }
            }
        }
    }
    Ok(doc.to_string())
}

/// The machine's own block in `temper.toml`, if it declares one.
fn machine_table<'a>(
    doc: &'a mut toml_edit::DocumentMut,
    machine: &str,
) -> Option<&'a mut toml_edit::Table> {
    doc.as_table_mut()
        .get_mut("machine")?
        .as_array_of_tables_mut()?
        .iter_mut()
        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(machine))
}

/// Append `value` to an array on the machine's own block, creating the array if
/// needed. Idempotent, and comment/format preserving.
fn append_machine_list(
    temper_toml: &str,
    machine: &str,
    key: &str,
    value: &str,
) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = temper_toml
        .parse()
        .with_context(|| format!("parsing temper.toml for the {key} edit"))?;
    let t = machine_table(&mut doc, machine)
        .ok_or_else(|| anyhow!("temper.toml declares no machine named '{machine}'"))?;
    let list = t
        .entry(key)
        .or_insert(toml_edit::Item::Value(toml_edit::Value::Array(
            toml_edit::Array::new(),
        )))
        .as_array_mut()
        .ok_or_else(|| anyhow!("[[machine]].{key} is not an array"))?;
    if !list.iter().any(|v| v.as_str() == Some(value)) {
        list.push(value);
    }
    Ok(doc.to_string())
}

/// Remove `value` from an array on the machine's own block. A no-op if the
/// machine, the array, or the entry is absent.
fn remove_machine_list(
    temper_toml: &str,
    machine: &str,
    key: &str,
    value: &str,
) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = temper_toml
        .parse()
        .with_context(|| format!("parsing temper.toml for the {key} edit"))?;
    if let Some(t) = machine_table(&mut doc, machine) {
        if let Some(list) = t.get_mut(key).and_then(|e| e.as_array_mut()) {
            list.retain(|v| v.as_str() != Some(value));
        }
    }
    Ok(doc.to_string())
}

/// Absorb a trusted tap into THIS machine's own `brew_trust`.
///
/// The fleet `[brew].trust` is a group decision; one machine adding to it
/// changes every other machine's spec on the strength of what one box happens to
/// trust. That is the incident AGENTS.md records, and it is why the default
/// target is here. Writing the fleet list stays possible, but only as an
/// explicit, reported opt-in (`--include-trust`).
pub fn append_machine_trust(temper_toml: &str, machine: &str, tap: &str) -> Result<String> {
    append_machine_list(temper_toml, machine, "brew_trust", tap)
}

/// Drop a tap from THIS machine's own `brew_trust`. Never touches the fleet
/// list: a tap the group declares is not this machine's to un-declare.
pub fn remove_machine_trust(temper_toml: &str, machine: &str, tap: &str) -> Result<String> {
    remove_machine_list(temper_toml, machine, "brew_trust", tap)
}

/// Absorb a flatpak remote into THIS machine's own `flatpak_remotes`.
pub fn append_machine_remote(temper_toml: &str, machine: &str, token: &str) -> Result<String> {
    append_machine_list(temper_toml, machine, "flatpak_remotes", token)
}

/// Drop a remote from THIS machine's own `flatpak_remotes`.
pub fn remove_machine_remote(temper_toml: &str, machine: &str, token: &str) -> Result<String> {
    remove_machine_list(temper_toml, machine, "flatpak_remotes", token)
}

/// Absorb a layered rpm into THIS machine's own `rpm_ostree` list.
pub fn append_machine_rpm(temper_toml: &str, machine: &str, pkg: &str) -> Result<String> {
    append_machine_list(temper_toml, machine, "rpm_ostree", pkg)
}

/// Drop a package from THIS machine's own `rpm_ostree` list.
pub fn remove_machine_rpm(temper_toml: &str, machine: &str, pkg: &str) -> Result<String> {
    remove_machine_list(temper_toml, machine, "rpm_ostree", pkg)
}

/// Silence an extra for THIS machine only, under `[machine.ignore].<manager>`.
///
/// Ignoring is a judgement, and it is a per-machine one far more often than a
/// fleet one — a flatpak preinstalled on one box's image is not preinstalled on
/// a Mac. The fleet `[ignore]` still exists for genuine fleet baselines; it is
/// just no longer what a single machine writes to.
pub fn append_machine_ignore(
    temper_toml: &str,
    machine: &str,
    manager: &str,
    value: &str,
) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = temper_toml
        .parse()
        .context("parsing temper.toml for the ignore edit")?;
    let t = machine_table(&mut doc, machine)
        .ok_or_else(|| anyhow!("temper.toml declares no machine named '{machine}'"))?;
    let ign = t
        .entry("ignore")
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| anyhow!("[machine.ignore] is not a table"))?;
    // Implicit so it renders as `[machine.ignore]` rather than an empty stub
    // when only one manager list is present.
    ign.set_implicit(false);
    let list = ign
        .entry(manager)
        .or_insert(toml_edit::Item::Value(toml_edit::Value::Array(
            toml_edit::Array::new(),
        )))
        .as_array_mut()
        .ok_or_else(|| anyhow!("[machine.ignore].{manager} is not an array"))?;
    if !list.iter().any(|v| v.as_str() == Some(value)) {
        list.push(value);
    }
    Ok(doc.to_string())
}

/// Remove a token from a machine's own loose `packages` list, preserving
/// comments + formatting. A no-op if the machine, the list, or the entry is
/// absent. Machine-scoped for the same reason every absorb is.
pub fn remove_machine_package(temper_toml: &str, machine: &str, token: &str) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = temper_toml
        .parse()
        .context("parsing temper.toml for the package edit")?;
    if let Some(arr) = doc
        .as_table_mut()
        .get_mut("machine")
        .and_then(|m| m.as_array_of_tables_mut())
    {
        if let Some(t) = arr
            .iter_mut()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(machine))
        {
            if let Some(list) = t.get_mut("packages").and_then(|e| e.as_array_mut()) {
                list.retain(|v| v.as_str() != Some(token));
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
mod machine_writer_tests {
    use super::*;

    /// The folder is the user's, and these functions edit it in place. Every
    /// machine-scope writer must round-trip, preserve the comments and the
    /// neighbouring machine, and leave TOML that still parses.
    ///
    /// Four of them — the remote and rpm pairs — had a single call site each and
    /// no test at all. A `toml_edit` writer that corrupts a document does it to
    /// the fleet spec, and `[git].auto_commit` then commits the damage.
    const DOC: &str = r#"# fleet spec — keep this comment
[ignore]
flatpak = ["org.example.Baseline"]

[[machine]]
name = "atlas"          # the desktop
os   = "linux"
rpm_ostree = ["already-here"]

[[machine]]
name = "helios"
os   = "mac"
"#;

    fn parses(doc: &str) -> toml::Value {
        doc.parse::<toml::Value>()
            .unwrap_or_else(|e| panic!("writer produced unparseable TOML ({e}):\n{doc}"))
    }

    fn machine<'a>(v: &'a toml::Value, name: &str) -> &'a toml::Value {
        v["machine"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"].as_str() == Some(name))
            .unwrap_or_else(|| panic!("machine `{name}` vanished"))
    }

    fn list(v: &toml::Value, m: &str, key: &str) -> Vec<String> {
        machine(v, m)
            .get(key)
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default()
    }

    #[test]
    fn every_machine_scope_writer_round_trips() {
        // (label, key, value, append, remove)
        type W = fn(&str, &str, &str) -> Result<String>;
        let cases: &[(&str, &str, &str, W, W)] = &[
            (
                "flatpak_remotes",
                "flatpak_remotes",
                "vendor https://example.com/v.flatpakrepo",
                append_machine_remote,
                remove_machine_remote,
            ),
            (
                "rpm_ostree",
                "rpm_ostree",
                "proton-vpn",
                append_machine_rpm,
                remove_machine_rpm,
            ),
            (
                "brew_trust",
                "brew_trust",
                "me/tap",
                append_machine_trust,
                remove_machine_trust,
            ),
            (
                "gnome_extensions",
                "gnome_extensions",
                "x@y",
                append_machine_extension,
                remove_machine_extension,
            ),
        ];

        for (label, key, value, add, drop) in cases {
            let added = add(DOC, "atlas", value)
                .unwrap_or_else(|e| panic!("{label}: append failed: {e}"));
            let v = parses(&added);
            assert!(
                list(&v, "atlas", key).iter().any(|x| x == value),
                "{label}: `{value}` is not in atlas's list after append:\n{added}"
            );
            // The other machine is untouched — a machine-scope write that
            // reaches a sibling is the failure this scope exists to prevent.
            assert!(
                list(&v, "helios", key).is_empty(),
                "{label}: the write reached helios:\n{added}"
            );
            assert!(
                added.contains("# fleet spec — keep this comment")
                    && added.contains("# the desktop"),
                "{label}: comments were lost:\n{added}"
            );

            let removed = drop(&added, "atlas", value)
                .unwrap_or_else(|e| panic!("{label}: remove failed: {e}"));
            let v = parses(&removed);
            assert!(
                !list(&v, "atlas", key).iter().any(|x| x == value),
                "{label}: `{value}` survived the remove:\n{removed}"
            );
            assert!(
                removed.contains("# fleet spec — keep this comment"),
                "{label}: comments were lost on remove:\n{removed}"
            );

            // Removing something absent is a no-op, not an error or a corruption.
            let twice = drop(&removed, "atlas", value)
                .unwrap_or_else(|e| panic!("{label}: second remove errored: {e}"));
            parses(&twice);
        }
    }

    /// Absorbing an extension must never produce a manifest that stops parsing.
    ///
    /// The writer emitted `extensions`, the pre-rename spelling. It still parses
    /// through a serde alias, so nothing failed on an un-migrated folder — but
    /// on a migrated one it appends a second key beside `gnome_extensions`, and
    /// serde rejects the pair as a **duplicate field**. The manifest then fails
    /// to load on every machine, and `auto_commit` pushes it. The migration
    /// renames first and reconciles afterwards, so following the documented
    /// order was enough to reach it.
    #[test]
    fn absorbing_an_extension_never_writes_both_spellings() {
        let migrated = "[[machine]]\nname = \"atlas\"\nos = \"linux\"\n\
                        gnome_extensions = [\"a@x\"]\n";
        let out = append_machine_extension(migrated, "atlas", "b@y").unwrap();
        assert!(
            !(out.contains("gnome_extensions") && out.contains("\nextensions")),
            "wrote both spellings — this manifest no longer parses:\n{out}"
        );
        // …and it really does still load.
        let v: toml::Value = out.parse().unwrap();
        let m = &v["machine"].as_array().unwrap()[0];
        let list: Vec<&str> = m["gnome_extensions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(list, vec!["a@x", "b@y"], "{out}");

        // A folder that has NOT been migrated keeps using its own spelling
        // rather than growing a second key from the other end.
        let old = "[[machine]]\nname = \"atlas\"\nos = \"linux\"\nextensions = [\"a@x\"]\n";
        let out = append_machine_extension(old, "atlas", "b@y").unwrap();
        assert!(
            !out.contains("gnome_extensions"),
            "an un-migrated folder must not gain the second key either:\n{out}"
        );
        assert!(out.contains("b@y"), "{out}");

        // And a drop reaches the entry whichever spelling holds it.
        for doc in [migrated, old] {
            let dropped = remove_machine_extension(doc, "atlas", "a@x").unwrap();
            assert!(!dropped.contains("a@x"), "not dropped:\n{dropped}");
        }
    }

    /// An existing entry is preserved when a sibling is appended, and the fleet
    /// `[ignore]` is never touched by a machine-scope write.
    #[test]
    fn appending_keeps_what_is_already_declared() {
        let out = append_machine_rpm(DOC, "atlas", "proton-vpn").unwrap();
        let v = parses(&out);
        let rpms = list(&v, "atlas", "rpm_ostree");
        assert!(
            rpms.contains(&"already-here".to_string()) && rpms.contains(&"proton-vpn".to_string()),
            "both must survive: {rpms:?}"
        );
        assert!(
            out.contains("org.example.Baseline"),
            "a machine-scope write must not disturb the fleet [ignore]:\n{out}"
        );
    }
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

    /// Every list `items()` can emit appears in `LISTS`, so it reaches `--json`.
    ///
    /// The two are the same knowledge in two places, which is the shape that has
    /// gone wrong repeatedly here — so it is checked rather than trusted. A list
    /// in `items()` but not `LISTS` would be counted by the emptiness check and
    /// invisible in the document, which is the worse of the two failures: the
    /// run does something a `--json` consumer was never shown.
    #[test]
    fn every_emitted_list_is_a_declared_list() {
        // A plan with one candidate in every field, so `items()` emits each tag.
        let plan = ReconcilePlan {
            brewfile_rel: Some("brewfiles/t".into()),
            adds: vec![AddItem {
                manager: Manager::Brew,
                name: "jq".into(),
                token: "brew \"jq\"".into(),
                ignore_key: "brew",
                ignore_value: "jq".into(),
            }],
            drops: vec!["brew \"gone\"".into()],
            trust_adds: vec!["a/b".into()],
            trust_drops: vec!["c/d".into()],
            gext_adds: vec!["e@x".into()],
            gext_drops: vec!["f@x".into()],
            package_drops: vec!["brew \"loose\"".into()],
            remote_adds: vec!["r".into()],
            remote_drops: vec!["s https://x".into()],
            rpm_adds: vec!["vpn".into()],
            rpm_drops: vec!["old".into()],
            dconf: Vec::new(),
        };
        for i in plan.items() {
            assert!(
                ReconcilePlan::LISTS.contains(&i.list),
                "items() emits `{}`, which LISTS does not declare — it would be \
                 counted but never appear in --json",
                i.list
            );
        }
        // …and the document really carries them all.
        let doc = plan.to_json("t");
        for name in ReconcilePlan::LISTS {
            assert_eq!(
                doc[*name].as_array().map(|a| a.len()),
                Some(1),
                "`{name}` did not reach the --json document"
            );
        }
        assert_eq!(doc["total"], 11);
        assert!(!plan.is_empty());
        assert!(ReconcilePlan {
            brewfile_rel: None,
            adds: vec![],
            drops: vec![],
            trust_adds: vec![],
            trust_drops: vec![],
            gext_adds: vec![],
            gext_drops: vec![],
            package_drops: vec![],
            remote_adds: vec![],
            remote_drops: vec![],
            rpm_adds: vec![],
            rpm_drops: vec![],
            dconf: vec![],
        }
        .is_empty());
    }

    /// A machine absorbs a tap into its OWN list, and the fleet list is
    /// untouched.
    ///
    /// This is the scope rule at its sharpest. `[brew].trust` is a group
    /// decision; one machine adding to it changes every other machine's spec on
    /// the strength of what one box happens to trust, and AGENTS.md records the
    /// incident where exactly that deleted a tap the rest of the fleet needed.
    #[test]
    fn absorbing_a_tap_writes_the_machine_not_the_fleet() {
        let src = "[brew]\ntrust = [\"fleet/tap\"]\n\n[[machine]]\nname = \"atlas\"\nos = \"linux\"\n";
        let out = append_machine_trust(src, "atlas", "mine/tap").unwrap();
        let doc: toml_edit::DocumentMut = out.parse().unwrap();
        // fleet list unchanged…
        assert_eq!(doc["brew"]["trust"].as_array().unwrap().len(), 1);
        // …and the tap landed on the machine.
        assert_eq!(
            doc["machine"][0]["brew_trust"][0].as_str(),
            Some("mine/tap")
        );
        // Idempotent.
        assert_eq!(append_machine_trust(&out, "atlas", "mine/tap").unwrap(), out);
    }

    /// Dropping a tap is machine-scoped too: a fleet-declared tap is not this
    /// machine's to un-declare, so `remove_machine_trust` cannot reach it.
    #[test]
    fn dropping_a_tap_cannot_reach_the_fleet_list() {
        let src = "[brew]\ntrust = [\"fleet/tap\"]\n\n[[machine]]\nname = \"atlas\"\nbrew_trust = [\"fleet/tap\", \"mine/tap\"]\n";
        let out = remove_machine_trust(src, "atlas", "fleet/tap").unwrap();
        let doc: toml_edit::DocumentMut = out.parse().unwrap();
        // Gone from the machine, still declared by the fleet — so the machine
        // keeps trusting it via the union, which is the correct outcome: opting
        // out of a group decision is a spec edit, not a reconcile.
        assert_eq!(doc["brew"]["trust"].as_array().unwrap().len(), 1);
        assert_eq!(doc["machine"][0]["brew_trust"].as_array().unwrap().len(), 1);
    }

    /// Ignoring is a per-machine judgement, and lands under `[machine.ignore]`.
    #[test]
    fn ignoring_an_extra_writes_the_machine_not_the_fleet() {
        let src = "[ignore]\nflatpak = [\"org.fleet\"]\n\n[[machine]]\nname = \"atlas\"\n";
        let out = append_machine_ignore(src, "atlas", "flatpak", "org.mine").unwrap();
        let doc: toml_edit::DocumentMut = out.parse().unwrap();
        assert_eq!(doc["ignore"]["flatpak"].as_array().unwrap().len(), 1);
        assert_eq!(
            doc["machine"][0]["ignore"]["flatpak"][0].as_str(),
            Some("org.mine")
        );
        assert_eq!(
            append_machine_ignore(&out, "atlas", "flatpak", "org.mine").unwrap(),
            out
        );
    }

    /// The loose-list twin of the extension round-trip: a machine-scope package
    /// can be un-declared, and only from that machine's own block.
    #[test]
    fn a_machine_package_can_be_dropped_without_touching_another_machine() {
        let src = "[[machine]]\nname = \"atlas\"\npackages = [\"brew \\\"jq\\\"\", \"brew \\\"bat\\\"\"]\n\n[[machine]]\nname = \"helios\"\npackages = [\"brew \\\"jq\\\"\"]\n";
        let out = remove_machine_package(src, "atlas", "brew \"jq\"").unwrap();
        assert!(out.contains("bat"), "dropped a sibling it was not asked about");
        // helios keeps its own declaration of the same package.
        assert_eq!(out.matches("jq").count(), 1);
        // Unknown machine / absent token / no list: all no-ops, never an error.
        for (machine, token) in [("nope", "brew \"jq\""), ("helios", "brew \"absent\"")] {
            assert_eq!(remove_machine_package(&out, machine, token).unwrap(), out);
        }
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
