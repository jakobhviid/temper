//! Locate the temper-home folder — the directory holding `temper.toml`.
//!
//! Resolution order: `$TEMPER_DIR` (explicit) → walk up from the cwd (you're
//! inside a folder) → a saved pointer (`temper use` writes it) → an auto-scan of
//! common locations. temper is delivery-agnostic: it never runs git or a sync
//! client — it only needs a path that contains a manifest.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

fn has_manifest(dir: &Path) -> bool {
    dir.join("temper.toml").is_file()
}

pub fn find_home() -> Result<PathBuf> {
    // 1. Explicit override.
    if let Ok(d) = std::env::var("TEMPER_DIR") {
        let p = PathBuf::from(d);
        if has_manifest(&p) {
            return Ok(p);
        }
        if !p.is_dir() {
            bail!(
                "TEMPER_DIR={} does not exist — moved? point it at the new place, \
                 or unset it to let temper search",
                p.display()
            );
        }
        bail!("TEMPER_DIR={} has no temper.toml", p.display());
    }
    // 2. Walk up from the cwd (you're inside a temper folder).
    let mut dir = std::env::current_dir()?;
    loop {
        if has_manifest(&dir) {
            return Ok(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    // 3. A saved pointer (your default home; `temper setup <dir>` writes it).
    //    One that leads nowhere is kept, not dropped: the folder was moved, and
    //    that is the first thing the reader needs to hear.
    let base = config_base();
    let pointer = base.as_deref().and_then(read_pointer_in);
    if let Some(p) = pointer.as_ref().filter(|p| has_manifest(p)) {
        return Ok(p.clone());
    }
    // 4. Auto-scan common locations (git checkout / cloud folder / USB). One
    //    match → use it; several → refuse and let the user choose (a fleet may
    //    have more than one library — never silently guess).
    let candidates = scan_candidates();
    if let (Some(old), Some(base)) = (pointer, base) {
        return relocate(&old, candidates, &base);
    }
    match candidates.len() {
        // Nothing found anywhere temper looks, so the reader is in one of two
        // situations and the message has to serve both. It used to name only
        // `temper setup`, which *picks* an existing folder — a dead end for the
        // larger group, who have just installed temper and have no folder at
        // all. `init` is the verb that creates one, and it went unmentioned.
        // It also says where it looked, so a folder sitting one level out of
        // reach reads as exactly that rather than as temper being broken.
        0 => bail!(
            "no temper folder found (looked for {SCAN_WHERE}).\n\
             \x20 have one already? point temper at it: `temper setup <dir>`, or set \
             TEMPER_DIR, or cd into it.\n\
             \x20 starting out?      `temper init` scaffolds one here from what this \
             machine already has."
        ),
        1 => Ok(candidates.into_iter().next().expect("len==1")),
        _ => bail!(
            "found several temper-homes — run `temper setup` to choose one \
             (or set TEMPER_DIR):\n{}",
            list(&candidates)
        ),
    }
}

/// Resolve when the saved folder is gone. Each answer opens by saying so: the
/// plain "no temper folder found" reads as if this machine was never set up,
/// which sends the reader to `init` and a second, empty folder.
fn relocate(old: &Path, candidates: Vec<PathBuf>, base: &Path) -> Result<PathBuf> {
    let gone = moved_note(old);
    match candidates.len() {
        0 => bail!(
            "{gone} — moved?\n\
             \x20 temper looked for {SCAN_WHERE}, and found nothing.\n\
             \x20 point it at the new place: `temper setup <dir>`, or set TEMPER_DIR."
        ),
        // The one folder every verb would resolve to anyway, so re-pinning it is
        // not a guess — and it makes this note show once, not on every run.
        1 => {
            let new = candidates.into_iter().next().expect("len==1");
            let how = match save_pointer_in(base, &new) {
                Ok(_) => "saved it as the temper home".to_string(),
                Err(_) => format!("using it (`temper setup {}` pins it)", new.display()),
            };
            eprintln!(
                "{} {gone} — found one at {}, {how}",
                crate::ui::yellow(crate::ui::g_warn()),
                new.display()
            );
            Ok(new)
        }
        _ => bail!(
            "{gone} — moved? found several temper-homes; run `temper setup` to choose \
             one (or set TEMPER_DIR):\n{}",
            list(&candidates)
        ),
    }
}

/// "your saved temper folder … is gone", worded for whether the directory left
/// or only its manifest did.
pub fn moved_note(old: &Path) -> String {
    let what = if old.is_dir() { "has no temper.toml" } else { "is gone" };
    format!("your saved temper folder {} {what}", old.display())
}

fn list(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| format!("  {}", p.display())).collect::<Vec<_>>().join("\n")
}

/// The config base (`$XDG_CONFIG_HOME`, else `~/.config`) that holds temper's
/// saved-home pointer. `None` when neither is set.
fn config_base() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}

/// The saved-pointer file under a config base (`<base>/temper/home`). Base is a
/// param so the round-trip is testable without mutating process-global env.
fn pointer_in(base: &Path) -> PathBuf {
    base.join("temper").join("home")
}

/// The path the saved pointer under `base` names, whether or not a folder is
/// still there.
fn read_pointer_in(base: &Path) -> Option<PathBuf> {
    let s = fs::read_to_string(pointer_in(base)).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| PathBuf::from(s))
}

