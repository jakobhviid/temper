//! Package model: parse Brewfile-grammar tokens, aggregate a machine's
//! effective set (union of composed apps + loose list), and compute drift
//! (missing / extras) against an installed snapshot.
//!
//! This is the PURE, unit-tested core. The actual install + `brew list`-style
//! probing that produces the `Installed` snapshot lives in `providers` (shell
//! outs, VM-verified) — kept separate so the set logic is testable anywhere.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::manifest::{self, Ignore, Machine};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Manager {
    Brew,
    Cask,
    Tap,
    Flatpak,
    Mas,
    Vscode,
}

impl Manager {
    pub fn as_str(self) -> &'static str {
        match self {
            Manager::Brew => "brew",
            Manager::Cask => "cask",
            Manager::Tap => "tap",
            Manager::Flatpak => "flatpak",
            Manager::Mas => "mas",
            Manager::Vscode => "vscode",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkg {
    pub manager: Manager,
    pub name: String,
    pub id: Option<String>, // mas only
    pub raw: String,        // original token, for materializing a Brewfile
}

impl Pkg {
    /// The name drift matches against an installed snapshot: brew/cask strip the
    /// tap prefix, vscode is case-insensitive, mas is its numeric id.
    pub fn match_name(&self) -> String {
        match self.manager {
            Manager::Brew | Manager::Cask => self
                .name
                .rsplit('/')
                .next()
                .unwrap_or(&self.name)
                .to_string(),
            Manager::Vscode => self.name.to_lowercase(),
            Manager::Mas => self.id.clone().unwrap_or_default(),
            Manager::Flatpak | Manager::Tap => self.name.clone(),
        }
    }

    fn dedup_key(&self) -> (Manager, String) {
        (self.manager, self.match_name())
    }
}

fn first_quoted(s: &str) -> Result<String> {
    let start = s.find('"').context("expected a quoted name")?;
    let rest = &s[start + 1..];
    let end = rest.find('"').context("unterminated quoted name")?;
    Ok(rest[..end].to_string())
}

fn mas_id(s: &str) -> Result<String> {
    let at = s.find("id:").context("mas entry needs `id:`")?;
    let digits: String = s[at + 3..]
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        bail!("mas entry has no numeric id");
    }
    Ok(digits)
}

/// Parse one Brewfile-grammar token: `brew "x"`, `cask "y"`, `tap "u/r"`,
/// `flatpak "id"`, `vscode "ext"`, `mas "Name", id: 123`.
pub fn parse(line: &str) -> Result<Pkg> {
    let line = line.trim();
    let (kind, rest) = line
        .split_once(char::is_whitespace)
        .with_context(|| format!("malformed package token `{line}`"))?;
    let name = first_quoted(rest)?;
    let manager = match kind {
        "brew" => Manager::Brew,
        "cask" => Manager::Cask,
        "tap" => Manager::Tap,
        "flatpak" => Manager::Flatpak,
        "vscode" => Manager::Vscode,
        "mas" => Manager::Mas,
        other => bail!("unknown package type `{other}`"),
    };
    let id = if manager == Manager::Mas {
        // Look for `id:` only AFTER the name's closing quote, so an "id:" inside
        // the quoted name (e.g. `mas "Bid: 5 stars", id: 9`) isn't misread.
        let after_name = rest
            .match_indices('"')
            .nth(1)
            .map_or(rest, |(i, _)| &rest[i + 1..]);
        Some(mas_id(after_name)?)
    } else {
        None
    };
    Ok(Pkg {
        manager,
        name,
        id,
        raw: line.to_string(),
    })
}

/// The machine's declared effective set: union of its composed apps' packages
/// (OS-scoped) plus its loose list, de-duplicated. (Ignore is applied later,
/// only when detecting extras — it never removes a declared package.)
pub fn effective_set(home: &Path, machine: &Machine) -> Result<Vec<Pkg>> {
    let mut raw: Vec<String> = Vec::new();
    for app in &machine.apps {
        let b = manifest::load_bundle(home, app)?;
        // Gate packages like every other bundle-level list. Skipping this made
        // the gate cover two of the five ways a bundle carries machine-specific
        // content: an `os = "linux"` bundle's `flatpak` lines landed in a Mac's
        // effective set, where they are permanently missing and the remediation
        // drift names cannot help. Silent, green, and wrong.
        if manifest::gated(&b.os, &b.role, machine) {
            continue;
        }
        raw.extend(b.packages);
        match machine.os.as_str() {
            "mac" => raw.extend(b.packages_mac),
            "linux" => raw.extend(b.packages_linux),
            _ => {}
        }
    }
    raw.extend(machine.packages.clone());

    // A referenced Brewfile: each non-comment, non-blank line is a package token.
    // A declared file that doesn't exist yet contributes nothing rather than
    // erroring — that's the seed case (`init`, or declaring `brewfile` before
    // running `reconcile --current-state-wins`), where it's about to be created.
    if let Some(bf) = &machine.brewfile {
        let path = home.join(bf);
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                return Err(e).with_context(|| format!("reading brewfile {}", path.display()))
            }
        };
        for line in content.lines() {
            let l = line.trim();
            if !l.is_empty() && !l.starts_with('#') {
                raw.push(l.to_string());
            }
        }
    }

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for line in raw {
        // Name the offending token so a malformed line (e.g. a bare
        // `mas "12345"` with no `id:`) is findable, not an opaque global error.
        let pkg = parse(&line).with_context(|| format!("in package token `{line}`"))?;
        if seen.insert(pkg.dedup_key()) {
            out.push(pkg);
        }
    }
    Ok(out)
}

