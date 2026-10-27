//! The host's own package manager: dnf (Fedora, RHEL and its rebuilds), apt
//! (Debian, Ubuntu) and pacman (Arch).
//!
//! Three providers with one shape, so one module: a declared list per manager,
//! one batched install, one batched removal, and two reads — what is installed,
//! and what a person asked for. Each manager is its own feature with its own
//! kinds and its own spec field, because package names are not portable between
//! families and a shared list would be a lie on two of the three.
//!
//! Two decisions carry the module, and both were measured rather than reasoned.
//!
//! **Extras are counted from a baseline.** All three managers record which
//! packages were asked for by name, apart from the dependencies they pulled in
//! (`apt-mark showmanual`, `dnf repoquery --userinstalled`, `pacman -Qqe`). But a
//! stock image already counts its own base as asked-for: 106–123 packages on
//! Debian, 211–225 on Ubuntu (`apt`, `bash` and `dpkg` among them), 39 on
//! Fedora, about 20 on EL, 17 on Arch. Diffing that against a spec would report
//! the base system as extras and offer `prune` the means to remove it. So the
//! first converge that manages a manager on a machine records what was already
//! asked for, and only what is asked for after that is ever an extra. The
//! baseline is evidence about the machine, like the ledger, and lives beside it
//! in the state directory.
//!
//! **Removal never takes anything with it.** All three remove whatever depends
//! on the package they are told to remove (apt and dnf on their own; pacman
//! refuses without `-c`). A removal that would take an installed dependant along
//! is held back and reported rather than run, which is decided by the manager's
//! own simulation before anything is confirmed — so the preview is the effect.
//!
//! temper never upgrades through these managers. On a mutable distro the OS
//! owns versions, as it does on an atomic one, and keeping to installs is what
//! keeps every converge revertible.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::manifest::{self, Bundle, Ignore, Machine};
use crate::primitives::which;

/// One of the host package managers temper drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SysPm {
    Dnf,
    Apt,
    Pacman,
}

impl SysPm {
    pub const ALL: &'static [SysPm] = &[SysPm::Dnf, SysPm::Apt, SysPm::Pacman];

    /// The provider name — also the spec field, the `[ignore]` key and the
    /// journal provider, so one word means one thing everywhere.
    pub fn name(self) -> &'static str {
        match self {
            SysPm::Dnf => "dnf",
            SysPm::Apt => "apt",
            SysPm::Pacman => "pacman",
        }
    }

    pub fn from_name(name: &str) -> Option<SysPm> {
        SysPm::ALL.iter().copied().find(|p| p.name() == name)
    }

    /// The `Finding.kind` for a declared-but-absent package (`extra = false`) or
    /// an asked-for-but-undeclared one (`extra = true`).
    ///
    /// Literal strings in a match, because the kind-coverage test reads them
    /// through this function rather than scraping source for them.
    pub fn kind(self, extra: bool) -> &'static str {
        match (self, extra) {
            (SysPm::Dnf, false) => "dnf",
            (SysPm::Dnf, true) => "dnf-extra",
            (SysPm::Apt, false) => "apt",
            (SysPm::Apt, true) => "apt-extra",
            (SysPm::Pacman, false) => "pacman",
            (SysPm::Pacman, true) => "pacman-extra",
        }
    }

    pub fn machine_list(self, m: &Machine) -> &[String] {
        match self {
            SysPm::Dnf => &m.dnf,
            SysPm::Apt => &m.apt,
            SysPm::Pacman => &m.pacman,
        }
    }

    pub fn bundle_list(self, b: &Bundle) -> &[String] {
        match self {
            SysPm::Dnf => &b.dnf,
            SysPm::Apt => &b.apt,
            SysPm::Pacman => &b.pacman,
        }
    }

    pub fn ignore_list(self, i: &Ignore) -> &[String] {
        match self {
            SysPm::Dnf => &i.dnf,
            SysPm::Apt => &i.apt,
            SysPm::Pacman => &i.pacman,
        }
    }
}

/// What this host can do about one manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// The manager is this host's: temper can read what it has installed.
    pub observe: bool,
    /// It can also install and remove.
    pub converge: bool,
    /// Why a manager that can be read cannot be converged, when that is so.
    pub why_not: Option<&'static str>,
}

