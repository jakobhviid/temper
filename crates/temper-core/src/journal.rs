//! Content-addressed, after-hash-guarded undo — lifted from amdl's model.
//!
//! A mutating run writes `runs/<id>/manifest.json` + a content-addressed
//! `objects/` store under the state dir (`$TEMPER_STATE_DIR`, else the platform
//! state dir). Each entry is a minimal inverse:
//!
//! - `Create` — temper created the file; undo deletes it if it still hashes to
//!   what temper wrote.
//! - `Restore` — temper overwrote an existing file; undo restores the prior
//!   bytes if the file still hashes to what temper left.
//! - `DconfKey` — a `setkey(dconf)` write; undo restores the prior value (or
//!   resets a previously-unset key), guarded on the live value.
//! - `DconfTree` — a whole-subtree `dconf load` (`restore`); undo resets the
//!   subtree and reloads the prior dump, guarded on the strip-filtered live dump.
//!
//! Every revert is guarded by an after check: if the target changed since, the
//! entry is skipped, never clobbered.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};

pub fn state_root() -> PathBuf {
    if let Ok(d) = std::env::var("TEMPER_STATE_DIR") {
        return PathBuf::from(d);
    }
    if cfg!(target_os = "macos") {
        if let Ok(h) = std::env::var("HOME") {
            return PathBuf::from(h).join("Library/Application Support/temper");
        }
    }
    if let Ok(d) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(d).join("temper");
    }
    if let Ok(h) = std::env::var("HOME") {
        return PathBuf::from(h).join(".local/state/temper");
    }
    PathBuf::from(".temper-state")
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op")]
enum Entry {
    Create {
        path: String,
        hash: String,
    },
    Restore {
        path: String,
        before: String,
        after: String,
    },
    /// A `setkey(dconf)` write. `before` = prior `dconf read` (None if the key
    /// was unset), `after` = the value temper wrote (the revert guard). Undo
    /// re-writes `before`, or resets the key when it was previously unset.
    DconfKey {
        key: String,
        before: Option<String>,
        after: String,
    },
    /// A whole-subtree `dconf load` (`restore`). `before` = the object hash of
    /// the **unfiltered** dump taken just before the load, so undo restores the
    /// machine's exact prior state; `after` = the hash of the **strip-filtered**
    /// dump just after it, the revert guard (a raw-dump guard would go stale
    /// within minutes of normal desktop churn). `strip` is stored so undo can
    /// reproduce that same filter.
    DconfTree {
        path: String,
        #[serde(default)]
        strip: Vec<String>,
        before: String,
        after: String,
    },
    /// Packages this run **newly installed** — not upgraded.
    ///
    /// Reversing an install is the same operation the provider already offers
    /// backwards (`gext uninstall`, `rpm-ostree uninstall`), and the set is
    /// known before the converge runs, because temper computes what is missing
    /// in order to install it. So it was never that packages *could not* be
    /// journaled — nobody had written down why they weren't.
    ///
    /// Providers whose uninstall needs root (`mas`) prompt during `undo`, which
    /// is a user-invoked interactive command, so that is expected rather than a
    /// surprise.
    ///
    /// An **upgrade** is deliberately not recorded — reverting one means pinning
    /// a prior version whose bottle or commit may be gone. That applies to brew
    /// and flatpak only, the two things `update` upgrades. temper never runs
    /// `rpm-ostree upgrade` (the OS owns image updates, and layered packages ride
    /// the deployment) and never upgrades extensions, so for those the revert is
    /// unconditional.
    PackagesInstalled {
        /// The provider name from `interface::PROVIDERS`.
        provider: String,
        packages: Vec<String>,
    },
    /// Something this run changed that `undo` cannot take back — an `exec`, a
    /// `sysfile`, a `setkey(defaults)`, a tap-trust.
    ///
    /// Recorded so the RUN exists. A converge whose only changes were
    /// unrevertible used to journal nothing at all, so no run directory was
    /// written, and a later bare `temper undo` picked the newest run it *could*
    /// see — some earlier, unrelated one — reverted that, and reported success.
    /// The user asked to undo what just happened and got something else undone.
    Unrevertible { what: String },
    /// An op written by a NEWER temper than the one reading. Undo skips and
    /// reports it rather than failing to parse the whole run — a downgrade
    /// loses the ability to revert that entry, not the ability to revert at all.
    #[serde(other)]
    Unknown,
}