/// A snapshot of what's installed, per manager (names already normalized the
/// way `match_name` produces them). Produced by `providers` on a real machine.
#[derive(Debug, Default)]
pub struct Installed {
    pub by_manager: HashMap<Manager, HashSet<String>>,
    /// Managers whose tool was present and **failed**. Distinct from absent
    /// (never asked) and from present-and-empty (asked, answered none): only
    /// this one means the machine's state is unknown, and a drop computed from
    /// an unknown is a spec deletion.
    pub unavailable: std::collections::BTreeSet<Manager>,
}

impl Installed {
    pub fn set(&mut self, m: Manager, names: impl IntoIterator<Item = String>) {
        self.by_manager.insert(m, names.into_iter().collect());
    }

    /// Record that this manager could not be read. Deliberately does **not**
    /// insert into `by_manager`, so `probed()` stays false and every
    /// missing/extras/drop computation treats it as no evidence at all.
    pub fn unavailable(&mut self, m: Manager) {
        self.unavailable.insert(m);
    }

    /// Managers that were asked and could not answer — what `drift` reports as
    /// `unavailable` rather than passing over in silence (Principle #6).
    pub fn unavailable_managers(&self) -> impl Iterator<Item = Manager> + '_ {
        self.unavailable.iter().copied()
    }

    fn has(&self, m: Manager, name: &str) -> bool {
        self.by_manager.get(&m).is_some_and(|s| s.contains(name))
    }

    /// Public form of `has` for callers outside this module (reconcile).
    pub fn contains(&self, m: Manager, name: &str) -> bool {
        self.has(m, name)
    }

    /// Whether this manager was probed at all (absent = not installed / skipped).
    pub fn probed(&self, m: Manager) -> bool {
        self.by_manager.contains_key(&m)
    }
}

/// Every manager, so a caller can iterate them instead of listing them.
///
/// A hand-written enumeration is how `prune`'s ignore protection came to cover
/// five of six managers and nobody noticed: the compiler checks a `match`, and
/// checks nothing about a sequence of `for` loops.
impl Manager {
    pub const ALL: &'static [Manager] = &[
        Manager::Brew,
        Manager::Cask,
        Manager::Tap,
        Manager::Flatpak,
        Manager::Mas,
        Manager::Vscode,
    ];
}

/// The journal provider name for a manager, or `None` when its installs are
/// deliberately not journaled.
///
/// Exhaustive on purpose: this decides what `undo` can take back, and it used to
/// be a hand-written list of five that silently omitted `Tap`. A manager added
/// later would have been installed and never recorded, so `undo` would report
/// success having left it in place. Every `Some` here must have an uninstall arm
/// in `journal::uninstall_packages` — a test holds the two together.
pub fn journal_provider(m: Manager) -> Option<&'static str> {
    match m {
        Manager::Brew => Some("brew"),
        Manager::Cask => Some("cask"),
        Manager::Flatpak => Some("flatpak"),
        Manager::Mas => Some("mas"),
        Manager::Vscode => Some("vscode"),
        // A tap is not installed, it is *tapped*, and it comes and goes with the
        // Brewfile that names it. `brew bundle cleanup --tap` untaps an orphan,
        // so there is no separate uninstall to record — and `brew untap` appears
        // nowhere in the tree precisely because nothing needs it.
        Manager::Tap => None,
    }
}

/// The `[ignore]` list for a manager. Exhaustive, so a new manager cannot be
/// added without answering this.
pub fn ignore_list(ignore: &Ignore, m: Manager) -> &[String] {
    match m {
        Manager::Brew => &ignore.brew,
        Manager::Cask => &ignore.cask,
        Manager::Flatpak => &ignore.flatpak,
        Manager::Mas => &ignore.mas,
        Manager::Vscode => &ignore.vscode,
        Manager::Tap => &ignore.tap,
    }
}

/// Declared packages not present in the installed snapshot (per manager). Only
/// considers managers that were actually probed.
pub fn missing<'a>(declared: &'a [Pkg], installed: &Installed) -> Vec<&'a Pkg> {
    declared
        .iter()
        .filter(|p| installed.probed(p.manager) && !installed.has(p.manager, &p.match_name()))
        .collect()
}

