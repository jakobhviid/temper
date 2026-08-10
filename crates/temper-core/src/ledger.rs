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
    pub hash: String,
    /// Which primitive owns it, for the report.
    pub kind: String,
    /// For a `block`, the marker naming its region. A block's residue is the
    /// region, not the file: the file belongs to the user, so retiring one is an
    /// edit and never a delete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
}

/// path → what temper left there.
pub type Ledger = BTreeMap<String, Deployed>;

fn path_for(machine: &str) -> PathBuf {
    crate::journal::state_root()
        .join("deployed")
        .join(format!("{machine}.json"))
}

pub fn load(machine: &str) -> Ledger {
    std::fs::read_to_string(path_for(machine))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
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

/// Paths the ledger holds that the spec no longer declares — the extras
/// direction for deployed files.
pub fn residue(machine: &str, declared: &[String]) -> Vec<(String, Deployed)> {
    load(machine)
        .into_iter()
        .filter(|(p, _)| !declared.iter().any(|d| d == p))
        .collect()
}

/// Whether a residue file is still exactly what temper deployed.
///
/// This is the whole safety story for removing it, and it is the same guard
/// `undo` uses: remove what temper left untouched, and **report** anything the
/// user has since edited rather than deleting their work. A ledger is a record,
/// not a licence.
pub fn is_untouched(path: &str, rec: &Deployed) -> bool {
    let expanded = crate::manifest::expand_tilde(path);
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
pub fn remove(path: &str, rec: &Deployed) -> Result<()> {
    let p = crate::manifest::expand_tilde(path);
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
        None => std::fs::remove_file(&p)
            .with_context(|| format!("removing {}", p.display())),
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
                Deployed { hash: "h1".into(), kind: "copy".into(), marker: None },
            );
            l.insert(
                "~/.config/dropped".into(),
                Deployed { hash: "h2".into(), kind: "copy".into(), marker: None },
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
            marker: Some("img".into()),
        };
        let p = f.to_string_lossy().to_string();
        assert!(is_untouched(&p, &rec), "an unedited region is removable");

        remove(&p, &rec).unwrap();
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
            marker: Some("img".into()),
        };
        assert!(!is_untouched(&f.to_string_lossy(), &rec));
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
            marker: None,
        };
        let p = f.to_string_lossy().to_string();
        assert!(is_untouched(&p, &rec));

        std::fs::write(&f, b"the user changed this").unwrap();
        assert!(!is_untouched(&p, &rec), "an edited file must be reported, not removed");

        std::fs::remove_file(&f).unwrap();
        assert!(!is_untouched(&p, &rec), "an absent file is nothing to remove");
    }
}