/// Answered from what can actually run, not from the distro's name.
///
/// dnf is the case that needs care: an atomic Fedora ships `dnf` and `dnf5` on
/// PATH, and they read the rpm database fine, but the root is read-only and an
/// install fails. "dnf is present" was the wrong question there in the same way
/// "rpm is present" was the wrong question for layering.
pub fn caps(pm: SysPm) -> Caps {
    match pm {
        SysPm::Dnf => {
            let observe = which("rpm").is_some() && which("dnf").is_some();
            let atomic = ostree_booted();
            Caps {
                observe,
                converge: observe && !atomic,
                why_not: (observe && atomic)
                    .then_some("image-based host, dnf cannot install — layer it with `rpm_ostree`"),
            }
        }
        SysPm::Apt => {
            let observe = which("dpkg-query").is_some() && which("apt-mark").is_some();
            Caps {
                observe,
                converge: observe && which("apt-get").is_some(),
                why_not: None,
            }
        }
        SysPm::Pacman => {
            let observe = which("pacman").is_some();
            Caps {
                observe,
                converge: observe,
                why_not: None,
            }
        }
    }
}

/// Whether this host boots an ostree deployment.
///
/// `TEMPER_OSTREE_BOOTED` overrides the marker file, so the test suite gets the
/// same answer on an atomic workstation as on a CI runner.
fn ostree_booted() -> bool {
    match std::env::var("TEMPER_OSTREE_BOOTED").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => Path::new("/run/ostree-booted").exists(),
    }
}

/// Every package this machine declares for `pm` — its bundles' (gated), then its
/// own, first declaration winning. The rule `effective_rpm` uses.
pub fn effective(home: &Path, machine: &Machine, pm: SysPm) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for app in &machine.apps {
        let bundle = manifest::load_bundle(home, app)?;
        if manifest::gated(&bundle.os, &bundle.role, machine) {
            continue;
        }
        for pkg in pm.bundle_list(&bundle) {
            if seen.insert(pkg.clone()) {
                out.push(pkg.clone());
            }
        }
    }
    for pkg in pm.machine_list(machine) {
        if seen.insert(pkg.clone()) {
            out.push(pkg.clone());
        }
    }
    Ok(out)
}

// --- reading -----------------------------------------------------------------

/// A command's non-empty output lines, or `None` when it could not be run or
/// failed — "could not ask", which must never read as "has nothing".
fn lines(cmd: &str, args: &[&str]) -> Option<Vec<String>> {
    let out = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect(),
    )
}

/// Every installed package's name. `None` when the manager is not this host's or
/// could not be read.
pub fn installed(pm: SysPm) -> Option<BTreeSet<String>> {
    if !caps(pm).observe {
        return None;
    }
    match pm {
        // The rpm database, not dnf: no repo metadata, no network, no root.
        SysPm::Dnf => lines("rpm", &["-qa", "--qf", "%{NAME}\n"])
            .map(|l| l.into_iter().filter(|n| n != "gpg-pubkey").collect()),
        // `binary:Package` rather than `Package`: a foreign-architecture package
        // is `libc6:i386`, which is how `apt-mark` names it and how it has to be
        // declared. dpkg also qualifies a native Multi-Arch: same package
        // (`libssl3:amd64`), which apt and a spec both name bare — so the native
        // and `all` qualifiers come off.
        SysPm::Apt => {
            let native = lines("dpkg", &["--print-architecture"])
                .and_then(|l| l.into_iter().next())
                .unwrap_or_default();
            lines(
                "dpkg-query",
                &["-W", "-f", "${binary:Package}\t${db:Status-Abbrev}\n"],
            )
            .map(|l| parse_dpkg_status(&l, &native))
        }
        SysPm::Pacman => lines("pacman", &["-Qq"]).map(|l| l.into_iter().collect()),
    }
}

/// `dpkg-query` lists every package it has a record of, including removed ones
/// that left config behind (`rc`). Installed is the second status letter: `i`, or
/// a trigger still pending on an installed package.
fn parse_dpkg_status(lines: &[String], native: &str) -> BTreeSet<String> {
    lines
        .iter()
        .filter_map(|l| {
            let (name, status) = l.split_once('\t')?;
            let name = match name.rsplit_once(':') {
                Some((bare, arch)) if arch == native || arch == "all" => bare,
                _ => name,
            };
            matches!(status.chars().nth(1), Some('i' | 'W' | 't')).then(|| name.to_string())
        })
        .collect()
}