/// The saved pointer's path when it no longer leads to a temper folder — moved,
/// renamed or deleted since `temper setup` saved it.
pub fn stale_pointer() -> Option<PathBuf> {
    read_pointer_in(&config_base()?).filter(|p| !has_manifest(p))
}

/// Persist `dir` as the default temper-home under `base`. Returns the pointer
/// file written.
fn save_pointer_in(base: &Path, dir: &Path) -> Result<PathBuf> {
    if !has_manifest(dir) {
        bail!(
            "{} has no temper.toml — not saving it as the temper home",
            dir.display()
        );
    }
    let p = pointer_in(base);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    let abs = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    fs::write(&p, abs.display().to_string()).with_context(|| format!("writing {}", p.display()))?;
    Ok(p)
}

/// Persist `dir` as the default temper-home. Returns the pointer file written.
pub fn save_pointer(dir: &Path) -> Result<PathBuf> {
    let base = config_base().context("cannot resolve a config dir (no HOME/XDG_CONFIG_HOME)")?;
    save_pointer_in(&base, dir)
}

/// Every common location that contains a `temper.toml` (a git checkout, a synced
/// cloud folder, a mounted disk — the ways a folder tends to arrive), in a stable
/// order and de-duplicated. `setup` builds its picker from this; `find_home` uses
/// it to auto-resolve a lone match and refuse an ambiguous several.
pub fn scan_candidates() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    scan_in(&home)
}

/// Parent folders a checkout tends to live in, matched case-insensitively
/// against the children of `~`: `Developer` is the macOS convention, while a
/// Linux box as often has `developer`, `dev`, `src`, `code`, … — so detection
/// doesn't hinge on which name, or which case, a given machine picked.
const DEV_PARENTS: &[&str] = &[
    "developer", "dev", "src", "code", "projects", "project", "repos", "repo", "git",
    "sources", "work", "workspace",
];

/// How many folders deep the scan looks under each kind of root. A dev parent
/// is often organised a level or two (`~/Developer/Private/steel`, ghq's
/// `~/src/github.com/<owner>/<repo>`); a sync client holds a folder at its top
/// or one level in (`Apps/`, `Documents/`); a mounted disk sits one or two
/// levels below the mount root (`/media/$USER/<disk>/…`).
const DEV_DEPTH: usize = 3;
const CLOUD_DEPTH: usize = 2;
const MEDIA_DEPTH: usize = 3;

/// Where the scan looks, in words — for the messages that report it found nothing.
pub const SCAN_WHERE: &str = "a temper.toml directly in ~, up to three folders deep \
     in ~/Developer, ~/dev, ~/src and the like and on mounted disks, and up to two \
     deep in cloud-sync folders";