/// Un-install packages this run installed. Best-effort per provider: one that
/// fails must not strand the rest of the revert, and a provider whose tool is
/// gone reports rather than errors.
fn uninstall_packages(provider: &str, packages: &[String]) -> bool {
    if packages.is_empty() {
        return true;
    }
    let mut cmd = match provider {
        "gnome-extensions" => {
            let mut c = std::process::Command::new("gext");
            c.arg("uninstall");
            c
        }
        "rpm-ostree" => {
            let mut c = std::process::Command::new("rpm-ostree");
            // Stages a new deployment, exactly as layering did. No `-r`: temper
            // reports that a reboot is needed and never initiates one.
            c.args(["uninstall", "--idempotent", "-y"]);
            c
        }
        "flatpak" => {
            let mut c = std::process::Command::new("flatpak");
            // `--user` is the scope temper may safely remove from: a system app
            // belongs to the image or to root. It is NOT necessarily the scope
            // the install wrote to — `flatpak install` defaults to the system
            // installation — so on an image-based host this revert finds nothing
            // and says so. Deliberately narrow rather than wrong: see ROADMAP,
            // "Which flatpak installation temper owns".
            c.args(["uninstall", "-y", "--noninteractive", "--user"]);
            c
        }
        "brew" => {
            let mut c = std::process::Command::new("brew");
            c.args(["uninstall", "--formula"]);
            c
        }
        "cask" => {
            let mut c = std::process::Command::new("brew");
            c.args(["uninstall", "--cask"]);
            c
        }
        "vscode" => {
            // One flag per extension, so this is handled below rather than by
            // appending bare args.
            let mut c = std::process::Command::new("code");
            for p in packages {
                c.arg("--uninstall-extension").arg(p);
            }
            return c
                .stdout(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
        }
        "mas" => {
            // `mas uninstall` takes the numeric id — which is exactly what
            // `Pkg::match_name` yields for this manager, so the recorded set is
            // already in the right shape. It removes from /Applications and so
            // needs root; `undo` is a user-invoked interactive command, so a
            // prompt here is expected rather than a surprise.
            let mut c = std::process::Command::new("sudo");
            c.args(["mas", "uninstall"]);
            c
        }
        _ => return false,
    };
    for p in packages {
        cmd.arg(p);
    }
    cmd.stdout(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn dconf_read(key: &str) -> Option<String> {
    let out = std::process::Command::new("dconf")
        .args(["read", key])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}

fn dconf_write(key: &str, value: &str) -> bool {
    std::process::Command::new("dconf")
        .args(["write", key, value])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn dconf_reset(key: &str) -> bool {
    std::process::Command::new("dconf")
        .args(["reset", key])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn dconf_dump(path: &str) -> Option<String> {
    let out = std::process::Command::new("dconf")
        .args(["dump", path])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Reverting a subtree needs a **reset then load**, not a bare load: `dconf
/// load` merges, so replaying the prior dump would restore old values but leave
/// behind every key the restore newly introduced.
fn dconf_load_tree(path: &str, content: &[u8]) -> bool {
    use std::io::Write as _;
    if !std::process::Command::new("dconf")
        .args(["reset", "-f", path])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return false;
    }
    let Ok(mut child) = std::process::Command::new("dconf")
        .args(["load", path])
        .stdin(std::process::Stdio::piped())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        if stdin.write_all(content).is_err() {
            return false;
        }
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

#[derive(Serialize, Deserialize)]
struct RunFile {
    argv: Vec<String>,
    entries: Vec<Entry>,
}

pub struct Journal {
    root: PathBuf,
    id: String,
    entries: Vec<Entry>,
}

fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

impl Journal {
    pub fn begin() -> Journal {
        let d = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Journal {
            root: state_root(),
            id: format!("{}-{:09}", d.as_secs(), d.subsec_nanos()),
            entries: Vec::new(),
        }
    }

    /// Record that `path` is being written: `before` = its prior bytes (None if
    /// it didn't exist), `after` = the bytes temper is writing.
    pub fn record_write(&mut self, path: &Path, before: Option<&[u8]>, after: &[u8]) -> Result<()> {
        let path = path.to_string_lossy().into_owned();
        match before {
            None => self.entries.push(Entry::Create {
                path,
                hash: hash(after),
            }),
            Some(bytes) => {
                let before = self.store_object(bytes)?;
                self.entries.push(Entry::Restore {
                    path,
                    before,
                    after: hash(after),
                });
            }
        }
        Ok(())
    }

    /// Record a `setkey(dconf)` write for undo (`before` = prior value or None).
    /// Record packages a converge newly installed, so `undo` can remove them.
    /// A no-op on an empty set, so a run that installed nothing adds no entry.
    pub fn record_packages(&mut self, provider: &str, packages: &[String]) {
        if packages.is_empty() {
            return;
        }
        self.entries.push(Entry::PackagesInstalled {
            provider: provider.to_string(),
            packages: packages.to_vec(),
        });
    }

    pub fn record_dconf(&mut self, key: &str, before: Option<String>, after: String) {
        self.entries.push(Entry::DconfKey {
            key: key.to_string(),
            before,
            after,
        });
    }

    /// Record a whole-subtree `dconf load` for undo. `before` is the unfiltered
    /// prior dump (stored as an object); `after_filtered` is the post-load dump
    /// already put through `strip` — the guard.
    pub fn record_dconf_tree(
        &mut self,
        path: &str,
        strip: &[String],
        before: &[u8],
        after_filtered: &str,
    ) -> Result<()> {
        let before = self.store_object(before)?;
        self.entries.push(Entry::DconfTree {
            path: path.to_string(),
            strip: strip.to_vec(),
            before,
            after: hash(after_filtered.as_bytes()),
        });
        Ok(())
    }

    fn store_object(&self, bytes: &[u8]) -> Result<String> {
        let h = hash(bytes);
        let dir = self.root.join("objects");
        fs::create_dir_all(&dir)?;
        let p = dir.join(&h);
        if !p.exists() {
            fs::write(&p, bytes)?;
        }
        Ok(h)
    }

    /// Record that this run changed something `undo` cannot revert, so the run
    /// is still written and a later `undo` cannot silently walk past it.
    pub fn record_unrevertible(&mut self, what: &str) {
        self.entries.push(Entry::Unrevertible {
            what: what.to_string(),
        });
    }

    /// Write the manifest atomically (presence = committed). No-op if empty.
    pub fn commit(self) -> Result<()> {
        if self.entries.is_empty() {
            return Ok(());
        }
        let dir = self.root.join("runs").join(&self.id);
        fs::create_dir_all(&dir)?;
        let run = RunFile {
            argv: std::env::args().collect(),
            entries: self.entries,
        };
        let tmp = dir.join("manifest.json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(&run)?)?;
        fs::rename(&tmp, dir.join("manifest.json"))?;
        Ok(())
    }
}

fn newest_run(runs: &Path) -> Result<PathBuf> {
    if !runs.is_dir() {
        bail!("nothing to undo");
    }
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(runs)? {
        let entry = entry?;
        let p = entry.path();
        if !p.join("manifest.json").is_file() {
            continue;
        }
        let mtime = entry.metadata()?.modified()?;
        if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
            best = Some((mtime, p));
        }
    }
    best.map(|(_, p)| p)
        .ok_or_else(|| anyhow!("nothing to undo"))
}

/// Revertible run ids, newest first.
pub fn list_runs() -> Result<Vec<String>> {
    let runs = state_root().join("runs");
    if !runs.is_dir() {
        return Ok(Vec::new());
    }
    let mut v: Vec<(SystemTime, String)> = Vec::new();
    for entry in fs::read_dir(&runs)? {
        let entry = entry?;
        if !entry.path().join("manifest.json").is_file() {
            continue;
        }
        v.push((
            entry.metadata()?.modified()?,
            entry.file_name().to_string_lossy().into_owned(),
        ));
    }
    v.sort_by_key(|b| std::cmp::Reverse(b.0));
    Ok(v.into_iter().map(|(_, id)| id).collect())
}

/// Revert a run — the one named by `run` (its id), else the newest. `dry_run`
/// reports without touching anything. Returns (reverted, skipped). A guard
/// check (does the file still hash to what temper left?) and a missing content
/// object both cause that entry to be *skipped and reported*, never clobbered
/// or aborted mid-run.
pub fn undo(run: Option<&str>, dry_run: bool) -> Result<(usize, usize)> {
    let root = state_root();
    let runs = root.join("runs");
    let run_dir = match run {
        Some(id) => {
            let d = runs.join(id);
            if !d.join("manifest.json").is_file() {
                bail!("no revertible run '{id}' (see `temper undo --list`)");
            }
            d
        }
        None => newest_run(&runs)?,
    };
    let rf: RunFile = serde_json::from_slice(&fs::read(run_dir.join("manifest.json"))?)?;

    let (mut reverted, mut skipped) = (0usize, 0usize);
    // A revert is exactly the kind of phase that must name its items: "reverted 3,
    // skipped 2" leaves you unable to tell *which* two were left alone, and a skip
    // here means "the file changed since temper wrote it" — the single most
    // important thing to know before running it again. No children to stream, so
    // the spinner is never in anyone's way.
    let cl = crate::ui::Checklist::new(rf.entries.len(), "reverting", false);
    for entry in rf.entries.iter().rev() {
        // Package entries revert by un-installing exactly what this run added.
        if let Entry::PackagesInstalled { provider, packages } = entry {
            // A dry run touches nothing — the guard every other arm has, and the
            // one arm that shells out to a real `uninstall`.
            if dry_run {
                reverted += packages.len();
                cl.noted(&format!(
                    "would un-install {provider}: {} package(s)",
                    packages.len()
                ));
                continue;
            }
            if uninstall_packages(provider, packages) {
                reverted += packages.len();
                cl.done(&format!("{provider}: {} package(s)", packages.len()));
            } else {
                skipped += packages.len();
                cl.skipped(
                    &format!("{provider}: {} package(s)", packages.len()),
                    "could not un-install",
                );
            }
            continue;
        }
        // A recorded-but-unrevertible change: say what it was and move on. This
        // is the entry that makes the run visible at all.
        if let Entry::Unrevertible { what } = entry {
            skipped += 1;
            cl.skipped(what, "cannot be reverted");
            continue;
        }
        // dconf key entries guard on the live value, not a file hash.
        if let Entry::DconfKey { key, before, after } = entry {
            cl.start(&format!("dconf {key}"));
            if dconf_read(key).as_deref() != Some(after.as_str()) {
                skipped += 1; // changed since temper wrote it → don't clobber
                cl.skipped(&format!("dconf {key}"), "changed since temper wrote it");
                continue;
            }
            if dry_run {
                reverted += 1;
                cl.noted(&format!("would revert dconf {key}"));
                continue;
            }
            let done = match before {
                Some(v) => dconf_write(key, v),
                None => dconf_reset(key),
            };
            if done {
                reverted += 1;
                cl.done(&format!("dconf {key}"));
            } else {
                skipped += 1;
                cl.skipped(&format!("dconf {key}"), "dconf write failed");
            }
            continue;
        }

        // A whole-subtree restore guards on the strip-filtered live dump.
        if let Entry::DconfTree {
            path,
            strip,
            before,
            after,
        } = entry
        {
            let live = dconf_dump(path).map(|d| crate::dconf::strip_dump(&d, strip));
            if live.as_deref().map(|d| hash(d.as_bytes())) != Some(after.clone()) {
                skipped += 1; // desktop changed since the restore → don't clobber
                continue;
            }
            if dry_run {
                reverted += 1;
                continue;
            }
            match fs::read(root.join("objects").join(before)) {
                Ok(bytes) if dconf_load_tree(path, &bytes) => reverted += 1,
                _ => skipped += 1,
            }
            continue;
        }

        // Written by a newer temper — nothing this binary can invert.
        if matches!(entry, Entry::Unknown) {
            skipped += 1;
            continue;
        }

        let (path, expect_after) = match entry {
            Entry::Create { path, hash } => (path, hash),
            Entry::Restore { path, after, .. } => (path, after),
            _ => unreachable!("handled above"),
        };
        cl.start(path);
        let p = PathBuf::from(path);
        let current = if p.is_file() { fs::read(&p).ok() } else { None };
        // Only revert if the file still hashes to what temper left it as.
        if !current
            .as_deref()
            .is_some_and(|b| hash(b).as_str() == expect_after.as_str())
        {
            skipped += 1;
            cl.skipped(
                path,
                if current.is_none() {
                    "gone since temper wrote it"
                } else {
                    "changed since temper wrote it"
                },
            );
            continue;
        }
        if dry_run {
            reverted += 1;
            cl.noted(&format!("would revert {path}"));
            continue;
        }
        let done = match entry {
            Entry::Create { .. } => fs::remove_file(&p).is_ok(),
            Entry::Restore { before, .. } => {
                // A missing object is a skip, not a fatal abort mid-run.
                match fs::read(root.join("objects").join(before)) {
                    Ok(bytes) => fs::write(&p, bytes).is_ok(),
                    Err(_) => false,
                }
            }
            _ => unreachable!("handled above"),
        };
        if done {
            reverted += 1;
            cl.done(path);
        } else {
            skipped += 1;
            cl.skipped(path, "could not restore the saved content");
        }
    }
    cl.finish();
    // Keep the run dir if anything was skipped, so it can be inspected/retried.
    if !dry_run && skipped == 0 {
        fs::remove_dir_all(&run_dir).ok();
    }
    Ok((reverted, skipped))
}

#[cfg(test)]
mod tests {
    /// A package entry round-trips, and an empty install adds none.
    ///
    /// The claim that packages "cannot" be journaled was never examined: the set
    /// temper installs is known *before* the converge, because temper computes
    /// what is missing in order to install it, and every provider's uninstall is
    /// the same operation backwards. What is genuinely not revertible is an
    /// UPGRADE — reverting one means pinning a prior version whose bottle or
    /// commit may be gone — so only new installs are recorded.
    #[test]
    fn package_installs_are_recorded_and_empty_ones_are_not() {
        let mut j = Journal::begin();
        j.record_packages("gnome-extensions", &[]);
        assert!(j.entries.is_empty(), "an empty install must add no entry");

        j.record_packages("rpm-ostree", &["vpn".to_string(), "vpn-gui".to_string()]);
        match &j.entries[0] {
            Entry::PackagesInstalled { provider, packages } => {
                assert_eq!(provider, "rpm-ostree");
                assert_eq!(packages.len(), 2);
            }
            _ => panic!("expected a package entry"),
        }

        // Serialises and parses back — the on-disk manifest is the contract, and
        // an older temper must skip it as Unknown rather than fail the whole run.
        let json = serde_json::to_string(&j.entries[0]).unwrap();
        assert!(json.contains("PackagesInstalled"), "{json}");
        let back: Entry = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, Entry::PackagesInstalled { .. }));
    }

    /// An unknown provider is refused rather than shelled out blindly.
    #[test]
    fn an_unknown_provider_is_not_uninstalled() {
        assert!(!uninstall_packages("not-a-provider", &["x".to_string()]));
        // …and an empty set is trivially fine, without running anything.
        assert!(uninstall_packages("not-a-provider", &[]));
    }

    use super::*;

    #[test]
    fn an_op_from_a_newer_temper_parses_as_unknown() {
        // Downgrade safety: a manifest containing an op this binary has never
        // heard of must still load, so the *rest* of the run stays revertible.
        // Without `#[serde(other)]` this fails to parse and `undo` dies whole.
        let rf: RunFile = serde_json::from_str(
            r#"{"argv":["temper"],"entries":[
                 {"op":"Create","path":"/tmp/x","hash":"abc"},
                 {"op":"SomeFutureOp","whatever":42}
               ]}"#,
        )
        .expect("unknown op must not fail the whole manifest");
        assert_eq!(rf.entries.len(), 2);
        assert!(matches!(rf.entries[1], Entry::Unknown));
    }

    #[test]
    fn a_dconf_tree_entry_round_trips() {
        let e = Entry::DconfTree {
            path: "/org/gnome/shell/".into(),
            strip: vec!["monitors/".into()],
            before: "objhash".into(),
            after: "guardhash".into(),
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"op\":\"DconfTree\""), "{s}");
        let back: Entry = serde_json::from_str(&s).unwrap();
        match back {
            Entry::DconfTree {
                path, strip, after, ..
            } => {
                assert_eq!(path, "/org/gnome/shell/");
                assert_eq!(strip, vec!["monitors/".to_string()]);
                assert_eq!(after, "guardhash");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn the_tree_guard_hashes_the_filtered_dump_not_the_raw_one() {
        // Why it matters: a live subtree churns constantly, so a raw-dump guard
        // would go stale within minutes and every undo would silently skip.
        let strip = vec!["last-selected".to_string()];
        let raw_a = "[/]\nk='v'\nlast-selected='a'\n";
        let raw_b = "[/]\nk='v'\nlast-selected='b'\n";
        assert_ne!(hash(raw_a.as_bytes()), hash(raw_b.as_bytes()));
        assert_eq!(
            hash(crate::dconf::strip_dump(raw_a, &strip).as_bytes()),
            hash(crate::dconf::strip_dump(raw_b, &strip).as_bytes()),
            "churn in a stripped key must not invalidate the undo guard"
        );
    }
}