/// Installed packages not declared and not ignored (per manager). NOTE: for
/// brew this is a naive set-diff; the dependency-aware `brew bundle cleanup`
/// (which won't flag a kept package's transitive deps) is applied by the brew
/// provider on a real machine. Exact for flatpak/vscode/mas.
pub fn extras(declared: &[Pkg], installed: &Installed, ignore: &Ignore) -> Vec<(Manager, String)> {
    let mut out = Vec::new();
    for (&m, names) in &installed.by_manager {
        let declared_here: HashSet<String> = declared
            .iter()
            .filter(|p| p.manager == m)
            .map(|p| p.match_name())
            .collect();
        let ignored: HashSet<&str> = ignore_list(ignore, m).iter().map(String::as_str).collect();
        for name in names {
            if !declared_here.contains(name) && !ignored.contains(name.as_str()) {
                out.push((m, name.clone()));
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Manager::ALL` really is all of them.
    ///
    /// The `match` below is exhaustive, so adding a variant stops this
    /// compiling until it is handled — and the count then forces `ALL` to be
    /// updated too. Together that makes every `for &m in Manager::ALL` loop
    /// complete by construction, which is what `prune`'s ignore protection
    /// depends on: it enumerated five of six managers by hand, and the compiler
    /// checks a match while checking nothing about a sequence of `for` loops.
    #[test]
    fn manager_all_lists_every_variant() {
        fn seen(m: Manager) -> u8 {
            match m {
                Manager::Brew => 0,
                Manager::Cask => 1,
                Manager::Tap => 2,
                Manager::Flatpak => 3,
                Manager::Mas => 4,
                Manager::Vscode => 5,
            }
        }
        let mut marks: Vec<u8> = Manager::ALL.iter().map(|m| seen(*m)).collect();
        marks.sort_unstable();
        assert_eq!(
            marks,
            (0..=5).collect::<Vec<u8>>(),
            "Manager::ALL is missing a variant or repeats one"
        );
    }

    /// Every manager's `[ignore]` list is reachable, so nothing can be ignored
    /// in the spec and pruned anyway.
    #[test]
    fn every_manager_has_a_reachable_ignore_list() {
        let ignore = Ignore {
            brew: vec!["b".into()],
            cask: vec!["c".into()],
            tap: vec!["t".into()],
            flatpak: vec!["f".into()],
            mas: vec!["m".into()],
            vscode: vec!["v".into()],
            ..Default::default()
        };
        for &m in Manager::ALL {
            assert_eq!(
                ignore_list(&ignore, m).len(),
                1,
                "`{}` has no reachable ignore list",
                m.as_str()
            );
        }
    }

    #[test]
    fn parse_tokens() {
        assert_eq!(parse("brew \"wget\"").unwrap().manager, Manager::Brew);
        let cask = parse("cask \"ublue-os/tap/1password-gui-linux\"").unwrap();
        assert_eq!(cask.manager, Manager::Cask);
        assert_eq!(cask.match_name(), "1password-gui-linux"); // tap prefix stripped
        let mas = parse("mas \"Xcode\", id: 497799835").unwrap();
        assert_eq!(mas.manager, Manager::Mas);
        assert_eq!(mas.match_name(), "497799835");
        assert_eq!(
            parse("vscode \"Rust-Lang.Rust\"").unwrap().match_name(),
            "rust-lang.rust"
        );
        assert!(parse("bogus \"x\"").is_err());
        // `id:` inside the quoted name must not be mistaken for the id.
        assert_eq!(
            parse("mas \"Bid: 5 stars\", id: 999").unwrap().match_name(),
            "999"
        );
    }

    #[test]
    fn missing_and_extras() {
        let declared = vec![
            parse("brew \"wget\"").unwrap(),
            parse("brew \"jq\"").unwrap(),
            parse("flatpak \"org.x.App\"").unwrap(),
        ];
        let mut installed = Installed::default();
        installed.set(
            Manager::Brew,
            [String::from("wget"), String::from("extra-tool")],
        );
        installed.set(Manager::Flatpak, [String::from("org.x.App")]);
        // flatpak not… actually declared+installed match → not missing

        let miss: Vec<_> = missing(&declared, &installed)
            .iter()
            .map(|p| p.name.clone())
            .collect();
        assert_eq!(miss, vec!["jq"]); // declared brew jq not installed

        let ignore = Ignore::default();
        let ex = extras(&declared, &installed, &ignore);
        assert_eq!(ex, vec![(Manager::Brew, "extra-tool".to_string())]);

        // ignoring it removes it from extras
        let ignore = Ignore {
            brew: vec!["extra-tool".into()],
            ..Default::default()
        };
        assert!(extras(&declared, &installed, &ignore).is_empty());
    }

    #[test]
    fn unprobed_manager_yields_no_missing() {
        let declared = vec![parse("mas \"Xcode\", id: 497799835").unwrap()];
        let installed = Installed::default(); // mas never probed
        assert!(missing(&declared, &installed).is_empty());
    }
}
