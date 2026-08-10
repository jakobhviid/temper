//! What the spec has deployed on this machine — the residue ledger.
//!
//! Packages self-clean: drop one from the spec and it becomes an extra, which
//! `prune` answers. Files did not, because nothing recorded them. Delete a
//! `copy` step and its file stayed on every machine forever, with no extras
//! direction to report it and no verb to remove it — column 11 of the feature
//! interface scoring zero for the largest primitive class in the tool.
//!
//! The asymmetry was never about files being special. It was that *enumerable*
//! state needs no bookkeeping and non-enumerable state does: temper can ask brew
//! what is installed, and cannot ask a filesystem which of its millions of files
//! temper put there. So it writes that down.
//!
//! The ledger is **machine state**, not folder content — it lives beside the
//! journal in the state root, so Principle #9 holds: the folder stays a folder
//! anyone can hand-edit, and nothing in it is tool-managed bookkeeping.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One deployed path and the content temper left there.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Deployed {
    /// blake3 of what temper wrote — the whole file for `copy`/`sysfile`, the
    /// region body for a `block`. The guard that makes removal safe.
    ///
    /// Empty means "temper could not read the source, so it cannot say what it
    /// would have written". Such an entry is never removable: an unknown hash
    /// must not read as a matching one.
    pub hash: String,
    /// Which primitive owns it, for the report.
    pub kind: String,
    /// The filesystem path, which is **not** the map key: a `block`'s identity
    /// is `(file, marker)`, so two blocks in one `.zshrc` need two entries and
    /// one path. Keying by path alone collided them, and the loser was never
    /// tracked at all.
    #[serde(default)]
    pub path: String,
    /// For a `block`, the marker naming its region. A block's residue is the
    /// region, not the file: the file belongs to the user, so retiring one is an
    /// edit and never a delete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
}

impl Deployed {
    /// The ledger key for a deployed thing: its path, plus the marker where the
    /// path alone is not an identity.
    pub fn key(path: &str, marker: Option<&str>) -> String {
        match marker {
            Some(m) => format!("{path}#{m}"),
            None => path.to_string(),
        }
    }
}

/// key → what temper left there. The key is `Deployed::key`, not the bare path.
pub type Ledger = BTreeMap<String, Deployed>;

fn path_for(machine: &str) -> PathBuf {
    crate::journal::state_root()
        .join("deployed")
        .join(format!("{machine}.json"))
}

pub fn load(machine: &str) -> Ledger {
    let mut l: Ledger = std::fs::read_to_string(path_for(machine))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    // A ledger written before `path` existed keyed every entry by its path, so
    // the key is the path. Recovering it here keeps an older machine's residue
    // trackable instead of quietly forgotten.
    for (key, rec) in l.iter_mut() {
        if rec.path.is_empty() {
            rec.path = key.split('#').next().unwrap_or(key).to_string();
        }
    }
    l
}

/// Replace the ledger for a machine. Written whole rather than appended: the
/// spec's current set of deployed paths IS the ledger, so a converge that stops
/// declaring something must stop recording it, or the residue would be reported
/// forever.
pub fn save(machine: &str, ledger: &Ledger) -> Result<()> {
    let p = path_for(machine);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    }
    let body = serde_json::to_string_pretty(ledger)?;
    std::fs::write(&p, body).with_context(|| format!("writing {}", p.display()))
}

/// Entries the ledger holds that the spec no longer declares — the extras
/// direction for deployed files.
///
/// `declared` holds ledger keys, and they are compared **resolved**, not as
/// written: `~/.config/x` and `/home/you/.config/x` name one file, and a spec
/// edit that only re-spells a target must not make the still-declared file look
/// like residue. It did, and `prune` deleted it.
pub fn residue(machine: &str, declared: &[String]) -> Vec<(String, Deployed)> {
    let resolved: Vec<PathBuf> = declared.iter().map(|d| resolve_key(d)).collect();
    load(machine)
        .into_iter()
        .filter(|(k, _)| {
            let mine = resolve_key(k);
            !resolved.contains(&mine)
        })
        .collect()
}