/// The scan over a given home root — split out so it's testable without touching
/// the process-global `$HOME` (which is racy under parallel tests).
///
/// A candidate is any folder holding a `temper.toml`, whatever it is called.
fn scan_in(home: &Path) -> Vec<PathBuf> {
    let mut roots: Vec<(PathBuf, usize)> = vec![(home.to_path_buf(), 1)];
    roots.extend(
        subdirs(home)
            .into_iter()
            .filter(|d| dir_name(d).is_some_and(|n| DEV_PARENTS.contains(&n.to_lowercase().as_str())))
            .map(|d| (d, DEV_DEPTH)),
    );
    // Cloud-sync roots — the same set dotsync probes, so a folder that arrives
    // via any common sync client is found on both macOS and Linux:
    // macOS's unified `~/Library/CloudStorage/*` clients (OneDrive, GoogleDrive,
    // Dropbox, … each a subdir), iCloud Drive, and the classic home-root dirs.
    roots.extend(subdirs(&home.join("Library/CloudStorage")).into_iter().map(|d| (d, CLOUD_DEPTH)));
    roots.push((home.join("Library/Mobile Documents/com~apple~CloudDocs"), CLOUD_DEPTH)); // iCloud
    roots.extend(
        ["Nextcloud", "Dropbox", "OneDrive", "ProtonDrive", "Proton Drive", "Google Drive", "Sync"]
            .iter()
            .map(|d| (home.join(d), CLOUD_DEPTH)),
    );
    roots.extend([
        (PathBuf::from("/media"), MEDIA_DEPTH),
        (PathBuf::from("/run/media").join(std::env::var("USER").unwrap_or_default()), MEDIA_DEPTH),
    ]);
    let mut found = Vec::new();
    for (root, depth) in roots {
        walk(&root, depth, &mut found);
    }
    // Dedup by *canonical* path, not the path string: on a case-insensitive FS
    // (macOS) `~/Developer/steel` and `~/developer/steel` are the same directory,
    // `/home` → `/var/home` symlinks alias too, and `~` and a dev parent both
    // reach the parent's own children — string-equality would list one real
    // home several times and make `find_home` cry "several homes".
    let mut out = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for cand in found {
        let key = cand.canonicalize().unwrap_or_else(|_| cand.clone());
        if !seen.contains(&key) {
            seen.push(key);
            out.push(cand);
        }
    }
    out
}

/// Collect every folder holding a `temper.toml` up to `depth` levels below `dir`.
/// A match is not descended into, and neither is a git checkout: whatever a
/// repo holds — an example or a test fixture with a `temper.toml` in it —
/// belongs to that repo, not to this machine. Stopping at repos is also what
/// keeps a three-deep walk of `~/Developer` cheap.
fn walk(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    for child in subdirs(dir) {
        if has_manifest(&child) {
            found.push(child);
        } else if !child.join(".git").exists() {
            walk(&child, depth - 1, found);
        }
    }
}

/// The subdirectories of `dir`, sorted so the scan's order is stable. Hidden
/// ones are skipped — caches and tool state live there — except `.temper`, the
/// name for a home kept out of sight.
fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| dir_name(p).is_some_and(|n| !n.starts_with('.') || n == ".temper"))
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    out
}

