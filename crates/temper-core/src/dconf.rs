//! Whole-desktop dconf snapshots (machine scope): the RIS `gnome-backup` /
//! `gnome-restore` (and Ptyxis) pair. `backup` dumps a dconf subtree through a
//! strip-keys filter into a file in the folder; `restore` loads it back into
//! live dconf. Restore is a distinct, confirm-gated verb — never part of
//! `update` — because reloading a snapshot clobbers live tweaks (which is
//! exactly why RIS excludes it from its update flow).
//!
//! Degrades on a host without `dconf` (e.g. a Mac): backup is a no-op, restore
//! errors loudly.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::journal::Journal;
use crate::manifest::Machine;
use crate::primitives::which;

/// Filter a `dconf dump` block, dropping any `key=value` whose `section/key`
/// path contains one of the `strip` substrings, and any section left empty by
/// that filtering. Pure — the testable heart of a filtered backup.
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
            section = t[1..t.len() - 1].trim_matches('/').to_string();
        } else if t.is_empty() {
            continue;
        } else if let Some((k, _)) = line.split_once('=') {
            let key = k.trim();
            let id = if section.is_empty() {
                key.to_string()
            } else {
                format!("{section}/{key}")
            };
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

/// Dump each of the machine's dconf snapshots (filtered) to its file, journaled
/// so `undo` reverts them. Returns the written paths. No-op where `dconf` is
/// absent.
pub fn backup(home: &Path, machine: &Machine, journal: &mut Journal) -> Result<Vec<PathBuf>> {
    if machine.dconf.is_empty() || which("dconf").is_none() {
        return Ok(Vec::new());
    }
    let mut written = Vec::new();
    for snap in &machine.dconf {
        let out = Command::new("dconf")
            .args(["dump", &snap.path])
            .output()
            .with_context(|| format!("dconf dump {}", snap.path))?;
        if !out.status.success() {
            bail!("dconf dump {} failed", snap.path);
        }
        let filtered = strip_dump(&String::from_utf8_lossy(&out.stdout), &snap.strip);
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
pub fn restore(home: &Path, machine: &Machine) -> Result<Vec<PathBuf>> {
    if machine.dconf.is_empty() {
        return Ok(Vec::new());
    }
    if which("dconf").is_none() {
        bail!("dconf not found — cannot restore a dconf snapshot on this host");
    }
    let mut loaded = Vec::new();
    for snap in &machine.dconf {
        let src = home.join(&snap.file);
        let content = fs::read_to_string(&src)
            .with_context(|| format!("reading snapshot {}", src.display()))?;
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
        loaded.push(src);
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
        let out = strip_dump(&dump.to_string(), &["last-selected".into(), "monitors/".into()]);
        assert!(out.contains("enabled-extensions"), "kept real key: {out}");
        assert!(out.contains("clock-format"));
        assert!(!out.contains("last-selected"), "stripped bookkeeping: {out}");
        assert!(!out.contains("panel-1"), "stripped monitor key: {out}");
        // The now-empty monitors section header is gone too.
        assert!(!out.contains("monitors"), "empty section dropped: {out}");
    }

    #[test]
    fn no_strip_is_identity_ish() {
        let dump = "[/]\nkey='v'\n";
        let out = strip_dump(&dump.to_string(), &[]);
        assert!(out.contains("[/]") && out.contains("key='v'"));
    }
}