/// The packages a person asked for by name — not the dependencies they pulled
/// in. `None` when the manager could not be read.
pub fn explicit(pm: SysPm) -> Option<BTreeSet<String>> {
    if !caps(pm).observe {
        return None;
    }
    let got = match pm {
        // `--disablerepo=*`: the history database and rpmdb answer this, so no
        // metadata is fetched and it works without root or a cache. dnf4 appends
        // its own newline after the query format, dnf5 does not; `lines` drops
        // the blank lines dnf4 then prints.
        SysPm::Dnf => lines(
            "dnf",
            &[
                "-q",
                "repoquery",
                "--userinstalled",
                "--disablerepo=*",
                "--qf",
                "%{name}\n",
            ],
        ),
        SysPm::Apt => lines("apt-mark", &["showmanual"]),
        SysPm::Pacman => lines("pacman", &["-Qqe"]),
    }?;
    Some(got.into_iter().collect())
}

/// Declared packages that are not installed. `None` when the manager cannot be
/// read here.
pub fn missing(pm: SysPm, effective: &[String]) -> Option<Vec<String>> {
    if effective.is_empty() {
        return Some(Vec::new());
    }
    let have = installed(pm)?;
    Some(
        effective
            .iter()
            .filter(|p| !have.contains(*p))
            .cloned()
            .collect(),
    )
}

/// What the extras direction found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Extras {
    /// Asked-for packages no scope declares, the ignore lists and the baseline
    /// taken out.
    Found(Vec<String>),
    /// No baseline is recorded yet, so nothing can be told apart from what the
    /// image shipped. The first `install` records one.
    NoBaseline,
    /// The manager could not be read.
    Unavailable,
}

/// Asked-for packages that no scope declares.
///
/// Gated on the machine declaring at least one package for this manager — the
/// probe invariant every provider keeps — and on a baseline existing, for the
/// reason the module header gives.
pub fn extras(machine: &str, pm: SysPm, effective: &[String], ignore: &Ignore) -> Extras {
    if effective.is_empty() {
        return Extras::Found(Vec::new());
    }
    let Some(asked) = explicit(pm) else {
        return Extras::Unavailable;
    };
    let Some(base) = baseline(machine, pm) else {
        return Extras::NoBaseline;
    };
    Extras::Found(extras_from(&asked, &base, effective, pm.ignore_list(ignore)))
}

/// The set arithmetic, apart from the machine, so it is testable.
fn extras_from(
    asked: &BTreeSet<String>,
    base: &BTreeSet<String>,
    effective: &[String],
    ignore: &[String],
) -> Vec<String> {
    asked
        .iter()
        .filter(|p| {
            !base.contains(*p) && !effective.contains(p) && !ignore.contains(p)
        })
        .cloned()
        .collect()
}