/// A ledger key resolved for comparison: the path expanded, the marker kept so
/// two regions in one file stay distinct.
fn resolve_key(key: &str) -> PathBuf {
    match key.split_once('#') {
        Some((path, marker)) => crate::manifest::expand_tilde(path).join(format!("#{marker}")),
        None => crate::manifest::expand_tilde(key),
    }
}

/// Whether the thing this record describes is still on disk — the file, or for
/// a `block`, its region.
///
/// Residue that is already gone is not residue. Reporting it anyway produced
/// permanent drift that no verb could clear: `prune` removed the file and left
/// the record, `prune` then correctly found nothing to remove, and `drift` went
/// on accusing the user of having edited a file that no longer existed.
pub fn still_present(rec: &Deployed) -> bool {
    let p = crate::manifest::expand_tilde(&rec.path);
    match &rec.marker {
        Some(marker) => std::fs::read_to_string(&p)
            .ok()
            .and_then(|t| crate::primitives::block_removed(&t, marker).ok().flatten())
            .is_some(),
        None => p.symlink_metadata().is_ok(),
    }
}

/// Whether a residue file is still exactly what temper deployed.
///
/// This is the whole safety story for removing it, and it is the same guard
/// `undo` uses: remove what temper left untouched, and **report** anything the
/// user has since edited rather than deleting their work. A ledger is a record,
/// not a licence.
pub fn is_untouched(rec: &Deployed) -> bool {
    if rec.hash.is_empty() {
        // temper never learned what it would have written there, so it cannot
        // claim the file is untouched. Reported, never removed.
        return false;
    }
    let expanded = crate::manifest::expand_tilde(&rec.path);
    let Ok(bytes) = std::fs::read(Path::new(&expanded)) else {
        // Already gone: nothing to remove, and nothing to warn about.
        return false;
    };
    match &rec.marker {
        // A block: compare the REGION, not the file. The rest of the file is the
        // user's and will have changed for reasons that are none of our business.
        Some(marker) => {
            let text = String::from_utf8_lossy(&bytes);
            match crate::primitives::block_removed(&text, marker) {
                Ok(Some((_, body))) => blake3::hash(body.as_bytes()).to_hex().to_string() == rec.hash,
                _ => false,
            }
        }
        None => blake3::hash(&bytes).to_hex().to_string() == rec.hash,
    }
}

/// Remove one piece of residue: a whole file, or a block's region.
///
/// Never a delete for a block — the file belongs to the user and only the
/// marker-delimited region was ever temper's.
pub fn remove(rec: &Deployed) -> Result<()> {
    let p = crate::manifest::expand_tilde(&rec.path);
    match &rec.marker {
        Some(marker) => {
            let text = std::fs::read_to_string(&p)
                .with_context(|| format!("reading {}", p.display()))?;
            if let Some((without, _)) = crate::primitives::block_removed(&text, marker)? {
                std::fs::write(&p, without)
                    .with_context(|| format!("writing {}", p.display()))?;
            }
            Ok(())
        }
        None => remove_path(&p),
    }
}