fn dir_name(p: &Path) -> Option<&str> {
    p.file_name()?.to_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_round_trip() {
        // Pass the config base as a param — no `set_var("XDG_CONFIG_HOME")`,
        // which is process-global (racy under parallel tests) and `unsafe` in
        // Rust 2024. A temp base means this never touches the real config.
        let base = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        fs::write(home.path().join("temper.toml"), "").unwrap();
        let written = save_pointer_in(base.path(), home.path()).unwrap();
        assert!(written.exists());
        let read = read_pointer_in(base.path()).unwrap();
        assert!(has_manifest(&read));
    }

    #[test]
    fn save_refuses_dir_without_manifest() {
        let empty = tempfile::TempDir::new().unwrap();
        assert!(save_pointer(empty.path()).is_err());
    }

    #[test]
    fn scan_finds_lowercase_developer_and_other_dev_parents() {
        // The regression: a checkout under a lowercase `~/developer` (or `~/dev`,
        // `~/src`, …) must be found, not only the macOS-cased `~/Developer`.
        for parent in ["developer", "Developer", "dev", "src", "code", "projects"] {
            let home = tempfile::TempDir::new().unwrap();
            let steel = home.path().join(parent).join("steel");
            fs::create_dir_all(&steel).unwrap();
            fs::write(steel.join("temper.toml"), "").unwrap();
            let found = scan_in(home.path());
            assert!(
                found.contains(&steel),
                "scan missed {}: got {found:?}",
                steel.display()
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn scan_dedups_aliased_roots() {
        // Two roots that resolve to the SAME directory (a case-insensitive macOS
        // FS aliases `Developer`/`developer`; here a symlink stands in) must yield
        // ONE candidate — not "several homes". Regression for the macOS dup bug.
        let home = tempfile::TempDir::new().unwrap();
        let steel = home.path().join("Developer").join("steel");
        fs::create_dir_all(&steel).unwrap();
        fs::write(steel.join("temper.toml"), "").unwrap();
        // `dev`, not `developer`: on a case-insensitive FS the latter IS `Developer`.
        std::os::unix::fs::symlink(home.path().join("Developer"), home.path().join("dev"))
            .unwrap();
        let found = scan_in(home.path());
        assert_eq!(found.len(), 1, "aliased roots must dedup to one: {found:?}");
    }

    #[test]
    fn scan_finds_cloud_sync_locations() {
        // A folder arriving via any common sync client must be found: a named
        // home-root client, iCloud Drive, and a macOS CloudStorage subdir.
        for rel in [
            "OneDrive/steel",
            "Proton Drive/steel",
            "Nextcloud/temper-home",
            "Library/Mobile Documents/com~apple~CloudDocs/steel",
            "Library/CloudStorage/OneDrive-Personal/steel",
        ] {
            let home = tempfile::TempDir::new().unwrap();
            let d = home.path().join(rel);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("temper.toml"), "").unwrap();
            let found = scan_in(home.path());
            assert!(found.contains(&d), "scan missed {rel}: got {found:?}");
        }
    }

    #[test]
    fn scan_finds_home_and_alt_repo_names() {
        let home = tempfile::TempDir::new().unwrap();
        // directly under ~ and under a named repo dir, both variants supported
        for rel in ["steel", "developer/temper-home", "dev/.temper"] {
            let d = home.path().join(rel);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("temper.toml"), "").unwrap();
        }
        let found = scan_in(home.path());
        assert_eq!(found.len(), 3, "expected all three, got {found:?}");
    }

    fn manifest_at(home: &Path, rel: &str) -> PathBuf {
        let d = home.join(rel);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("temper.toml"), "").unwrap();
        d
    }

    #[test]
    fn scan_finds_a_folder_nested_in_a_dev_parent_under_any_name() {
        // An organised dev folder (`~/Developer/Private/steel`) and ghq's
        // `~/src/<host>/<owner>/<repo>` layout, and a name nobody listed.
        for rel in [
            "Developer/Private/steel",
            "src/github.com/someone/fleet",
            "code/my-machines",
            "Nextcloud/Apps/temper",
        ] {
            let home = tempfile::TempDir::new().unwrap();
            let d = manifest_at(home.path(), rel);
            let found = scan_in(home.path());
            assert_eq!(found, vec![d], "scan for {rel}");
        }
    }

    #[test]
    fn scan_stops_at_depth_repos_and_hidden_folders() {
        let home = tempfile::TempDir::new().unwrap();
        let h = home.path();
        manifest_at(h, "Developer/a/b/c/steel"); // four deep: past DEV_DEPTH
        manifest_at(h, "Nextcloud/a/b/steel"); // three deep: past CLOUD_DEPTH
        manifest_at(h, "Developer/.cache/steel"); // hidden
        manifest_at(h, "Developer/tool/examples/fleet"); // inside a repo…
        fs::create_dir_all(h.join("Developer/tool/.git")).unwrap(); // …this one
        manifest_at(h, "Videos/steel"); // not a dev parent: only ~'s own children count
        assert_eq!(scan_in(h), Vec::<PathBuf>::new());
    }

    #[test]
    fn a_checkout_that_is_itself_a_home_is_found() {
        // The repo-is-a-leaf rule must not hide a home that is a git checkout.
        let home = tempfile::TempDir::new().unwrap();
        let d = manifest_at(home.path(), "Developer/Private/steel");
        fs::create_dir_all(d.join(".git")).unwrap();
        manifest_at(home.path(), "Developer/Private/steel/examples/x"); // not a second home
        assert_eq!(scan_in(home.path()), vec![d]);
    }
}