/// Entries in THIS machine's own list that the machine does not hold as
/// asked-for: not installed, or installed only as another package's dependency
/// — the undeclare cell, machine scope only. Empty when the manager cannot be
/// read: "declared but absent" and "cannot tell" look the same, and only one
/// may be acted on.
pub fn machine_absent(pm: SysPm, machine_own: &[String]) -> Vec<String> {
    if machine_own.is_empty() {
        return Vec::new();
    }
    let (Some(have), Some(asked)) = (installed(pm), explicit(pm)) else {
        return Vec::new();
    };
    let mut out: Vec<String> = machine_own
        .iter()
        .filter(|p| !have.contains(*p) || !asked.contains(*p))
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

// --- the baseline --------------------------------------------------------------

fn baseline_path(machine: &str) -> PathBuf {
    crate::journal::state_root()
        .join("baseline")
        .join(format!("{machine}.json"))
}

fn load_baselines(machine: &str) -> BTreeMap<String, BTreeSet<String>> {
    std::fs::read_to_string(baseline_path(machine))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// What this manager had already been asked for when temper first managed it on
/// this machine.
pub fn baseline(machine: &str, pm: SysPm) -> Option<BTreeSet<String>> {
    load_baselines(machine).remove(pm.name())
}

/// Record the baseline if there is none. Returns whether one was recorded.
///
/// Taken before the first converge installs anything, so what temper installs is
/// never part of it. Never overwritten: re-taking it would fold every hand
/// install since into "was already here" and hide it for good. Deleting the file
/// is the way to start over, and WORKFLOWS says where it is.
pub fn record_baseline(machine: &str, pm: SysPm) -> Result<bool> {
    let mut all = load_baselines(machine);
    if all.contains_key(pm.name()) {
        return Ok(false);
    }
    let Some(asked) = explicit(pm) else {
        return Ok(false);
    };
    all.insert(pm.name().to_string(), asked);
    let p = baseline_path(machine);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    }
    std::fs::write(&p, serde_json::to_string_pretty(&all)?)
        .with_context(|| format!("writing {}", p.display()))?;
    Ok(true)
}

/// Packages temper marked as asked-for without installing them — declared ones
/// the manager held as dependencies. Letting go of one undoes the mark and
/// nothing more, so they are remembered apart from what temper installed.
fn marked_path(machine: &str) -> PathBuf {
    crate::journal::state_root()
        .join("marked")
        .join(format!("{machine}.json"))
}

fn load_marked(machine: &str) -> BTreeMap<String, BTreeSet<String>> {
    std::fs::read_to_string(marked_path(machine))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_marked(machine: &str, all: &BTreeMap<String, BTreeSet<String>>) -> Result<()> {
    let p = marked_path(machine);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    }
    std::fs::write(&p, serde_json::to_string_pretty(all)?)
        .with_context(|| format!("writing {}", p.display()))
}

/// The packages temper only marked, for this manager on this machine.
pub fn marked(machine: &str, pm: SysPm) -> BTreeSet<String> {
    load_marked(machine).remove(pm.name()).unwrap_or_default()
}

/// Remember packages temper marked as asked-for.
pub fn note_marked(machine: &str, pm: SysPm, pkgs: &[String]) -> Result<()> {
    if pkgs.is_empty() {
        return Ok(());
    }
    let mut all = load_marked(machine);
    all.entry(pm.name().to_string())
        .or_default()
        .extend(pkgs.iter().cloned());
    save_marked(machine, &all)
}

/// Forget packages that are no longer temper's mark — demoted, or gone.
pub fn forget_marked(machine: &str, pm: SysPm, pkgs: &[String]) -> Result<()> {
    if pkgs.is_empty() {
        return Ok(());
    }
    let mut all = load_marked(machine);
    if let Some(set) = all.get_mut(pm.name()) {
        for p in pkgs {
            set.remove(p);
        }
    }
    save_marked(machine, &all)
}

// --- changing ----------------------------------------------------------------

/// Whether this process already runs as root, so `sudo` is neither needed nor,
/// in a minimal container, necessarily installed.
fn is_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(2).map(|e| e == "0"))
        })
        .unwrap_or(false)
}

/// A command that runs as root: through `sudo` unless temper already is root.
///
/// apt's settings travel through `env` rather than as `sudo VAR=value`, which
/// sudoers may refuse; `env` runs as root and sets them on the far side.
fn privileged(pm: SysPm, args: &[&str]) -> Command {
    let mut argv: Vec<&str> = Vec::new();
    if !is_root() {
        argv.push("sudo");
    }
    match pm {
        SysPm::Dnf => argv.push("dnf"),
        SysPm::Apt => argv.extend([
            "env",
            "DEBIAN_FRONTEND=noninteractive",
            "NEEDRESTART_MODE=l",
            "apt-get",
        ]),
        SysPm::Pacman => argv.push("pacman"),
    }
    argv.extend(args);
    let mut c = Command::new(argv[0]);
    c.args(&argv[1..]);
    c
}

/// Have root before a privileged batch starts. The batch runs under a spinner
/// with stdin closed, so a password prompt inside it would land on top of the
/// spinner line — or, with no terminal, nowhere. `install` has usually asked
/// already (its one up-front ask), in which case this is silent.
fn ensure_root(work: &str) {
    if !is_root() {
        crate::sudo::acquire(work);
    }
}

/// apt waits for its lock instead of failing on it. A freshly booted Ubuntu runs
/// its unattended upgrade at exactly the moment a first converge does.
const APT_LOCK_WAIT: &str = "DPkg::Lock::Timeout=300";