/// Remove a path, whatever kind it is, escalating only when the unprivileged
/// attempt is refused.
///
/// `remove_file` alone covered neither case that matters: a `sysfile` deploys
/// into root-owned `/etc`, and a retired target is as likely to be a directory
/// (`~/.config/old-app`) as a file. Both failed, and prune counted them removed.
pub fn remove_path(p: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(p)
        .with_context(|| format!("reading {}", p.display()))?;
    let unprivileged = if meta.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        // A symlink is removed as a link — never followed, or retiring a link
        // would delete whatever it points at.
        std::fs::remove_file(p)
    };
    match unprivileged {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            // Root-owned residue is temper's to remove — it placed it, with
            // `sudo install`. Same escalation, backwards.
            let ok = std::process::Command::new("sudo")
                .arg("rm")
                .arg("-rf")
                .arg("--")
                .arg(p)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                Ok(())
            } else {
                Err(anyhow::anyhow!(
                    "removing {} needs root and the escalation failed",
                    p.display()
                ))
            }
        }
        Err(e) => Err(e).with_context(|| format!("removing {}", p.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_state<T>(f: impl FnOnce() -> T) -> T {
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("TEMPER_STATE_DIR", d.path());
        let out = f();
        std::env::remove_var("TEMPER_STATE_DIR");
        out
    }

    /// The ledger round-trips, and residue is "recorded but no longer declared".
    #[test]
    fn residue_is_what_the_spec_stopped_declaring() {
        with_state(|| {
            let mut l = Ledger::new();
            l.insert(
                "~/.config/kept".into(),
                Deployed { hash: "h1".into(), kind: "copy".into(), path: "~/.config/kept".into(), marker: None },
            );
            l.insert(
                "~/.config/dropped".into(),
                Deployed { hash: "h2".into(), kind: "copy".into(), path: "~/.config/dropped".into(), marker: None },
            );
            save("m", &l).unwrap();
            assert_eq!(load("m"), l);

            let declared = vec!["~/.config/kept".to_string()];
            let r = residue("m", &declared);
            assert_eq!(r.len(), 1);
            assert_eq!(r[0].0, "~/.config/dropped");

            // Declaring everything leaves no residue…
            assert!(residue("m", &["~/.config/kept".into(), "~/.config/dropped".into()]).is_empty());
            // …and a machine with no ledger has none either.
            assert!(residue("unknown-machine", &[]).is_empty());
        })
    }

    /// A block's residue is its region, and removing it leaves the file.
    ///
    /// This is why `block` could not simply join `copy` in the ledger: the file
    /// belongs to the user — a `.zshrc` — and only the marker-delimited region
    /// was ever temper's. Recording the path and deleting it would have been the
    /// single most destructive thing in the tool.
    #[test]
    fn removing_a_block_edits_the_file_rather_than_deleting_it() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("zshrc");
        let body = "source ~/.image";
        let text = format!(
            "# mine before\n\n# >>> temper:img >>>\n{body}\n# <<< temper:img <<<\n# mine after\n"
        );
        std::fs::write(&f, &text).unwrap();
        let rec = Deployed {
            hash: blake3::hash(body.as_bytes()).to_hex().to_string(),
            kind: "block".into(),
            path: f.to_string_lossy().to_string(),
            marker: Some("img".into()),
        };
        assert!(is_untouched(&rec), "an unedited region is removable");

        remove(&rec).unwrap();
        let after = std::fs::read_to_string(&f).expect("the file must still exist");
        assert!(!after.contains("temper:img"), "the region should be gone");
        assert!(after.contains("# mine before"), "the user's content must survive");
        assert!(after.contains("# mine after"), "…on both sides of the region");
    }

    /// An edited region is reported, not removed — the file half of the guard
    /// applies to the region half too.
    #[test]
    fn an_edited_block_region_is_not_untouched() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("zshrc");
        std::fs::write(
            &f,
            "# >>> temper:img >>>\nthe user changed this\n# <<< temper:img <<<\n",
        )
        .unwrap();
        let rec = Deployed {
            hash: blake3::hash(b"source ~/.image").to_hex().to_string(),
            kind: "block".into(),
            path: f.to_string_lossy().to_string(),
            marker: Some("img".into()),
        };
        assert!(!is_untouched(&rec));
    }

    /// A file the user has edited since deployment is NOT removable.
    ///
    /// Without this the ledger becomes a licence to delete: temper would remove
    /// work it did not do, on the strength of a record that only says what temper
    /// once wrote there.
    #[test]
    fn an_edited_file_is_not_untouched() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("x");
        std::fs::write(&f, b"original").unwrap();
        let rec = Deployed {
            hash: blake3::hash(b"original").to_hex().to_string(),
            kind: "copy".into(),
            path: f.to_string_lossy().to_string(),
            marker: None,
        };
        assert!(is_untouched(&rec));

        std::fs::write(&f, b"the user changed this").unwrap();
        assert!(!is_untouched(&rec), "an edited file must be reported, not removed");

        std::fs::remove_file(&f).unwrap();
        assert!(!is_untouched(&rec), "an absent file is nothing to remove");
        assert!(!still_present(&rec), "…and it is not residue any more either");
    }
}
