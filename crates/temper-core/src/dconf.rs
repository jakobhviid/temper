//! Whole-desktop dconf snapshots (machine scope): the RIS `gnome-backup` /
//! `gnome-restore` (and Ptyxis) pair. `backup` dumps a dconf subtree through a
//! strip-keys filter into a file in the folder; `restore` loads it back into
//! live dconf. Restore is a distinct, confirm-gated verb — never part of
//! `update` — because reloading a snapshot clobbers live tweaks (which is
//! exactly why RIS excludes it from its update flow).
//!
//! Degrades on a host without `dconf` (e.g. a Mac): backup is a no-op, restore
//! errors loudly.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::journal::Journal;
use crate::manifest::{DconfSnapshot, Machine};
use crate::primitives::which;

/// The relative section path a `[…]` header denotes — `""` for the dump root
/// (`[/]`). Shared by the filter and the parser so both agree on what a
/// `section/key` id is.
fn section_of(header: &str) -> String {
    let t = header.trim();
    t.trim_start_matches('[')
        .trim_end_matches(']')
        .trim_matches('/')
        .to_string()
}

/// The id `strip` matches against and drift reports: `section/key`, or a bare
/// `key` at the dump root.
pub fn key_id(section: &str, key: &str) -> String {
    if section.is_empty() {
        key.to_string()
    } else {
        format!("{section}/{key}")
    }
}

/// Filter a `dconf dump` block, dropping any `key=value` whose `section/key`
/// path contains one of the `strip` substrings, and any section left empty by
/// that filtering. Pure — the testable heart of a filtered capture.
pub fn strip_dump(dump: &str, strip: &[String]) -> String {
    let mut out = String::new();
    let mut header = String::new();
    let mut section = String::new();
    let mut kept: Vec<String> = Vec::new();

    fn flush(out: &mut String, header: &str, kept: &[String]) {
        if !kept.is_empty() && !header.is_empty() {
            out.push_str(header);
            out.push('\n');
            for k in kept {
                out.push_str(k);
                out.push('\n');
            }
            out.push('\n');
        }
    }

    for line in dump.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            flush(&mut out, &header, &kept);
            kept.clear();
            header = line.to_string();
            section = section_of(t);
        } else if t.is_empty() {
            continue;
        } else if let Some((k, _)) = line.split_once('=') {
            let key = k.trim();
            let id = key_id(&section, key);
            if !strip.iter().any(|p| id.contains(p.as_str())) {
                kept.push(line.to_string());
            }
        } else {
            kept.push(line.to_string()); // preserve odd lines within a section
        }
    }
    flush(&mut out, &header, &kept);
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

/// Parse a dump into `(section, key) → value`. Ordered, so a diff over two
/// parses reports sections alphabetically and keys alphabetically within one —
/// deterministic output regardless of the order dconf emitted them. Lines that
/// aren't `key=value` (and the headers themselves) are structure, not data, and
/// don't survive the parse.
pub fn parse_dump(dump: &str) -> BTreeMap<(String, String), String> {
    let mut out = BTreeMap::new();
    let mut section = String::new();
    for line in dump.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            section = section_of(t);
        } else if t.is_empty() {
            continue;
        } else if let Some((k, v)) = t.split_once('=') {
            out.insert((section.clone(), k.trim().to_string()), v.trim().to_string());
        }
    }
    out
}

/// One key-level difference between a snapshot file and live dconf. `file` is
/// the value the spec holds, `live` the value on the machine — exactly one may
/// be `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyDiff {
    pub section: String,
    pub key: String,
    pub file: Option<String>,
    pub live: Option<String>,
}

impl KeyDiff {
    /// `section/key` — the same id `strip` matches against.
    pub fn id(&self) -> String {
        key_id(&self.section, &self.key)
    }

    /// Drift vocabulary, matching the package side: `missing` = declared in the
    /// spec but not set on the machine, `extra` = set on the machine but not
    /// captured, `changed` = both, different values.
    pub fn status(&self) -> &'static str {
        match (&self.file, &self.live) {
            (Some(_), Some(_)) => "changed",
            (Some(_), None) => "missing",
            (None, Some(_)) => "extra",
            (None, None) => "unchanged",
        }
    }
}