/// Install `pkgs` by name. Returns the ones that are installed now — read back,
/// not taken from the exit status — which is what a run journals.
///
/// Only the names: what they pull in belongs to them, and the manager records
/// it as a dependency so its own autoremove can clear it once nothing needs it.
/// Journaling every package that appeared during the run instead would claim
/// whatever else changed the machine meanwhile — an unattended upgrade waiting
/// on the same lock installs a kernel — and `undo` would then remove it.
///
/// A name the manager accepts without installing a package of that name (a
/// pacman group, a dnf provide) exits 0; read back, it is not installed, so it
/// is journaled as nothing.
pub fn install(pm: SysPm, pkgs: &[String], verbose: bool) -> Vec<String> {
    if pkgs.is_empty() {
        return Vec::new();
    }
    let label = format!("{} install", pm.name());
    ensure_root(&format!("{label}: {}", pkgs.join(", ")));
    if pm == SysPm::Apt {
        // Package lists go stale, and a container image ships none at all, so an
        // install against them fails with "Unable to locate package". Refreshing
        // costs a few seconds and only runs when there is something to install.
        // A failed refresh is not fatal: the lists already present may do.
        let refresh = privileged(pm, &["update", "-o", APT_LOCK_WAIT]);
        crate::providers::run_child(refresh, verbose, "apt-get update", "refreshing package lists");
    }
    crate::providers::batch_then_isolate(pkgs, &label, verbose, |items| {
        let mut args: Vec<&str> = match pm {
            SysPm::Dnf => vec!["install", "-y"],
            SysPm::Apt => vec!["install", "-y", "-o", APT_LOCK_WAIT],
            // `--needed` so a package already present is not reinstalled. No
            // `-y`/`-u`: refreshing the sync database without upgrading is the
            // partial upgrade Arch does not support, and upgrading is not
            // temper's to do. A stale database shows up as a package that cannot
            // be found or fetched, and `pacman -Syu` is the remedy.
            SysPm::Pacman => vec!["-S", "--needed", "--noconfirm"],
        };
        args.extend(items.iter().map(String::as_str));
        privileged(pm, &args)
    });
    // Unreadable afterwards: nothing is provably installed, so nothing is
    // journaled — an undo entry for a package that may not be there is worse
    // than none.
    installed(pm)
        .map(|after| pkgs.iter().filter(|p| after.contains(*p)).cloned().collect())
        .unwrap_or_default()
}

// --- why a package is installed --------------------------------------------------

/// Each manager records whether a package was asked for by name or pulled in as
/// a dependency, and its autoremove trusts that record. temper keeps it true to
/// the spec: a declared package is held as asked-for, and one temper no longer
/// wants goes back to being a dependency — never anything about a package the
/// spec does not name.
///
/// apt re-marks a package when it is installed by name; dnf and pacman leave an
/// already-installed dependency marked as one (measured on Debian 13, Ubuntu
/// 22.04, Fedora 44, Alma 8/9, Rocky 10 and Arch), so the mark is set directly.
fn mark_args(pm: SysPm, as_asked: bool) -> Vec<&'static str> {
    match (pm, as_asked) {
        (SysPm::Apt, true) => vec!["apt-mark", "manual"],
        (SysPm::Apt, false) => vec!["apt-mark", "auto"],
        (SysPm::Dnf, true) if dnf5() => vec!["dnf", "-y", "mark", "user"],
        (SysPm::Dnf, false) if dnf5() => vec!["dnf", "-y", "mark", "dependency"],
        (SysPm::Dnf, true) => vec!["dnf", "-y", "mark", "install"],
        (SysPm::Dnf, false) => vec!["dnf", "-y", "mark", "remove"],
        (SysPm::Pacman, true) => vec!["pacman", "-D", "--asexplicit"],
        (SysPm::Pacman, false) => vec!["pacman", "-D", "--asdeps"],
    }
}

/// dnf5 renamed `mark install`/`mark remove` to `mark user`/`mark dependency`.
fn dnf5() -> bool {
    lines("dnf", &["--version"])
        .and_then(|l| l.into_iter().next())
        .is_some_and(|v| v.contains("dnf5"))
}

