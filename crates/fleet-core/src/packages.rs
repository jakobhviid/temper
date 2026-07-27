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
            Manager::Brew | Manager::Cask => {
                self.name.rsplit('/').next().unwrap_or(&self.name).to_string()
            }
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
        Some(mas_id(rest)?)
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
        raw.extend(b.packages);
        match machine.os.as_str() {
            "mac" => raw.extend(b.packages_mac),
            "linux" => raw.extend(b.packages_linux),
            _ => {}
        }
    }
    raw.extend(machine.packages.clone());

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for line in raw {
        let pkg = parse(&line)?;
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
}

impl Installed {
    pub fn set(&mut self, m: Manager, names: impl IntoIterator<Item = String>) {
        self.by_manager.insert(m, names.into_iter().collect());
    }

    fn has(&self, m: Manager, name: &str) -> bool {
        self.by_manager.get(&m).is_some_and(|s| s.contains(name))
    }

    /// Whether this manager was probed at all (absent = not installed / skipped).
    pub fn probed(&self, m: Manager) -> bool {
        self.by_manager.contains_key(&m)
    }
}

fn ignore_list<'a>(ignore: &'a Ignore, m: Manager) -> &'a [String] {
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

    #[test]
    fn parse_tokens() {
        assert_eq!(parse("brew \"wget\"").unwrap().manager, Manager::Brew);
        let cask = parse("cask \"ublue-os/tap/1password-gui-linux\"").unwrap();
        assert_eq!(cask.manager, Manager::Cask);
        assert_eq!(cask.match_name(), "1password-gui-linux"); // tap prefix stripped
        let mas = parse("mas \"Xcode\", id: 497799835").unwrap();
        assert_eq!(mas.manager, Manager::Mas);
        assert_eq!(mas.match_name(), "497799835");
        assert_eq!(parse("vscode \"Rust-Lang.Rust\"").unwrap().match_name(), "rust-lang.rust");
        assert!(parse("bogus \"x\"").is_err());
    }

    #[test]
    fn missing_and_extras() {
        let declared = vec![
            parse("brew \"wget\"").unwrap(),
            parse("brew \"jq\"").unwrap(),
            parse("flatpak \"org.x.App\"").unwrap(),
        ];
        let mut installed = Installed::default();
        installed.set(Manager::Brew, [String::from("wget"), String::from("extra-tool")]);
        installed.set(Manager::Flatpak, [String::from("org.x.App")]);
        // flatpak not… actually declared+installed match → not missing

        let miss: Vec<_> = missing(&declared, &installed).iter().map(|p| p.name.clone()).collect();
        assert_eq!(miss, vec!["jq"]); // declared brew jq not installed

        let ignore = Ignore::default();
        let ex = extras(&declared, &installed, &ignore);
        assert_eq!(ex, vec![(Manager::Brew, "extra-tool".to_string())]);

        // ignoring it removes it from extras
        let ignore = Ignore { brew: vec!["extra-tool".into()], ..Default::default() };
        assert!(extras(&declared, &installed, &ignore).is_empty());
    }

    #[test]
    fn unprobed_manager_yields_no_missing() {
        let declared = vec![parse("mas \"Xcode\", id: 497799835").unwrap()];
        let installed = Installed::default(); // mas never probed
        assert!(missing(&declared, &installed).is_empty());
    }
}