/// Diff a snapshot file against a live dump. **Both sides must already be
/// filtered through the same `strip` list** — otherwise stripped keys read as
/// permanent `extra` drift. Pure; the testable heart of dconf drift.
pub fn diff_dumps(file: &str, live: &str) -> Vec<KeyDiff> {
    let (a, b) = (parse_dump(file), parse_dump(live));
    let mut out = Vec::new();
    for (id, fv) in &a {
        match b.get(id) {
            Some(lv) if lv == fv => {}
            other => out.push(KeyDiff {
                section: id.0.clone(),
                key: id.1.clone(),
                file: Some(fv.clone()),
                live: other.cloned(),
            }),
        }
    }
    for (id, lv) in &b {
        if !a.contains_key(id) {
            out.push(KeyDiff {
                section: id.0.clone(),
                key: id.1.clone(),
                file: None,
                live: Some(lv.clone()),
            });
        }
    }
    out.sort_by(|x, y| (&x.section, &x.key).cmp(&(&y.section, &y.key)));
    out
}

/// Group diffs by their section header, preserving the sorted order. This is
/// the grouping reconcile prompts on: for a snapshot rooted at
/// `/org/gnome/shell/extensions/` each section *is* one extension, so
/// per-extension prompts fall out of the dump's own structure — the engine
/// never learns what an extension is.
pub fn group_by_section(diffs: &[KeyDiff]) -> Vec<(String, Vec<KeyDiff>)> {
    let mut out: Vec<(String, Vec<KeyDiff>)> = Vec::new();
    for d in diffs {
        match out.last_mut() {
            Some((s, v)) if *s == d.section => v.push(d.clone()),
            _ => out.push((d.section.clone(), vec![d.clone()])),
        }
    }
    out
}

/// A dump as section blocks — the shape both the filter and the editors think
/// in. `header` is the raw `[…]` line so a rewrite preserves it byte-for-byte.
struct Block {
    header: String,
    section: String,
    lines: Vec<String>,
}

fn blocks(dump: &str) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    for line in dump.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            out.push(Block {
                header: line.to_string(),
                section: section_of(t),
                lines: Vec::new(),
            });
        } else if t.is_empty() {
            continue;
        } else if let Some(b) = out.last_mut() {
            b.lines.push(line.to_string());
        }
    }
    out
}