/// Mark `pkgs` as asked-for (`true`) or as dependencies (`false`). No package is
/// installed or removed. Returns the ones that carry the mark afterwards, read
/// back.
pub fn mark(pm: SysPm, pkgs: &[String], as_asked: bool) -> Vec<String> {
    if pkgs.is_empty() {
        return Vec::new();
    }
    let label = format!(
        "{} mark {}",
        pm.name(),
        if as_asked { "as yours" } else { "as a dependency" }
    );
    ensure_root(&format!("{label}: {}", pkgs.join(", ")));
    let base = mark_args(pm, as_asked);
    crate::providers::batch_then_isolate(pkgs, &label, false, |items| {
        let mut argv: Vec<&str> = Vec::new();
        if !is_root() {
            argv.push("sudo");
        }
        argv.extend(base.iter().copied());
        argv.extend(items.iter().map(String::as_str));
        let mut c = Command::new(argv[0]);
        c.args(&argv[1..]);
        c
    });
    match explicit(pm) {
        Some(asked) => pkgs
            .iter()
            .filter(|p| asked.contains(*p) == as_asked)
            .cloned()
            .collect(),
        None => Vec::new(),
    }
}

/// Declared packages that are installed, but held by the manager as somebody
/// else's dependency — so its autoremove takes them when whatever pulled them in
/// goes, declaration or not. `None` when the manager cannot be read.
pub fn held_as_dependency(pm: SysPm, effective: &[String]) -> Option<Vec<String>> {
    if effective.is_empty() {
        return Some(Vec::new());
    }
    let have = installed(pm)?;
    let asked = explicit(pm)?;
    Some(
        effective
            .iter()
            .filter(|p| have.contains(*p) && !asked.contains(*p))
            .cloned()
            .collect(),
    )
}

/// What a removal would do, as the manager itself simulates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sim {
    /// Removes exactly what was asked.
    Clean,
    /// Would take other installed packages along — something that depends on
    /// one of them, or, for a name that expands, the packages it expands to.
    Cascades,
    /// The simulation itself failed — an unknown package, a broken database.
    Failed,
}

/// Simulate removing `pkgs`. Read-only, and needs no root on any of the three.
///
/// The removal itself takes exactly the named packages (see `remove`), so a
/// simulation that names anything else is a removal temper will not run.
fn simulate(pm: SysPm, pkgs: &[String]) -> Sim {
    // The C locale, because the refusals below are recognised by their text.
    let run = |cmd: &str, args: Vec<&str>| {
        Command::new(cmd)
            .args(args)
            .args(pkgs)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .output()
            .ok()
    };
    match pm {
        // rpm checks the set's reverse dependencies and refuses with "is needed
        // by (installed) …"; anything else is a different failure.
        SysPm::Dnf => match run("rpm", vec!["-e", "--test"]) {
            Some(o) if o.status.success() => Sim::Clean,
            Some(o) if String::from_utf8_lossy(&o.stderr).contains("is needed by") => {
                Sim::Cascades
            }
            _ => Sim::Failed,
        },
        // apt simulates the whole transaction; a `Remv` line for a package that
        // was not asked for is a dependant going with it.
        SysPm::Apt => match run("apt-get", vec!["-s", "remove"]) {
            Some(o) if o.status.success() => {
                let removed = remv_lines(&String::from_utf8_lossy(&o.stdout));
                if removed.iter().all(|r| pkgs.contains(r)) {
                    Sim::Clean
                } else {
                    Sim::Cascades
                }
            }
            _ => Sim::Failed,
        },
        // pacman refuses to break a dependency without `-c`, and says so. It
        // also expands a group name to its members, which `--print` lists — so
        // the targets are compared with what was asked, as apt's are.
        SysPm::Pacman => match run("pacman", vec!["-Rp", "--print-format", "%n"]) {
            Some(o) if o.status.success() => {
                let targets = String::from_utf8_lossy(&o.stdout);
                if targets.lines().map(str::trim).filter(|l| !l.is_empty()).all(|t| pkgs.iter().any(|p| p == t)) {
                    Sim::Clean
                } else {
                    Sim::Cascades
                }
            }
            Some(o) if String::from_utf8_lossy(&o.stderr).contains("could not satisfy dependencies") => {
                Sim::Cascades
            }
            _ => Sim::Failed,
        },
    }
}

/// Package names from `apt-get -s` output: `Remv jq [1.7.1-3]`.
fn remv_lines(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|l| l.strip_prefix("Remv "))
        .filter_map(|r| r.split_whitespace().next())
        .map(String::from)
        .collect()
}

/// Why a package was held back from a removal.
pub const HELD_UNSIMULATED: &str = "the package manager could not simulate removing it";

/// What letting go of named packages does — `prune` for extras, `undo` for what
/// a run installed or marked.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Release {
    /// Removed by name: nothing else goes with them.
    pub remove: Vec<String>,
    /// Kept, and marked as dependencies: something installed needs them, or
    /// temper only ever marked them. The manager's autoremove clears them once
    /// nothing needs them.
    pub demote: Vec<String>,
    /// Left alone, with the reason.
    pub held: Vec<(String, &'static str)>,
    /// Already gone — nothing to do.
    pub gone: Vec<String>,
}

/// Plan letting go of `pkgs`. `only_marked` are packages temper never installed,
/// only marked as asked-for; letting go of one undoes the mark and nothing more.
/// `None` when the manager cannot be read — "cannot tell", never "all gone".
///
/// The whole set is simulated first, so a package needed only by another one in
/// the same set is removed with it. Where the set would take something else
/// along, each is tried alone, which names the ones another package needs; those
/// are demoted rather than removed, and the rest are tried together again,
/// because two packages fine alone can still be needed by a third.
pub fn release_plan(
    pm: SysPm,
    pkgs: &[String],
    only_marked: &BTreeSet<String>,
) -> Option<Release> {
    let have = installed(pm)?;
    let mut plan = Release::default();
    let mut candidates = Vec::new();
    for p in pkgs {
        if !have.contains(p) {
            plan.gone.push(p.clone());
        } else if only_marked.contains(p) {
            plan.demote.push(p.clone());
        } else {
            candidates.push(p.clone());
        }
    }
    if candidates.is_empty() {
        return Some(plan);
    }
    if simulate(pm, &candidates) == Sim::Clean {
        plan.remove = candidates;
        return Some(plan);
    }
    for p in candidates {
        match simulate(pm, std::slice::from_ref(&p)) {
            Sim::Clean => plan.remove.push(p),
            Sim::Cascades => plan.demote.push(p),
            Sim::Failed => plan.held.push((p, HELD_UNSIMULATED)),
        }
    }
    if !plan.remove.is_empty() && simulate(pm, &plan.remove) != Sim::Clean {
        plan.demote.append(&mut plan.remove);
    }
    Some(plan)
}

/// Carry out a `release_plan`. Returns `(removed, demoted)`, each read back.
pub fn release(pm: SysPm, plan: &Release, verbose: bool) -> (Vec<String>, Vec<String>) {
    let removed = remove(pm, &plan.remove, verbose);
    let demoted = mark(pm, &plan.demote, false);
    if !removed.is_empty() {
        eprintln!(
            "  {} anything {} pulled in stays, marked as dependencies — `{}` removes it once nothing needs it",
            crate::ui::dim(crate::ui::g_arrow()),
            removed.join(", "),
            autoremove_hint(pm)
        );
    }
    (removed, demoted)
}

/// The manager's own clean-up, named for the user to run. temper never runs it:
/// it is whole-system — apt's took an unrelated orphan in the same breath — and
/// that is the manager's call, not a spec's.
pub fn autoremove_hint(pm: SysPm) -> &'static str {
    match pm {
        SysPm::Dnf => "sudo dnf autoremove",
        SysPm::Apt => "sudo apt autoremove",
        SysPm::Pacman => "sudo pacman -Rns $(pacman -Qdtq)",
    }
}

/// Remove `pkgs` by name. Returns what is actually gone afterwards — read back,
/// not assumed.
///
/// Exactly the named packages: dnf's `clean_requirements_on_remove` is turned
/// off and pacman runs without `-s`, as apt-get does by default. What the named
/// packages pulled in stays, still marked as dependencies, for the manager's
/// own autoremove — which sees it on all three.
fn remove(pm: SysPm, pkgs: &[String], verbose: bool) -> Vec<String> {
    if pkgs.is_empty() {
        return Vec::new();
    }
    let label = format!("{} remove", pm.name());
    ensure_root(&format!("{label}: {}", pkgs.join(", ")));
    let reported = crate::providers::batch_then_isolate(pkgs, &label, verbose, |items| {
        let mut args: Vec<&str> = match pm {
            SysPm::Dnf => vec!["remove", "-y", "--setopt=clean_requirements_on_remove=False"],
            SysPm::Apt => vec!["remove", "-y", "-o", APT_LOCK_WAIT],
            SysPm::Pacman => vec!["-R", "--noconfirm"],
        };
        args.extend(items.iter().map(String::as_str));
        privileged(pm, &args)
    });
    match installed(pm) {
        Some(have) => pkgs.iter().filter(|p| !have.contains(*p)).cloned().collect(),
        None => reported,
    }
}