fn render(blocks: &[Block]) -> String {
    let mut out = String::new();
    for b in blocks {
        if b.lines.is_empty() {
            continue; // a section with no keys is not a section
        }
        out.push_str(&b.header);
        out.push('\n');
        for l in &b.lines {
            out.push_str(l);
            out.push('\n');
        }
        out.push('\n');
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

/// The key a dump line declares, if it is a `key=value` line.
fn line_key(line: &str) -> Option<&str> {
    line.trim().split_once('=').map(|(k, _)| k.trim())
}

/// Set `section/key` to `value`, replacing it in place if present, appending to
/// its section if not, and creating the section if the dump has none. Pure —
/// the write half of a per-key reconcile absorb.
pub fn dump_with_key(dump: &str, section: &str, key: &str, value: &str) -> String {
    let mut bs = blocks(dump);
    let entry = format!("{key}={value}");
    if let Some(b) = bs.iter_mut().find(|b| b.section == section) {
        match b.lines.iter_mut().find(|l| line_key(l) == Some(key)) {
            Some(l) => *l = entry,
            None => b.lines.push(entry),
        }
    } else {
        bs.push(Block {
            // dconf writes the root section as `[/]`; nested ones unslashed.
            header: if section.is_empty() {
                "[/]".to_string()
            } else {
                format!("[{section}]")
            },
            section: section.to_string(),
            lines: vec![entry],
        });
    }
    render(&bs)
}

/// Drop `section/key`, removing the section if that left it empty. Pure — the
/// write half of dropping a key the machine no longer sets.
pub fn dump_without_key(dump: &str, section: &str, key: &str) -> String {
    let mut bs = blocks(dump);
    if let Some(b) = bs.iter_mut().find(|b| b.section == section) {
        b.lines.retain(|l| line_key(l) != Some(key));
    }
    render(&bs)
}

/// Apply accepted absorbs to a snapshot file's content (spec←machine): a key
/// the machine sets takes the live value; a key the machine no longer sets is
/// dropped. Pure — the write half of a per-key reconcile.
pub fn absorbed(content: &str, accepted: &[KeyDiff]) -> String {
    let mut out = content.to_string();
    for d in accepted {
        out = match &d.live {
            Some(v) => dump_with_key(&out, &d.section, &d.key, v),
            None => dump_without_key(&out, &d.section, &d.key),
        };
    }
    out
}

/// Shorten a value for a prompt line so one runaway GVariant literal can't
/// swamp the screen.
fn clip(s: &str) -> String {
    const MAX: usize = 44;
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    format!("{}…", s.chars().take(MAX - 1).collect::<String>())
}

/// A compact, human description of one key's change, for a reconcile prompt.
///
/// A GVariant string array (`enabled-extensions`, `favorite-apps`, a keybinding
/// list) renders as a **member-level** delta — `+2 −1  +caffeine, +blur-my-shell,
/// −dash-to-dock` — because printing two hundred-character arrays side by side
/// tells you nothing. Any other value renders as `old → new`. Both are generic:
/// the rule is "this value is list-shaped", never "this key is special".
pub fn describe(d: &KeyDiff) -> String {
    if let (Some(f), Some(l)) = (&d.file, &d.live) {
        if let (Ok(a), Ok(b)) = (
            crate::primitives::parse_gvariant_as(f),
            crate::primitives::parse_gvariant_as(l),
        ) {
            let added: Vec<&String> = b.iter().filter(|m| !a.contains(m)).collect();
            let removed: Vec<&String> = a.iter().filter(|m| !b.contains(m)).collect();
            if added.is_empty() && removed.is_empty() {
                return "reordered".to_string();
            }
            let mut parts: Vec<String> = Vec::new();
            parts.extend(added.iter().map(|m| format!("+{m}")));
            parts.extend(removed.iter().map(|m| format!("−{m}")));
            return format!(
                "+{} −{}  {}",
                added.len(),
                removed.len(),
                clip(&parts.join(", "))
            );
        }
        return format!("{} → {}", clip(f), clip(l));
    }
    match (&d.file, &d.live) {
        (None, Some(l)) => format!("live: {}", clip(l)),
        (Some(f), None) => format!("captured: {} (machine no longer sets it)", clip(f)),
        _ => String::new(),
    }
}

/// What a snapshot's live state looks like versus its file.
pub enum SnapshotState {
    /// No `dconf` on this host — the snapshot can't be evaluated (a Mac).
    NoDconf,
    /// The snapshot file doesn't exist yet — nothing has been captured.
    Uncaptured,
    /// Key-level differences (empty = in sync).
    Diffs(Vec<KeyDiff>),
}

/// Compare one declared snapshot against live dconf, filtering **both** sides
/// through its `strip` list so a stripped key never reads as drift.
pub fn snapshot_state(home: &Path, snap: &DconfSnapshot) -> Result<SnapshotState> {
    if which("dconf").is_none() {
        return Ok(SnapshotState::NoDconf);
    }
    let src = home.join(&snap.file);
    if !src.is_file() {
        return Ok(SnapshotState::Uncaptured);
    }
    let out = Command::new("dconf")
        .args(["dump", &snap.path])
        .output()
        .with_context(|| format!("dconf dump {}", snap.path))?;
    if !out.status.success() {
        bail!("dconf dump {} failed", snap.path);
    }
    let live = strip_dump(&String::from_utf8_lossy(&out.stdout), &snap.strip);
    // The file was written filtered, but re-filter it: a `strip` entry added
    // after the last capture would otherwise show its stale keys as drift.
    let file = strip_dump(
        &fs::read_to_string(&src).with_context(|| format!("reading {}", src.display()))?,
        &snap.strip,
    );
    Ok(SnapshotState::Diffs(diff_dumps(&file, &live)))
}

/// The raw, **unfiltered** `dconf dump` of a subtree. Unfiltered is what undo
/// needs: a restore must be revertible to the machine's exact prior state, not
/// to a strip-filtered approximation of it.
fn dump_raw(path: &str) -> Result<String> {
    let out = Command::new("dconf")
        .args(["dump", path])
        .output()
        .with_context(|| format!("dconf dump {path}"))?;
    if !out.status.success() {
        bail!("dconf dump {path} failed");
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Capture each of the machine's dconf snapshots (filtered) to its file,
/// journaled so `undo` reverts them. Returns the written paths. No-op where
/// `dconf` is absent — callers that should fail loudly check first.
pub fn capture(home: &Path, machine: &Machine, journal: &mut Journal) -> Result<Vec<PathBuf>> {
    if machine.dconf.is_empty() || which("dconf").is_none() {
        return Ok(Vec::new());
    }
    let mut written = Vec::new();
    for snap in &machine.dconf {
        let filtered = strip_dump(&dump_raw(&snap.path)?, &snap.strip);
        let dest = home.join(&snap.file);
        if let Some(p) = dest.parent() {
            fs::create_dir_all(p).with_context(|| format!("creating {}", p.display()))?;
        }
        let before = fs::read(&dest).ok();
        fs::write(&dest, &filtered).with_context(|| format!("writing {}", dest.display()))?;
        journal.record_write(&dest, before.as_deref(), filtered.as_bytes())?;
        written.push(dest);
    }
    Ok(written)
}

/// Load each snapshot file back into live dconf (in declared order — so a shared
/// snapshot then a per-machine one layers correctly). Caller confirms first.
///
/// Journaled per subtree so `undo` reverts a restore: the machine's prior
/// **unfiltered** state is stored before each load. `dry_run` reports the files
/// it would load and touches nothing.
pub fn restore(home: &Path, machine: &Machine, dry_run: bool) -> Result<Vec<PathBuf>> {
    if machine.dconf.is_empty() {
        return Ok(Vec::new());
    }
    if which("dconf").is_none() {
        bail!("dconf not found — cannot restore a dconf snapshot on this host");
    }
    let mut loaded = Vec::new();
    // Per-snapshot progress: a restore overwrites live desktop state, so which
    // paths were actually loaded is worth naming rather than totalling. No child
    // output to fight here (dconf load is silent), so the region is always
    // welcome — but a dry run has no effects to report, so it gets none
    // (Principle #6b: temper reports what happened, and nothing did).
    let cl = (!dry_run).then(|| crate::ui::Checklist::new(machine.dconf.len(), "restoring", false));
    let mut journal = Journal::begin();
    for snap in &machine.dconf {
        if let Some(cl) = &cl {
            cl.start(&snap.path);
        }
        let src = home.join(&snap.file);
        let content = fs::read_to_string(&src)
            .with_context(|| format!("reading snapshot {}", src.display()))?;
        if dry_run {
            loaded.push(src);
            continue;
        }
        // Capture the prior state BEFORE the load — this is the undo payload.
        let before = dump_raw(&snap.path)?;
        let mut child = Command::new("dconf")
            .args(["load", &snap.path])
            .stdin(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawning dconf load {}", snap.path))?;
        child
            .stdin
            .take()
            .context("dconf load stdin unavailable")?
            .write_all(content.as_bytes())
            .with_context(|| format!("piping snapshot into dconf load {}", snap.path))?;
        if !child.wait()?.success() {
            bail!("dconf load {} failed", snap.path);
        }
        // Guard on the FILTERED dump: a live subtree churns on its own (mru
        // lists, window state), so hashing the raw dump would make every undo
        // skip within minutes. `strip` is already the manifest's declaration of
        // which keys are meaningful — reuse it rather than invent a second rule.
        let after = strip_dump(&dump_raw(&snap.path)?, &snap.strip);
        journal.record_dconf_tree(&snap.path, &snap.strip, before.as_bytes(), &after)?;
        if let Some(cl) = &cl {
            cl.done(&format!("dconf {} ← {}", snap.path, snap.file));
        }
        loaded.push(src);
    }
    if let Some(cl) = cl {
        cl.finish();
    }
    if !dry_run {
        journal.commit()?;
    }
    Ok(loaded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_drops_matching_keys_and_empty_sections() {
        let dump = "\
[/]
enabled-extensions=['a']
last-selected-power-profile='balanced'

[org/gnome/shell/monitors]
panel-1='x'

[org/gnome/desktop]
clock-format='24h'
";
        // Strip the bookkeeping key and the whole per-monitor section.
        let out = strip_dump(dump, &["last-selected".into(), "monitors/".into()]);
        assert!(out.contains("enabled-extensions"), "kept real key: {out}");
        assert!(out.contains("clock-format"));
        assert!(
            !out.contains("last-selected"),
            "stripped bookkeeping: {out}"
        );
        assert!(!out.contains("panel-1"), "stripped monitor key: {out}");
        // The now-empty monitors section header is gone too.
        assert!(!out.contains("monitors"), "empty section dropped: {out}");
    }

    #[test]
    fn no_strip_is_identity_ish() {
        let dump = "[/]\nkey='v'\n";
        let out = strip_dump(dump, &[]);
        assert!(out.contains("[/]") && out.contains("key='v'"));
    }

    #[test]
    fn parse_ids_root_keys_bare_and_nests_the_rest() {
        let m = parse_dump("[/]\na='1'\n\n[org/gnome/desktop]\nb='2'\n");
        assert_eq!(m.get(&(String::new(), "a".into())).unwrap(), "'1'");
        assert_eq!(
            m.get(&("org/gnome/desktop".into(), "b".into())).unwrap(),
            "'2'"
        );
        // The id form matches what `strip` filters on.
        assert_eq!(key_id("", "a"), "a");
        assert_eq!(key_id("org/gnome/desktop", "b"), "org/gnome/desktop/b");
    }

    #[test]
    fn diff_reports_changed_missing_and_extra() {
        let file = "[shell]\nsame='x'\nchanged='old'\ngone='y'\n";
        let live = "[shell]\nsame='x'\nchanged='new'\nfresh='z'\n";
        let d = diff_dumps(file, live);
        // An identical key is not drift.
        assert!(!d.iter().any(|k| k.key == "same"), "{d:?}");

        let by = |k: &str| d.iter().find(|x| x.key == k).unwrap().clone();
        assert_eq!(by("changed").status(), "changed");
        // In the spec, not on the machine.
        assert_eq!(by("gone").status(), "missing");
        // On the machine, not captured.
        assert_eq!(by("fresh").status(), "extra");
        assert_eq!(by("fresh").id(), "shell/fresh");
    }

    #[test]
    fn identical_dumps_have_no_drift() {
        let d = "[a]\nk='v'\n\n[b]\nj='w'\n";
        assert!(diff_dumps(d, d).is_empty());
    }

    #[test]
    fn grouping_yields_one_group_per_extension() {
        // A snapshot rooted at /org/gnome/shell/extensions/ dumps one section
        // per extension — so per-extension prompts need no GNOME knowledge.
        let file = "[blur-my-shell]\nradius=30\n\n[dash-to-dock]\ndock-position='LEFT'\n";
        let live = "[blur-my-shell]\nradius=60\n\n[dash-to-dock]\ndock-position='BOTTOM'\n";
        let groups = group_by_section(&diff_dumps(file, live));
        let names: Vec<&str> = groups.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(names, vec!["blur-my-shell", "dash-to-dock"]);
        assert!(groups.iter().all(|(_, v)| v.len() == 1));
    }

    #[test]
    fn set_replaces_in_place_and_leaves_neighbours_alone() {
        let d = "[shell]\na='1'\nb='2'\n\n[desktop]\nc='3'\n";
        let out = dump_with_key(d, "shell", "a", "'9'");
        assert!(out.contains("a='9'"), "{out}");
        assert!(out.contains("b='2'") && out.contains("c='3'"), "{out}");
        assert_eq!(out.matches("a=").count(), 1, "no duplicate key: {out}");
    }

    #[test]
    fn set_appends_to_an_existing_section_and_creates_a_missing_one() {
        let d = "[shell]\na='1'\n";
        let out = dump_with_key(d, "shell", "new", "'v'");
        assert!(out.contains("a='1'") && out.contains("new='v'"), "{out}");

        let out = dump_with_key(d, "desktop/interface", "clock", "'24h'");
        assert!(out.contains("[desktop/interface]"), "{out}");
        assert!(out.contains("clock='24h'"), "{out}");
        // The root section keeps dconf's own `[/]` spelling.
        assert!(dump_with_key("", "", "k", "'v'").contains("[/]"));
    }

    #[test]
    fn remove_drops_the_key_and_any_section_it_emptied() {
        let d = "[shell]\na='1'\n\n[lonely]\nonly='x'\n";
        let out = dump_without_key(d, "lonely", "only");
        assert!(!out.contains("lonely"), "empty section dropped: {out}");
        assert!(out.contains("a='1'"), "{out}");

        let out = dump_without_key(d, "shell", "a");
        assert!(!out.contains("a='1'") && out.contains("only='x'"), "{out}");
    }

    #[test]
    fn edits_round_trip_through_the_parser() {
        // The written file must parse back to exactly the intended state —
        // this is what keeps a reconcile absorb from re-drifting next run.
        let d = "[shell]\na='1'\n";
        let out = dump_with_key(d, "shell", "b", "'2'");
        let m = parse_dump(&out);
        assert_eq!(m.get(&("shell".into(), "b".into())).unwrap(), "'2'");
        assert_eq!(m.len(), 2);
        assert!(diff_dumps(&out, &out).is_empty());
    }

    #[test]
    fn absorbing_every_diff_clears_the_drift() {
        // The invariant that makes reconcile trustworthy: absorb everything and
        // the next drift run is silent — no key re-offered forever.
        let file = "[shell]\nchanged='old'\ngone='y'\n";
        let live = "[shell]\nchanged='new'\nfresh='z'\n";
        let out = absorbed(file, &diff_dumps(file, live));
        assert!(
            diff_dumps(&out, live).is_empty(),
            "absorbed file still drifts: {out}"
        );
    }

    #[test]
    fn absorbing_a_subset_leaves_exactly_the_rest() {
        let file = "[shell]\na='1'\nb='2'\n";
        let live = "[shell]\na='9'\nb='8'\n";
        let diffs = diff_dumps(file, live);
        let only_a: Vec<KeyDiff> = diffs.iter().filter(|d| d.key == "a").cloned().collect();
        let out = absorbed(file, &only_a);
        let rest = diff_dumps(&out, live);
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].key, "b");
    }

    #[test]
    fn describe_renders_a_list_valued_key_as_a_member_delta() {
        // enabled-extensions is one key holding an array — one prompt, and the
        // change must read as members, not two walls of text.
        let d = KeyDiff {
            section: String::new(),
            key: "enabled-extensions".into(),
            file: Some("['a@x', 'b@y']".into()),
            live: Some("['a@x', 'c@z']".into()),
        };
        let s = describe(&d);
        assert!(s.starts_with("+1 −1"), "{s}");
        assert!(s.contains("+c@z") && s.contains("−b@y"), "{s}");
        // A non-list value falls back to old → new.
        let d = KeyDiff {
            section: "i".into(),
            key: "clock".into(),
            file: Some("'12h'".into()),
            live: Some("'24h'".into()),
        };
        assert_eq!(describe(&d), "'12h' → '24h'");
    }

    #[test]
    fn stripped_keys_never_read_as_drift() {
        // The churn key differs on both sides but is filtered out of each.
        let strip = vec!["last-selected".to_string()];
        let file = strip_dump("[/]\nk='v'\nlast-selected='a'\n", &strip);
        let live = strip_dump("[/]\nk='v'\nlast-selected='b'\n", &strip);
        assert!(diff_dumps(&file, &live).is_empty());
    }
}