/// Undo an install of `pkgs` (`marked_on` is `None`), or the marks a run set on
/// `marked_on`'s packages, which are only ever un-marked. Returns how many are
/// reverted: gone, or held as a dependency again.
pub fn revert(pm: SysPm, pkgs: &[String], marked_on: Option<&str>) -> usize {
    let only: BTreeSet<String> = if marked_on.is_some() {
        pkgs.iter().cloned().collect()
    } else {
        BTreeSet::new()
    };
    let Some(plan) = release_plan(pm, pkgs, &only) else {
        eprintln!(
            "{} {} could not list what is installed — nothing was reverted",
            crate::ui::yellow(crate::ui::g_warn()),
            pm.name()
        );
        return 0;
    };
    for (p, why) in &plan.held {
        eprintln!(
            "{} kept {p} ({}) — {why}",
            crate::ui::yellow(crate::ui::g_warn()),
            pm.name()
        );
    }
    let (removed, demoted) = release(pm, &plan, false);
    if let Some(machine) = marked_on {
        let _ = forget_marked(machine, pm, &demoted);
    }
    plan.gone.len() + removed.len() + demoted.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(xs: &[&str]) -> BTreeSet<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    /// Every manager has both kinds, and no two managers share one.
    #[test]
    fn every_manager_has_its_own_two_kinds() {
        let mut seen = BTreeSet::new();
        for &pm in SysPm::ALL {
            for extra in [false, true] {
                assert!(
                    seen.insert(pm.kind(extra)),
                    "`{}` is claimed twice",
                    pm.kind(extra)
                );
            }
            assert_eq!(SysPm::from_name(pm.name()), Some(pm));
        }
    }

    /// The base image is never an extra; only what was asked for since is.
    #[test]
    fn extras_are_counted_from_the_baseline() {
        // Shaped like a stock Ubuntu: the base counts as asked-for.
        let base = set(&["apt", "bash", "dpkg", "openssh-server"]);
        let asked = set(&["apt", "bash", "dpkg", "openssh-server", "jq", "cowsay", "htop"]);
        let declared = vec!["jq".to_string()];
        let ignore = vec!["htop".to_string()];
        assert_eq!(
            extras_from(&asked, &base, &declared, &ignore),
            vec!["cowsay".to_string()]
        );
        // An undeclared package from the baseline stays out — it was there
        // before temper managed this machine, and prune must never reach it.
        assert!(extras_from(&base, &base, &[], &[]).is_empty());
    }

    /// Removed packages that kept their config (`rc`) are not installed; a
    /// trigger still pending on an installed package is.
    #[test]
    fn dpkg_status_reads_the_installed_letter() {
        let rows: Vec<String> = [
            "jq\tii ",
            "libc6:i386\tii ",
            "libssl3:amd64\tii ",
            "tzdata:all\tii ",
            "old\trc ",
            "held\thi ",
            "pending\tiW ",
            "half\tiF ",
            "gone\tun ",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            parse_dpkg_status(&rows, "amd64"),
            set(&["jq", "libc6:i386", "libssl3", "tzdata", "held", "pending"])
        );
    }

    /// The apt simulation is read off its `Remv` lines — captured from Debian 13
    /// removing `libonig5`, which takes `jq` and `libjq1` with it.
    #[test]
    fn apt_simulation_names_what_would_go() {
        let out = "NOTE: This is only a simulation!\n\
                   Reading package lists...\n\
                   The following packages will be REMOVED:\n  jq libjq1 libonig5\n\
                   Remv jq [1.7.1+dfsg-2]\n\
                   Remv libjq1 [1.7.1+dfsg-2]\n\
                   Remv libonig5 [6.9.9-1+b1]\n";
        assert_eq!(remv_lines(out), vec!["jq", "libjq1", "libonig5"]);
    }
}
