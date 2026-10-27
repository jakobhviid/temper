//! Package managers as (probe, converge) shell-outs. The pure set logic lives
//! in `packages`; this layer talks to the real tools.
//!
//! Guarded throughout: a manager is only probed/converged if its CLI is present
//! AND the effective set actually contains one of its packages — so on a
//! machine without brew, or with no declared packages, this is a clean no-op.
//! The real converge (`brew bundle`, `flatpak install`) is VM-verified; it is
//! never exercised by the sandboxed tests (which declare no packages).
//!
//! `gext` (GNOME extensions) and `rpm-ostree` (layered rpms) are modeled at the
//! bottom — they don't use Brewfile grammar, so they have their own bundle
//! fields (`extensions`, `rpm`). Both are Linux/VM-only and guarded on their CLI.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::manifest::{self, Machine};
use crate::packages::{Installed, Manager, Pkg};
use crate::primitives::which;

fn have(cmd: &str) -> bool {
    which(cmd).is_some()
}

/// A command's non-empty output lines (trimmed), or `None` when it **failed**.
///
/// The distinction is the whole of Principle #12: "the tool answered and the
/// answer is none" and "I could not ask" are different facts, and every write
/// path reads the second as the first if you let it. A tap that will not tap, a
/// Mac not signed into the App Store, `code` over ssh — each exits non-zero with
/// an empty stdout, which as a bare `Vec` is indistinguishable from a machine
/// with nothing installed.
fn run_lines_opt(cmd: &str, args: &[&str]) -> Result<Option<Vec<String>>> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("running {cmd} {args:?}"))?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
    ))
}

/// `run_lines_opt` for the callers where a failure genuinely is "nothing to
/// report": display names, and the version lists used to *measure* an upgrade.
/// Never use this to decide what is installed.
fn run_lines(cmd: &str, args: &[&str]) -> Result<Vec<String>> {
    Ok(run_lines_opt(cmd, args)?.unwrap_or_default())
}

/// Run **one** command for a whole set of one type, falling back to per-item
/// only if that fails.
///
/// Batching is the fast path: one process instead of N, and — for anything
/// needing root — one password prompt instead of one per item, which is the
/// difference between a converge you can walk away from and one you have to
/// babysit. Every provider's CLI takes a list (`gext install UUID [UUID…]`,
/// `mas install <id>…`, `rpm-ostree install <pkg>…`), so the loops were costing
/// that for nothing.
///
/// What the loops *were* buying is the reason for the fallback: a batch that
/// fails tells you nothing about which item failed, and one bad entry must not
/// strand the rest (Principle #6 — a forgiving provider reports loudly and
/// continues). So on failure each item is retried alone, which both isolates the
/// damage and names it.
///
/// Returns the items that actually landed — which is what gets journaled, so a
/// failed install never leaves an undo entry for something that was never there.
pub(crate) fn batch_then_isolate<F>(items: &[String], label: &str, verbose: bool, build: F) -> Vec<String>
where
    F: Fn(&[String]) -> Command,
{
    if items.is_empty() {
        return Vec::new();
    }
    // Under a spinner, like every other converge child. `.output()` blocks until
    // the child exits and draws nothing while it does, so the longest operation
    // temper performs — `rpm-ostree install`, which downloads and then composes
    // an entire deployment — printed *nothing at all* for minutes. Not a frozen
    // spinner: no line. A run indistinguishable from a hang is a run people kill
    // halfway, and `mas install`, `gext install` and `gext uninstall` all came
    // through here the same way.
    //
    // No failure replay, unlike `run_child`. A failed **batch** is a probe, not
    // a verdict: the isolating pass below is what decides, so replaying the
    // batch's log would report a failure that is about to be recovered — and
    // where it is not, the per-item line names the item, which is the useful
    // half.
    //
    // The batch carries the phase alone and lets the child's own output name the
    // item it is on; the isolating pass names the item itself, because there the
    // point IS which one is being retried.
    let ok = |mut c: Command, doing: &str| -> bool {
        if verbose {
            c.stdin(Stdio::null());
            if crate::ui::json_mode() {
                c.stdout(stderr_stdio());
            }
            return c.status().map(|s| s.success()).unwrap_or(false);
        }
        run_with_spinner(c, label, doing)
            .map(|(ok, _)| ok)
            .unwrap_or(false)
    };
    if ok(build(items), label) {
        return items.to_vec();
    }
    // The batch failed: find out which ones, rather than reporting the whole set
    // as lost or the whole set as fine.
    let mut landed = Vec::new();
    for item in items {
        if ok(
            build(std::slice::from_ref(item)),
            &format!("{label}: {item}"),
        ) {
            landed.push(item.clone());
        } else {
            eprintln!(
                "{} {label} failed for {item} — skipped",
                crate::ui::yellow(crate::ui::g_warn())
            );
        }
    }
    landed
}

/// Snapshot what's installed, but only for managers that (a) appear in the
/// effective set and (b) have their CLI present. Absent managers stay unprobed
/// (so nothing is reported as missing/extra for them).
/// Put a manager's raw listing into the shape `Pkg::match_name` produces, so a
/// declared token and an installed one are comparable.
fn normalize(m: Manager, lines: Vec<String>) -> Vec<String> {
    match m {
        // `mas list` rows look like: "497799835  Xcode (14.0)" → first token.
        Manager::Mas => lines
            .into_iter()
            .filter_map(|l| l.split_whitespace().next().map(String::from))
            .collect(),
        Manager::Vscode => lines.into_iter().map(|s| s.to_lowercase()).collect(),
        _ => lines,
    }
}

pub fn probe(effective: &[Pkg]) -> Result<Installed> {
    probe_scoped(effective, false)
}

/// `probe` for the SEED case: enumerate every manager whose tool is here, not
/// just the ones already declared.
///
/// **vscode is deliberately excluded even when seeding.** The probe invariant is
/// what keeps a VS Code Settings Sync setup the sole registrar of its extensions
/// (SPEC says so), and `init` adopting them wholesale is exactly the ownership
/// temper does not want. Everything else is fair game — discovering it is what
/// `init` is for.
pub fn probe_seeding() -> Result<Installed> {
    probe_scoped(&[], true)
}

/// How each manager is enumerated: the tool, and the argv that lists what it has.
///
/// Exhaustive, so a manager added later cannot be silently unprobed — and an
/// unprobed manager is worse than it sounds: `missing()` only considers managers
/// that answered, so its declared packages are never reported missing and never
/// installed. Silent, green, and nothing on the machine.
fn probe_argv(m: Manager) -> (&'static str, &'static [&'static str]) {
    match m {
        Manager::Brew => ("brew", &["list", "--formula"]),
        Manager::Cask => ("brew", &["list", "--cask"]),
        Manager::Tap => ("brew", &["tap"]),
        // BOTH scopes count as installed, so an app is not reported missing
        // because it sits in the installation temper does not converge into.
        // Which scope it is in only matters for *removal* — see
        // `flatpak_system_apps`.
        Manager::Flatpak => ("flatpak", &["list", "--app", "--columns=application"]),
        Manager::Mas => ("mas", &["list"]),
        Manager::Vscode => ("code", &["--list-extensions"]),
    }
}

fn probe_scoped(effective: &[Pkg], seed: bool) -> Result<Installed> {
    let mut managers: HashSet<Manager> = effective.iter().map(|p| p.manager).collect();
    if seed {
        // Everything except vscode — Settings Sync stays the sole registrar of
        // those extensions, and `init` adopting them wholesale is an ownership
        // temper declines (see `probe_seeding`).
        managers.extend(
            Manager::ALL
                .iter()
                .copied()
                .filter(|m| *m != Manager::Vscode),
        );
    }
    let mut inst = Installed::default();

    for &m in Manager::ALL {
        if !managers.contains(&m) {
            continue;
        }
        let (cmd, args) = probe_argv(m);
        if !have(cmd) {
            continue; // the tool is absent — never asked, so nothing to report
        }
        // `have()` answers "is the tool here", never "did it work". A tool that
        // is present and fails is the dangerous case: its empty stdout used to
        // land as an empty installed-set, which `probed()` reports as a real
        // answer, and every drop path then reads as "the machine has none of
        // these, delete them from the spec".
        match run_lines_opt(cmd, args)? {
            Some(lines) => inst.set(m, normalize(m, lines)),
            None => inst.unavailable(m),
        }
    }
    Ok(inst)
}

/// Parse a `mas list` row (`"497799835  Xcode (14.0)"`) into `(id, app-name)`,
/// stripping the trailing ` (version)`. Pure — the testable heart of name lookup.
fn parse_mas_line(line: &str) -> Option<(String, String)> {
    let (id, rest) = line.trim().split_once(char::is_whitespace)?;
    let name = rest.trim();
    // Drop a trailing " (version)" if present.
    let name = match name.rfind(" (") {
        Some(i) if name.ends_with(')') => name[..i].trim(),
        _ => name,
    };
    if id.is_empty() || name.is_empty() {
        return None;
    }
    Some((id.to_string(), name.to_string()))
}

/// Map of installed Mac App Store id → human app name (from `mas list`). Empty
/// where `mas` is absent. Used by `reconcile` so a mas extra shows/writes its
/// name (`mas "Xcode", id: 497799835`), not a bare numeric id.
pub fn mas_names() -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    if !have("mas") {
        return out;
    }
    for line in run_lines("mas", &["list"]).unwrap_or_default() {
        if let Some((id, name)) = parse_mas_line(&line) {
            out.insert(id, name);
        }
    }
    out
}

/// Resolve a brew short name to its `(manager, fully-qualified token name)` via
/// Resolve short brew names to their fully-qualified identity in **one**
/// `brew info --json=v2 <name…>` call — a tap formula/cask becomes its
/// `user/tap/name`. Crucial for `reconcile`: a bare short token (e.g.
/// `brew "sesh"`) may not match the installed tap formula in `brew bundle
/// cleanup`, so it stays "undeclared" and is re-offered forever.
///
/// Batched on purpose: `brew info` spins up Ruby and evaluates each formula, so
/// one call per extra made `reconcile` hang for tens of seconds on a machine
/// with a couple dozen extras — a single call for all of them is ~one call's
/// cost. Names brew can't resolve are simply absent from the map (caller falls
/// back to the short name). Casks take precedence over a same-named formula.
pub fn brew_identities(names: &[&str]) -> std::collections::BTreeMap<String, (Manager, String)> {
    let mut map = std::collections::BTreeMap::new();
    if names.is_empty() || !have("brew") {
        return map;
    }
    let out = Command::new("brew")
        .args(["info", "--json=v2"])
        .args(names)
        .output();
    let Ok(out) = out else { return map };
    if !out.status.success() {
        return map;
    }
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return map;
    };
    // Casks first, then formulae — `or_insert` keeps the cask on a name clash,
    // matching the old single-name precedence.
    if let Some(arr) = v.get("casks").and_then(|c| c.as_array()) {
        for c in arr {
            if let (Some(tok), Some(full)) = (
                c.get("token").and_then(|t| t.as_str()),
                c.get("full_token").and_then(|t| t.as_str()),
            ) {
                map.entry(tok.to_string())
                    .or_insert((Manager::Cask, full.to_string()));
            }
        }
    }
    if let Some(arr) = v.get("formulae").and_then(|f| f.as_array()) {
        for f in arr {
            if let (Some(name), Some(full)) = (
                f.get("name").and_then(|t| t.as_str()),
                f.get("full_name").and_then(|t| t.as_str()),
            ) {
                map.entry(name.to_string())
                    .or_insert((Manager::Brew, full.to_string()));
            }
        }
    }
    map
}

/// The label a progress spinner should show for a line of Homebrew output, or
/// `None` for a line that names no single package. Pure → unit-tested, because
/// this is the one place temper reads Homebrew's human output as a data feed and
/// a wrong guess shows the user the wrong package name.
///
/// `brew bundle` itself prints nothing per entry unless `--verbose` (see
/// `bundle/brew.rb`), so the signal we key on is the *nested* `brew install`'s
/// own `ohai` lines — those are printed unconditionally. Lines naming a *list*
/// ("Installing dependencies for x: a, b", "Fetching downloads for: a, b") are
/// deliberately `None`: each item in them arrives again on its own line, and
/// showing the list would just flash unreadably.
fn brew_progress_label(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix("==> ")?;
    // "Installing llvm dependency: xz" — the dep is what's building right now.
    if let Some((_, dep)) = rest.split_once(" dependency: ") {
        return Some(dep.trim().to_string());
    }
    if rest.starts_with("Installing dependencies for ") || rest.starts_with("Fetching downloads for")
    {
        return None; // a list — its members each get their own line
    }
    // `Upgrading` is the *upgrade* vocabulary and `update` runs `brew upgrade`, so
    // leaving it out made the busiest verb the quietest one. `Downloading` is
    // deliberately absent: its argument is a URL, and the package name from the
    // preceding `Upgrading` line is the better thing to leave on screen while the
    // download runs.
    for prefix in [
        "Installing Cask ",
        "Installing ",
        "Upgrading Cask ",
        "Upgrading ",
        "Fetching ",
    ] {
        if let Some(x) = rest.strip_prefix(prefix) {
            return Some(x.trim().to_string());
        }
    }
    // "Pouring llvm--22.1.8.arm64_tahoe.bottle.tar.gz" → the formula name.
    if let Some(x) = rest.strip_prefix("Pouring ") {
        let name = x.split_once("--").map_or(x, |(n, _)| n);
        return Some(name.trim().to_string());
    }
    // "Running installer for mactex; your password may be necessary."
    if let Some(x) = rest.strip_prefix("Running installer for ") {
        return Some(x.split(';').next().unwrap_or(x).trim().to_string());
    }
    None
}

/// How wide a progress label may get. The region is one line that rewrites itself
/// in place, so a label that outgrows the terminal wraps and the line stops being
/// one line.
const LABEL_MAX: usize = 48;

/// What a child is working on right now, for the region's message.
///
/// Homebrew's `==>` headers are parsed for a clean name where they match. Every
/// other child — `flatpak update`, `rpm-ostree uninstall` — had no parser at all
/// and so sat on its opening message for the whole run, which is indistinguishable
/// from being wedged. So anything else falls back to its own last line, elided:
/// less pretty than a parsed name, and far better than nothing, because the point
/// is to be able to say where a run stopped.
///
/// Blank and decorative lines are skipped so the label does not flicker to empty,
/// and a warning is left to `noteworthy_lines`, which prints it properly rather
/// than flashing it past in a region that is about to be overwritten.
fn progress_label(line: &str) -> Option<String> {
    if let Some(name) = brew_progress_label(line) {
        return Some(crate::ui::elide(&name, LABEL_MAX));
    }
    let t = line.trim();
    if t.is_empty() || t.chars().all(|c| !c.is_alphanumeric()) || is_noteworthy(t) {
        return None;
    }
    Some(crate::ui::elide(t.trim_start_matches("==> ").trim(), LABEL_MAX))
}

/// Whether a captured line *starts* something worth surfacing even on a
/// successful run — the capture-and-replay-on-failure pattern would otherwise
/// swallow Homebrew's advisories, which the streamed (`--verbose`) path showed.
/// Case-insensitive on the prefix: Homebrew capitalizes (`Warning:`), flatpak
/// does not (`error: Failed to install …`), and an advisory must survive the
/// quiet path whichever tool wrote it.
fn is_noteworthy(line: &str) -> bool {
    let l = line.trim_start();
    let n = l.len().min(8);
    let head = l[..n].to_ascii_lowercase();
    head.starts_with("warning:") || head.starts_with("error:")
}

/// The lines to surface from a *successful* run: every `Warning:`/`Error:` line
/// **plus its body**. Homebrew's advisories are multi-line blocks whose body
/// carries the remedy —
///
/// ```text
/// Warning: Formulae dependency graph sorting found a circular dependency:
///   libtiff, webp
/// This is usually caused by stale dependency data in installed keg tabs.
/// If it persists, run the following commands and try again:
///   brew update
/// ```
///
/// — so first-line-only would print a problem with its fix cut off. A block runs
/// until a blank line or the next `==>` progress line (which is Homebrew moving
/// on to the next package, not part of the advisory).
fn noteworthy_lines(log: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut in_block = false;
    for line in log.lines() {
        if is_noteworthy(line) {
            in_block = true;
        } else if line.trim().is_empty() || line.trim_start().starts_with("==> ") {
            in_block = false;
        }
        if in_block {
            out.push(line);
        }
    }
    out
}

/// Run a `brew bundle` (or any converge child) with its output captured, driving
/// a spinner off the package names it announces. Returns the merged log on
/// failure so the caller can replay everything it swallowed.
///
/// Quiet by default is the repo-wide convention (exec steps, `brew upgrade`), but
/// a silent 40-minute converge reads as a hang — so the spinner shows *which*
/// package is being worked on right now, and warnings still print.
///
/// `stdin` is explicitly **null**. Inheriting it would let a child that decides
/// to prompt (a missing `-y`, a polkit fallback) block on the tty while its
/// question disappears into the captured pipe — an invisible hang. Closed stdin
/// turns that into a fast failure whose log we replay, which is debuggable.
fn run_with_spinner(mut cmd: Command, what: &str, initial: &str) -> Result<(bool, String)> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().with_context(|| format!("spawning {what}"))?;
    let stdout = child.stdout.take().context("piped stdout")?;
    let stderr = child.stderr.take().context("piped stderr")?;

    let pb = crate::ui::spinner(initial);
    // A child that goes quiet gets the terminal back: `sudo` and polkit prompt on
    // `/dev/tty`, which none of the pipes above capture, so a question can land
    // under the region and be erased by the next redraw. See `ui::StallWatch`.
    let stall = crate::ui::StallWatch::new(&pb);

    // stderr is drained on its own thread: a full pipe buffer on either stream
    // would deadlock the child while we block reading the other. Read by line
    // rather than to EOF so it can report the child is alive as it goes — plenty
    // of tools put their progress here, and a child watched on stdout alone reads
    // as silent for as long as it happens to be talking on the other stream.
    let beat = stall.beat();
    let errs = std::thread::spawn(move || {
        let mut buf = String::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            beat.activity();
            buf.push_str(&line);
            buf.push('\n');
        }
        buf
    });

    let mut log = String::new();
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        stall.activity();
        if let Some(label) = progress_label(&line) {
            // The phase is kept and the item appended, rather than replaced. Which
            // phase you are in is the half that says whether a pause is expected,
            // and it is also what the line still reads as once the child goes quiet
            // and the region freezes — the last thing that happened before the
            // silence is exactly what a stalled run needs to be able to say.
            pb.set_message(format!("{initial}: {label}"));
        }
        log.push_str(&line);
        log.push('\n');
    }
    let ok = child
        .wait()
        .with_context(|| format!("waiting for {what}"))?
        .success();
    let log = format!("{log}{}", errs.join().unwrap_or_default());
    drop(stall); // stop the watcher before the region goes away
    pb.finish_and_clear();

    if ok {
        for line in noteworthy_lines(&log) {
            eprintln!("{line}"); // stderr: keeps `--json` clean
        }
    }
    Ok((ok, log))
}

/// Run a **best-effort** converge child: `--verbose` streams it live, otherwise
/// it runs under [`run_with_spinner`] — silent on success, and on failure the
/// swallowed log is replayed with a warning instead of aborting the run.
///
/// This is the one door every such child goes through, because a child's own
/// output is about the child's domain, not about temper's run. Left inherited,
/// `flatpak update`'s "Nothing to update." (its remotes) reads as a verdict on
/// the whole converge, and — writing to temper's stdout — corrupts `--json`.
/// Returns whether the child succeeded.
pub(crate) fn run_child(mut cmd: Command, verbose: bool, what: &str, initial: &str) -> bool {
    if verbose {
        // The user asked to see it — stream live. But `--verbose` and `--json`
        // are both global, so under JSON a streamed child would write its output
        // into the document (Principle #6b). Send it to stderr instead: still
        // live, still complete, and stdout stays the machine-readable channel.
        if crate::ui::json_mode() {
            cmd.stdout(stderr_stdio());
        }
        return cmd.status().map(|s| s.success()).unwrap_or(false);
    }
    match run_with_spinner(cmd, what, initial) {
        Ok((true, _)) => true,
        Ok((false, log)) => {
            eprintln!("{} {what} failed:", crate::ui::yellow(crate::ui::g_warn()));
            if !log.is_empty() {
                eprint!("{log}"); // replay what the capture swallowed
            }
            false
        }
        Err(e) => {
            eprintln!("{} could not run {what}: {e:#}", crate::ui::yellow(crate::ui::g_warn()));
            false
        }
    }
}

/// A `Stdio` pointing at this process's stderr, so a streamed child can be seen
/// without being mistaken for temper's stdout.
fn stderr_stdio() -> Stdio {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        if let Ok(fd) = std::io::stderr().as_fd().try_clone_to_owned() {
            return Stdio::from(fd);
        }
    }
    Stdio::null()
}

/// The casks in `effective` that Homebrew will need **root** for — those with a
/// `pkg` or `installer` artifact (`mactex`, `zoom`, `dotnet-sdk`, …) — and that
/// this run would actually touch (not installed, or installed but outdated).
/// Empty on a converged machine, without brew, or when no declared cask needs
/// root, so the caller never asks for a password speculatively.
///
/// Batched into one `brew info --json=v2 --cask` over just the candidates: on an
/// already-converged machine the candidate list is empty and this costs nothing
/// beyond two cheap list calls.
pub fn casks_needing_root(effective: &[Pkg]) -> Vec<String> {
    if !have("brew") {
        return Vec::new();
    }
    let declared: Vec<&str> = effective
        .iter()
        .filter(|p| p.manager == Manager::Cask)
        .map(|p| p.name.as_str())
        .collect();
    if declared.is_empty() {
        return Vec::new();
    }
    let installed: HashSet<String> = run_lines("brew", &["list", "--cask"])
        .unwrap_or_default()
        .into_iter()
        .collect();
    let outdated: HashSet<String> = run_lines("brew", &["outdated", "--cask", "--quiet"])
        .unwrap_or_default()
        .into_iter()
        .collect();
    // A tap-qualified token (`user/tap/x`) is listed by brew under its short name.
    let short = |n: &str| n.rsplit('/').next().unwrap_or(n).to_string();
    let candidates: Vec<String> = declared
        .iter()
        .filter(|n| {
            let s = short(n);
            !installed.contains(&s) || outdated.contains(&s)
        })
        .map(|n| n.to_string())
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }

    let out = Command::new("brew")
        .args(["info", "--json=v2", "--cask"])
        .args(&candidates)
        .output();
    let Ok(out) = out else { return Vec::new() };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return Vec::new();
    };
    let mut need = Vec::new();
    for c in v.get("casks").and_then(|c| c.as_array()).into_iter().flatten() {
        let roots = c
            .get("artifacts")
            .and_then(|a| a.as_array())
            .is_some_and(|arts| {
                arts.iter().any(|a| {
                    a.get("pkg").is_some() || a.get("installer").is_some()
                })
            });
        if roots {
            if let Some(t) = c.get("token").and_then(|t| t.as_str()) {
                need.push(t.to_string());
            }
        }
    }
    need.sort();
    need
}

/// Converge the effective set (install-missing; never removes). brew-family
/// packages go through one materialized Brewfile + `brew bundle`; flatpaks are
/// installed by id. `dry_run` performs no mutation. Returns the number of
/// declared packages considered.
/// Which converge path a manager takes. Exhaustive, so a manager added later
/// cannot fall through every branch and simply never be installed — the mirror
/// of the probe gap, and just as quiet: the package stays declared, stays
/// missing, and every converge reports success.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum ConvergeVia {
    /// The aggregate `brew bundle` — brew, cask, tap and vscode ride one file.
    BrewBundle,
    Flatpak,
    /// Separate and **forgiving**: mas is the flakiest provider (no App Store
    /// sign-in, an app not tied to the Apple ID), and riding `brew bundle` means
    /// one mas failure aborts the whole converge. Split out, its failures are
    /// warned and skipped (Principle #6).
    Mas,
}

fn converge_via(m: Manager) -> ConvergeVia {
    match m {
        Manager::Brew | Manager::Cask | Manager::Tap | Manager::Vscode => ConvergeVia::BrewBundle,
        Manager::Flatpak => ConvergeVia::Flatpak,
        Manager::Mas => ConvergeVia::Mas,
    }
}

/// What a package converge achieved, and what it could not.
///
/// The second field exists because a converge that partly failed used to be
/// expressible only two ways, and both were wrong. brew `bail!`ed, which stopped
/// every provider after it and the whole config phase for one bad cask; flatpak
/// discarded its child's verdict, so the failure reached the terminal and
/// nothing else. Neither could tell `--json` that the machine did not reach the
/// declared state.
///
/// Only the providers that run ONE aggregate child are named here, because for
/// them the child's exit status is the only signal there is. `mas`, `rpm-ostree`
/// and `gext` go through `batch_then_isolate`, which retries per item and warns
/// with the item's own name — that information is already delivered, and
/// repeating it as a provider-level failure would say less, not more.
pub struct Converged {
    /// Declared packages considered by this converge.
    pub considered: usize,
    /// Providers whose aggregate child reported failure. Named rather than
    /// counted: "brew could not finish" and "flatpak could not finish" send you
    /// to different places.
    pub failed: Vec<&'static str>,
}

pub fn converge(effective: &[Pkg], dry_run: bool, verbose: bool) -> Result<Converged> {
    let mut failed: Vec<&'static str> = Vec::new();
    let brewish: Vec<&Pkg> = effective
        .iter()
        .filter(|p| converge_via(p.manager) == ConvergeVia::BrewBundle)
        .collect();

    if !brewish.is_empty() && have("brew") && !dry_run {
        let body: String = brewish.iter().map(|p| format!("{}\n", p.raw)).collect();
        let tmp = std::env::temp_dir().join(format!("temper-Brewfile-{}", std::process::id()));
        std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
        let mut cmd = Command::new("brew");
        cmd.arg("bundle");
        // Quiet by default: `--quiet` drops the per-package "Using <formula>" line
        // printed for every already-installed dep. `--verbose` keeps brew's full
        // output. Real warnings/errors surface either way.
        if !verbose {
            cmd.arg("--quiet");
        }
        cmd.arg("--file").arg(&tmp);
        // Quiet path: capture and drive a spinner naming the package being
        // installed right now. `--verbose` streams brew's own output instead —
        // the spinner would fight it for the cursor.
        let failed_log = if verbose {
            let status = cmd.status().context("running brew bundle")?;
            (!status.success()).then(String::new)
        } else {
            let (ok, log) = run_with_spinner(cmd, "brew bundle", "converging packages")?;
            (!ok).then_some(log)
        };
        let _ = std::fs::remove_file(&tmp);
        if let Some(log) = failed_log {
            // Replay everything the capture swallowed (the exec-step contract:
            // quiet on success, the full story on failure) — then carry on.
            //
            // This used to `bail!`, and `run_install` calls `converge` with `?`,
            // so one failing entry aborted the entire converge: flatpak, GNOME
            // extensions and rpm-ostree never ran, and neither did a single
            // config step. brew does not behave that way itself — it attempts
            // every Brewfile entry and reports how many failed — so the
            // all-or-nothing was temper's alone, and brew was the only provider
            // that had it. Measured, because guessing a tool's unhappy path is
            // how the last defect here got in: two bad entries yield "Installing
            // <a> has failed! / Installing <b> has failed! / `brew bundle`
            // failed! 2 Brewfile dependencies failed to install", exit 1.
            //
            // Which entries failed is deliberately not parsed out of that. A
            // package manager's verdict is never temper's; the probe is. So the
            // honest report is "brew could not finish", and `drift` names every
            // package still missing from its own observation.
            if !log.is_empty() {
                eprint!("{log}");
            }
            failed.push("brew");
        }
    }

    let flatpaks: Vec<&str> = effective
        .iter()
        .filter(|p| converge_via(p.manager) == ConvergeVia::Flatpak)
        .map(|p| p.name.as_str())
        .collect();
    if !flatpaks.is_empty() && have("flatpak") && !dry_run {
        let mut cmd = Command::new("flatpak");
        // `--system` is spelled out rather than left to flatpak's default, which
        // is what it happens to be today. Removal names the same scope
        // explicitly, and the pair only stays a pair if neither side is
        // inherited: an unflagged install resolves against `--installation`
        // config and `FLATPAK_USER_DIR`, so "the default" is a property of the
        // host, not of temper.
        cmd.args(["install", "-y", "--noninteractive", "--system"]);
        for f in &flatpaks {
            cmd.arg(f);
        }
        // Best-effort: a missing remote or app must not abort the whole run —
        // but it must be *reported*, and reaching the terminal is only half of
        // that. `run_child` replays the log and warns; discarding its verdict
        // meant nothing recorded the failure, so `--json` described a converge
        // that had not happened.
        if !run_child(cmd, verbose, "flatpak install", "installing flatpaks") {
            failed.push("flatpak");
            if let Some(hint) = flatpak_polkit_hint("install", &flatpaks) {
                eprintln!("{} {hint}", crate::ui::yellow(crate::ui::g_warn()));
            }
        }
    }

    // Forgiving mas: install each App Store app on its own; a failure is warned
    // (to stderr, so `--json` stays clean) and skipped, never fatal.
    let mas: Vec<&Pkg> = effective
        .iter()
        .filter(|p| converge_via(p.manager) == ConvergeVia::Mas)
        .collect();
    if !mas.is_empty() && have("mas") && !dry_run {
        // Only install what's genuinely missing. Re-running `mas install` on an
        // already-installed app (every id in `mas list`) just prints a redundant
        // "Already installed" warning — and can trigger a bare password prompt.
        let present = mas_names();
        let todo: Vec<&Pkg> = mas
            .iter()
            .copied()
            .filter(|p| !present.contains_key(p.id.as_deref().unwrap_or(&p.name)))
            .collect();
        if !todo.is_empty() {
            // Heads-up: a `mas install` can surface a bare macOS "Password:" line
            // with no context — say what it's for before it appears.
            eprintln!(
                "→ Installing {} App Store app(s) via mas — macOS may prompt for your \
                 password to authorize the install.",
                todo.len()
            );
        }
        // One spinner for the whole App Store phase. These are full downloads, so
        // a big one otherwise looks like a hang.
        let pb = (!verbose && !todo.is_empty())
            .then(|| crate::ui::spinner_counted(1, "App Store"));
        if let Some(pb) = &pb {
            pb.set_message(format!("Installing {} app(s)", todo.len()));
        }
        // ONE `mas install` for the whole set. `mas install <id>…` is plural, and
        // batching is what turns N password prompts into one — the difference
        // between a converge you can walk away from and one you must babysit.
        // (The previous loop was justified as "mas has no batch mode". It does.)
        let ids: Vec<String> = todo
            .iter()
            .map(|p| p.id.clone().unwrap_or_else(|| p.name.clone()))
            .collect();
        batch_then_isolate(&ids, "mas install", verbose, |batch| {
            let mut cmd = Command::new("mas");
            // Quiet by default: mute mas's post-install "not indexed in Spotlight"
            // warnings. `--verbose` lets them through.
            if !verbose {
                cmd.env("MAS_NO_AUTO_INDEX", "1");
            }
            cmd.arg("install");
            for id in batch {
                cmd.arg(id);
            }
            cmd
        });
        if let Some(pb) = pb {
            pb.finish_and_clear();
        }
    }

    Ok(Converged { considered: effective.len(), failed })
}

/// `brew trust` third-party taps before any converge/upgrade. Homebrew 5.2+
/// gates untrusted taps and silently skips their formulae otherwise. Best-effort
/// (matches RIS); a no-op without brew or an empty list.
///
/// Quiet by default: `brew trust` prints `Already trusted tap: <tap>` for every
/// tap on every run — pure "already OK" noise. We swallow those confirmations but
/// still surface a genuinely new trust or any warning/error. `--verbose` shows
/// brew's full output.
pub fn trust_taps(taps: &[String], verbose: bool) -> Result<()> {
    if taps.is_empty() || !have("brew") {
        return Ok(());
    }
    let mut cmd = Command::new("brew");
    cmd.args(["trust", "--tap"]);
    for t in taps {
        cmd.arg(t);
    }
    if verbose {
        let _ = cmd.status();
        return Ok(());
    }
    if let Ok(out) = cmd.output() {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let l = line.trim_end();
            if !l.trim_start().starts_with("Already trusted tap:") && !l.trim().is_empty() {
                eprintln!("{l}"); // a new trust — surface it (to stderr; keeps --json clean)
            }
        }
        if !out.stderr.is_empty() {
            eprint!("{}", String::from_utf8_lossy(&out.stderr)); // warnings/errors
        }
    }
    Ok(())
}

/// `brew untrust` taps trusted on the machine but not declared — the prune
/// (machine→spec) counterpart of `trust_taps`. Best-effort; a no-op without brew
/// or an empty list. Output flows to the terminal (prune already confirmed).
pub fn untrust_taps(taps: &[String]) -> Result<()> {
    if taps.is_empty() || !have("brew") {
        return Ok(());
    }
    let mut cmd = Command::new("brew");
    cmd.args(["untrust", "--tap"]);
    for t in taps {
        cmd.arg(t);
    }
    if crate::ui::json_mode() {
        cmd.stdout(stderr_stdio());
    }
    if !cmd.status().map(|s| s.success()).unwrap_or(false) {
        eprintln!(
            "{} brew untrust failed — {} tap(s) may still be trusted",
            crate::ui::yellow(crate::ui::g_warn()),
            taps.len()
        );
    }
    Ok(())
}

/// Live tap-trust state: the taps Homebrew currently trusts (`brew trust --json
/// v1`, read-only). `None` when brew is absent — the caller then skips trust
/// drift entirely (a declared `[brew].trust` is meaningless without brew, so it
/// must NOT read as "everything untrusted"). Only the `taps` array is returned;
/// temper trusts at tap granularity (`brew trust --tap`), so trusted individual
/// formulae/casks/commands aren't temper's to manage and would be noise.
pub fn trusted_taps() -> Result<Option<Vec<String>>> {
    if !have("brew") {
        return Ok(None);
    }
    let out = Command::new("brew")
        .args(["trust", "--json", "v1"])
        .output()
        .context("running brew trust --json v1")?;
    if !out.status.success() {
        // An older brew without `trust` (or a transient failure): treat as "can't
        // tell", not "nothing trusted", so we don't cry drift on every tap.
        return Ok(None);
    }
    #[derive(serde::Deserialize)]
    struct Trust {
        #[serde(default)]
        taps: Vec<String>,
    }
    let parsed: Trust =
        serde_json::from_slice(&out.stdout).context("parsing brew trust --json v1")?;
    Ok(Some(parsed.taps))
}

/// `name → version` for every installed package (brew formulae **and** casks,
/// plus flatpak apps). Read from list output with explicit columns — never from a
/// tool's human sentence — so it is locale-proof.
///
/// This is the ground truth an upgrade is measured against, and it deliberately
/// does not ask a package manager what it considers "outdated": measured on a real
/// machine, `brew outdated --quiet` reported **nothing** while `brew upgrade`
/// went on to upgrade twelve packages. Trusting that number would have printed
/// "packages already current" over a run that upgraded a dozen — the very defect
/// this reporting exists to remove. Comparing versions before and after cannot
/// disagree with what happened.
///
/// Best-effort and guarded: empty without the CLIs, and a probe that fails
/// contributes nothing rather than failing the run. ~0.4s per brew snapshot.
pub fn installed_versions() -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    if have("brew") {
        // `brew list --versions` → "name 1.2.3" (sometimes several versions).
        for line in run_lines("brew", &["list", "--versions"]).unwrap_or_default() {
            if let Some((name, vers)) = line.split_once(char::is_whitespace) {
                out.insert(format!("brew:{name}"), vers.trim().to_string());
            }
        }
    }
    if have("flatpak") {
        for line in run_lines(
            "flatpak",
            &["list", "--app", "--columns=application,version"],
        )
        .unwrap_or_default()
        {
            let mut cols = line.split('\t');
            if let Some(app) = cols.next() {
                let ver = cols.next().unwrap_or("").trim().to_string();
                out.insert(format!("flatpak:{app}"), ver);
            }
        }
    }
    out
}

/// How many packages changed version between two [`installed_versions`]
/// snapshots. Only names present in **both** count: a transitive dependency that
/// an upgrade pulled in for the first time is not itself an upgrade.
pub fn upgraded_between(
    before: &std::collections::BTreeMap<String, String>,
    after: &std::collections::BTreeMap<String, String>,
) -> usize {
    after
        .iter()
        .filter(|(name, now)| before.get(*name).is_some_and(|was| was != *now))
        .count()
}

/// Upgrade installed packages (brew + flatpak). Best-effort; VM-verified. The
/// caller only invokes this when packages are actually declared, so a machine
/// with an empty set never triggers a global upgrade.
pub fn upgrade(verbose: bool) -> Result<Vec<&'static str>> {
    let mut failed: Vec<&'static str> = Vec::new();
    if have("brew") {
        let mut cmd = Command::new("brew");
        cmd.arg("upgrade");
        // `-y` skips Homebrew 6.0+'s upgrade confirmation prompt: the user already
        // consented by running `temper converge`/`temper update`, and a prompt here
        // would stall an otherwise-unattended converge.
        cmd.arg("-y");
        // Quiet by default: suppress the per-formula progress spam (an up-to-date
        // machine should be near-silent). `--verbose` keeps brew's full output.
        if !verbose {
            cmd.arg("--quiet");
        }
        if !run_child(cmd, verbose, "brew upgrade", "upgrading packages") {
            failed.push("brew");
        }
    }
    if have("flatpak") {
        let mut cmd = Command::new("flatpak");
        cmd.args(["update", "-y", "--noninteractive"]);
        // Captured (see `run_child`): flatpak prints `Nothing to update.` whenever
        // its remotes carry nothing new, which — mid-run, in flatpak's voice —
        // reads as temper's verdict on a converge that is about to install and
        // upgrade plenty. The download progress it prints instead becomes a
        // spinner, and a real failure is reported rather than discarded — which
        // means keeping the verdict, not only replaying the log to the terminal.
        if !run_child(cmd, verbose, "flatpak update", "upgrading flatpaks") {
            failed.push("flatpak");
        }
    }
    Ok(failed)
}

/// Remove installed-but-not-declared packages. brew-family goes through
/// dependency-aware `brew bundle cleanup --force` against the effective
/// Brewfile (so a kept package's transitive deps aren't removed); flatpak
/// extras are uninstalled by id. VM-verified.
/// The `brew bundle cleanup` type flag for a manager, or `None` where cleanup is
/// not how it is removed.
///
/// Exhaustive because the consequence is a silent cap: naming any type flag
/// turns the unnamed ones **off**, so a manager missing from this list is one
/// `cleanup` will not touch — previewed, confirmed, and quietly skipped. That is
/// the same failure the explicit flags were introduced to prevent, and it was a
/// hand-written list of five.
fn cleanup_flag(m: Manager) -> Option<&'static str> {
    match m {
        Manager::Brew => Some("--formula"),
        Manager::Cask => Some("--cask"),
        Manager::Tap => Some("--tap"),
        Manager::Mas => Some("--mas"),
        Manager::Vscode => Some("--vscode"),
        // Removed by an explicit `flatpak uninstall`, scoped to the user
        // installation — brew's flatpak cleanup extension is not what temper
        // uses, and passing a flag for it would hand brew a removal temper
        // reports on itself.
        Manager::Flatpak => None,
    }
}

/// `[ignore]` entries rendered as Brewfile tokens, so `brew bundle cleanup`
/// treats them as things that may stay.
///
/// `mas` is keyed by its numeric id (that is what `Pkg::match_name` yields and
/// what `[ignore].mas` holds), so the id stands in for the display name too —
/// cleanup matches on the id, and no name is available here anyway.
fn ignored_brewfile_lines(ignore: &crate::manifest::Ignore) -> Vec<String> {
    let mut out = Vec::new();
    // Driven from `Manager::ALL` and the exhaustive `ignore_list`, rather than a
    // sequence of `for` loops over named fields. The compiler checks a match and
    // checks nothing about the loops — so a manager added later would silently
    // lose its ignore protection here, which is the exact bug this function was
    // written to fix, one variant later.
    for &m in Manager::ALL {
        if m == Manager::Flatpak {
            continue; // not a Brewfile type; flatpak extras are removed separately
        }
        for name in crate::packages::ignore_list(ignore, m) {
            out.push(match m {
                // mas is keyed by its numeric id — that is what `match_name`
                // yields and what `[ignore].mas` holds, and cleanup matches on
                // it, so the id stands in for the display name too.
                Manager::Mas => format!("mas \"{name}\", id: {name}"),
                other => format!("{} \"{name}\"", other.as_str()),
            });
        }
    }
    out
}

/// Rewrite the cleanup Brewfile's body so every declared-trusted tap carries
/// `trusted: true`.
///
/// `brew bundle cleanup --force` calls `Homebrew::Trust.replace!` with whatever
/// `trusted:` options the file it was handed contains — **replace**, not merge,
/// so a file that mentions none empties the trust store outright. temper declares
/// trust in `[brew].trust` / `brew_trust` rather than in the Brewfile, so its
/// cleanup file mentioned none and every run wiped the lot.
///
/// A tap already in the body (a `tap "x"` line from a machine's Brewfile) gains
/// the option; one declared only as trust gets a line of its own. The taps being
/// untrusted *by this run* are trusted-but-undeclared by definition, so they are
/// absent from `trust` and correctly do not survive.
fn with_declared_trust(body: &str, trust: &[String]) -> String {
    let mut out = String::new();
    let mut seen: Vec<&str> = Vec::new();
    for line in body.lines() {
        let named = trust.iter().find(|t| {
            line.starts_with("tap ") && line.contains(&format!("\"{t}\"")) && !line.contains("trusted:")
        });
        match named {
            Some(t) => {
                seen.push(t.as_str());
                out.push_str(&format!("{line}, trusted: true\n"));
            }
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    for t in trust {
        if !seen.contains(&t.as_str()) && !body.contains(&format!("\"{t}\"")) {
            out.push_str(&format!("tap \"{t}\", trusted: true\n"));
        }
    }
    out
}

pub fn prune_apply(
    effective: &[Pkg],
    extras: &[(Manager, String)],
    brew_deps: &BrewExtras,
    ignore: &crate::manifest::Ignore,
    trust: &[String],
) -> Result<()> {
    let has_brewish = effective.iter().any(|p| {
        matches!(
            p.manager,
            Manager::Brew | Manager::Cask | Manager::Tap | Manager::Mas | Manager::Vscode
        )
    });
    if has_brewish && have("brew") {
        // The file brew reads is "what may stay", and that is the declared set
        // PLUS everything `[ignore]` covers. `[ignore]` never subtracts from the
        // declared set (Principle #4), so an ignored package is by definition
        // absent from `effective` — and building the cleanup file from
        // `effective` alone therefore handed brew every ignored package as an
        // orphan. It uninstalled them: unpreviewed, uncounted, unconfirmed, and
        // outside the plan the user confirmed.
        let mut body: String = effective
            .iter()
            .filter(|p| {
                matches!(
                    p.manager,
                    Manager::Brew | Manager::Cask | Manager::Tap | Manager::Mas | Manager::Vscode
                )
            })
            .map(|p| format!("{}\n", p.raw))
            .collect();
        for line in ignored_brewfile_lines(ignore) {
            body.push_str(&line);
            body.push('\n');
        }
        // …and everything cleanup would take that the confirmed plan did not
        // name: a formula that is unneeded already (brew's autoremove would take
        // it, and that is not this prune's to do), or anything that appeared
        // since the plan was made. Written as keepers, so cleanup removes the
        // plan and nothing more. Under the autoremove fallback the plan named
        // every unneeded formula, so only what appeared since is kept.
        let planned: HashSet<&str> = extras
            .iter()
            .map(|(_, n)| n.as_str())
            .chain(brew_deps.dependencies.iter().map(String::as_str))
            .collect();
        // Read before cleanup runs: `cleanup --force` marks every formula in its
        // file as asked for, keepers included, and this is what puts them back.
        let before = brew_record();
        let unneeded = if brew_deps.autoremove {
            before.as_ref().map(|r| r.dependencies.clone()).unwrap_or_default()
        } else {
            HashSet::new()
        };
        let casks: HashSet<String> = run_lines("brew", &["list", "--cask"])
            .unwrap_or_default()
            .into_iter()
            .collect();
        for (m, n) in brew_cleanup_list(effective, ignore, false).ok().flatten().unwrap_or_default() {
            if planned.contains(n.as_str()) || unneeded.contains(&n) {
                continue;
            }
            let line = match m {
                Manager::Tap => format!("tap \"{n}\"\n"),
                _ if casks.contains(&n) => format!("cask \"{n}\"\n"),
                _ => format!("brew \"{n}\"\n"),
            };
            body.push_str(&line);
        }
        // …and the same for trust, which brew reconciles from this file too:
        // `cleanup --force` calls `Trust.replace!` with the file's `trusted:`
        // options, so a file that declares none empties the store. Observed on a
        // real machine — one `prune` untrusted the tap temper itself installs
        // from, which then made `brew bundle cleanup` unable to compute extras
        // at all, so the *next* prune would have read every skipped formula as
        // an orphan.
        let body = with_declared_trust(&body, trust);
        let tmp =
            std::env::temp_dir().join(format!("temper-Brewfile-prune-{}", std::process::id()));
        fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
        // Name the types explicitly instead of inheriting brew's defaults.
        //
        // With no type flags, `brew bundle cleanup` cleans every supported type —
        // but `HOMEBREW_BUNDLE_CLEANUP_NO_CASK` and friends turn individual ones
        // off from the user's environment. temper would then preview a cask
        // removal, get it confirmed, watch brew skip it, and report success: a
        // silent cap (Principle #6) whose cause lives outside the repo. Passing
        // the flag for each type we actually put in the file makes the set
        // temper's decision, which is the only way temper can honestly report on
        // it. Note this must cover EVERY type present — naming one type turns
        // the others off.
        let mut flags: Vec<&str> = Vec::new();
        for &m in Manager::ALL {
            let Some(flag) = cleanup_flag(m) else { continue };
            if effective.iter().any(|p| p.manager == m) {
                flags.push(flag);
            }
        }
        // Streamed on purpose: prune is destructive, confirmed, and the user
        // is watching it happen. Under `--json` that stream would land in the
        // document, so it goes to stderr instead.
        // Homebrew's own autoremove after each uninstall is whole-system; the
        // dependencies this prune frees are already in the plan. Left on only
        // for the fallback, where the plan said it would run.
        let mut cleanup = Command::new("brew");
        if !brew_deps.autoremove {
            cleanup.env("HOMEBREW_NO_AUTOREMOVE", "1");
        }
        let status = cleanup
            .args(["bundle", "cleanup", "--force"])
            .args(&flags)
            .arg("--file")
            .arg(&tmp)
            .stdout(if crate::ui::json_mode() { stderr_stdio() } else { Stdio::inherit() })
            .status()
            .context("running brew bundle cleanup")?;
        let _ = fs::remove_file(&tmp);
        // `cleanup --force` marks every formula its file names as asked for — the
        // keepers above and the `[ignore]`d ones too. Left like that, the orphan
        // this prune kept reads as asked for at the next drift, and the next
        // prune removes it. What was a dependency before is one again; a
        // declared formula stays marked, as `brew bundle install` marks it.
        if let (Some(before), Some(after)) = (before, brew_record()) {
            let declared = declared_formulae(effective);
            let promoted: Vec<String> = after
                .asked
                .iter()
                .filter(|f| before.dependencies.contains(&f.name))
                .filter(|f| !std::iter::once(&f.name).chain(&f.names).any(|n| declared.contains(n)))
                .map(|f| f.name.clone())
                .collect();
            brew_mark_dependencies(&promoted);
        }
        if !status.success() {
            bail!("brew bundle cleanup failed");
        }
    }

    let flatpaks: Vec<&str> = extras
        .iter()
        .filter(|(m, _)| *m == Manager::Flatpak)
        .map(|(_, n)| n.as_str())
        .collect();
    // Group the extras by the installation holding each, then issue one batched
    // call per installation. An app in both yields a removal from both — it is
    // undeclared, and half-removing it would leave drift the next `drift` reports
    // and this run claimed to have cleared.
    //
    // The plan groups identically, so the confirm cannot overstate what this
    // does; this is the executor's own last check, not the only one.
    let mut by_scope: Vec<(&str, Vec<&str>)> =
        FLATPAK_SCOPES.iter().map(|s| (*s, Vec::new())).collect();
    if !flatpaks.is_empty() {
        match flatpak_app_scopes() {
            Some(live) => {
                let mut elsewhere: Vec<String> = Vec::new();
                for f in &flatpaks {
                    for (_, inst) in live.iter().filter(|(a, _)| a == f) {
                        match flatpak_scope_flag(inst) {
                            Some(flag) => {
                                if let Some((_, items)) =
                                    by_scope.iter_mut().find(|(s, _)| *s == flag)
                                {
                                    items.push(f);
                                }
                            }
                            // A custom installation needs `--installation=NAME`,
                            // and picking one for the user is not temper's call.
                            None => elsewhere.push(format!("{f} ({inst})")),
                        }
                    }
                }
                if !elsewhere.is_empty() {
                    eprintln!(
                        "{} {} flatpak(s) live in an installation temper does not manage \
                         and were NOT removed: {}",
                        crate::ui::yellow(crate::ui::g_warn()),
                        elsewhere.len(),
                        elsewhere.join(", ")
                    );
                }
            }
            None => {
                eprintln!(
                    "{} could not read which installation holds each flatpak — none removed",
                    crate::ui::yellow(crate::ui::g_warn())
                );
            }
        }
    }
    for (scope, items) in by_scope.iter().filter(|(_, i)| !i.is_empty()) {
        if !have("flatpak") {
            break;
        }
        let mut cmd = Command::new("flatpak");
        cmd.args(["uninstall", "-y", "--noninteractive", scope]);
        for f in items {
            cmd.arg(f);
        }
        // Streamed on purpose (unlike the converge children): `prune` is manual,
        // confirmed and destructive, the user is at the keyboard, and *what was
        // removed* is the deliverable. Under `--json` the same stream would land
        // in the document, so there it goes to stderr.
        if crate::ui::json_mode() {
            cmd.stdout(stderr_stdio());
        }
        if !cmd.status().map(|s| s.success()).unwrap_or(false) {
            eprintln!(
                "{} flatpak uninstall {scope} failed — {} extra(s) may remain",
                crate::ui::yellow(crate::ui::g_warn()),
                items.len()
            );
            if *scope == "--system" {
                if let Some(hint) = flatpak_polkit_hint("uninstall", items) {
                    eprintln!("{} {hint}", crate::ui::yellow(crate::ui::g_warn()));
                }
            }
        }
    }
    Ok(())
}

// --- gext: GNOME extensions (Linux desktop) -----------------------------------

/// Union of a machine's composed apps' `extensions`, de-duplicated.
/// Every extension this machine declares, fleet-then-machine, first declaration
/// of a uuid winning. The **spec**, not just the uuid, because whether an
/// extension should be switched on travels with the declaration that names it.
pub fn effective_extension_specs(
    home: &Path,
    machine: &Machine,
) -> Result<Vec<manifest::GnomeExtension>> {
    let mut out: Vec<manifest::GnomeExtension> = Vec::new();
    // First declaration wins WITHIN a tier, so bundle order is stable.
    let push = |e: &manifest::GnomeExtension, out: &mut Vec<manifest::GnomeExtension>| {
        if !out.iter().any(|x| x.uuid() == e.uuid()) {
            out.push(e.clone());
        }
    };
    for app in &machine.apps {
        let bundle = manifest::load_bundle(home, app)?;
        // Bundle-level os/role gate: a server (or a Mac) never layers a
        // desktop-Linux bundle's GNOME extensions, even if it composes it.
        if manifest::gated(&bundle.os, &bundle.role, machine) {
            continue;
        }
        for e in &bundle.gnome_extensions {
            push(e, &mut out);
        }
    }
    // The machine's own list wins outright, rather than being unioned in behind
    // the bundles': it is the MORE SPECIFIC declaration, and the whole point of
    // machine scope is "this box differs". Keeping the bundle's entry meant a
    // machine asking for `enabled = false` on a uuid its bundle also declared
    // was silently overruled — the attribute was dropped on the floor and the
    // extension stayed switched on, with nothing reporting why.
    for e in &machine.gnome_extensions {
        if let Some(slot) = out.iter_mut().find(|x| x.uuid() == e.uuid()) {
            *slot = e.clone();
        } else {
            out.push(e.clone());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod extension_dconf_path_tests {
    /// The rule the code used to apply — uuid in, path out — against the paths
    /// the extensions on a real desktop actually declare. Not one matches, and
    /// the failures are not a single systematic transform: the domain goes, but
    /// also case changes, names change outright, and two nest several levels
    /// deep. Any future attempt to "just derive it" has to answer this table.
    #[test]
    fn no_rule_maps_a_uuid_to_its_subtree() {
        // (uuid, the subtree it really uses) — read off installed gschemas.
        let real = [
            ("tilingshell@ferrarodomenico.com", "tilingshell"),
            ("CoverflowAltTab@palatis.blogspot.com", "coverflowalttab"),
            ("appindicatorsupport@rgcjonas.gmail.com", "appindicator"),
            ("rounded-window-corners@fxgn", "rounded-window-corners-reborn"),
            ("logomenu@aryan_k", "Logo-menu"),
            (
                "compiz-windows-effect@hermes83.github.com",
                "com/github/hermes83/compiz-windows-effect",
            ),
        ];
        // Each plausible rule, and a case that kills it. Dropping the domain is
        // right often enough to look like the answer — which is exactly how the
        // guess survived review.
        type Rule = (&'static str, fn(&str) -> String);
        let rules: [Rule; 3] = [
            ("the uuid itself", |u| u.to_string()),
            ("drop the domain", |u| {
                u.split('@').next().unwrap().to_string()
            }),
            ("drop the domain, lowercased", |u| {
                u.split('@').next().unwrap().to_lowercase()
            }),
        ];
        for (name, rule) in rules {
            assert!(
                real.iter().any(|(uuid, path)| rule(uuid) != *path),
                "`{name}` maps every uuid to its real subtree — if that is true, \
                 the probe is unnecessary and this test is the thing that is wrong"
            );
        }
        // …and no rule at all gets the nested one.
        assert_eq!(
            real.iter()
                .find(|(u, _)| u.starts_with("compiz-windows-effect"))
                .map(|(_, p)| *p),
            Some("com/github/hermes83/compiz-windows-effect"),
            "the schema path can be nested several levels below the uuid"
        );
    }

    #[test]
    fn the_schema_path_is_read_out_of_the_gschema() {
        let xml = r#"<?xml version="1.0"?>
<schemalist>
  <schema id="org.gnome.shell.extensions.tilingshell"
          path="/org/gnome/shell/extensions/tilingshell/">
    <key name="x" type="b"><default>true</default></key>
  </schema>
</schemalist>"#;
        assert_eq!(
            super::first_shell_extension_path(xml).as_deref(),
            Some("/org/gnome/shell/extensions/tilingshell/")
        );
    }

    /// An extension may ship a schema for keyspace it does not own. Claiming
    /// `/org/gnome/desktop/interface/` as "its subtree" would hand one extension
    /// ownership of keys the whole desktop shares.
    #[test]
    fn a_schema_outside_the_extensions_tree_is_not_its_subtree() {
        let xml = r#"<schemalist>
  <schema id="org.gnome.desktop.interface" path="/org/gnome/desktop/interface/"/>
</schemalist>"#;
        assert_eq!(super::first_shell_extension_path(xml), None);

        // …but it must still find the real one when both are present.
        let both = r#"<schemalist>
  <schema id="org.gnome.desktop.interface" path="/org/gnome/desktop/interface/"/>
  <schema id="org.gnome.shell.extensions.foo" path="/org/gnome/shell/extensions/foo/"/>
</schemalist>"#;
        assert_eq!(
            super::first_shell_extension_path(both).as_deref(),
            Some("/org/gnome/shell/extensions/foo/")
        );
    }

    /// The extensions root itself is not any one extension's subtree — taking it
    /// would make that extension own every other extension's settings.
    #[test]
    fn the_bare_extensions_root_is_rejected() {
        let xml = r#"<schema path="/org/gnome/shell/extensions/"/>"#;
        assert_eq!(super::first_shell_extension_path(xml), None);
    }
}

#[cfg(test)]
mod extension_scope_tests {
    use crate::manifest::GnomeExtension;

    /// The machine's declaration replaces the bundle's for the same uuid.
    ///
    /// Composition kept the FIRST entry per uuid and pushed bundles first, so a
    /// machine saying `{ uuid = "x", enabled = false }` about something its
    /// bundle also declared was silently overruled: the attribute vanished and
    /// the extension stayed switched on. Machine scope exists to say "this box
    /// differs" — losing that is losing the point.
    #[test]
    fn the_machines_declaration_wins_over_a_bundles() {
        // The composition rule, exercised directly on the shape the loop builds.
        let bundle = GnomeExtension::Uuid("x@y".into());
        let machine = GnomeExtension::Spec(crate::manifest::GnomeExtensionSpec {
            uuid: "x@y".into(),
            enabled: false,
            settings: None,
            settings_path: None,
        });
        let mut out = [bundle];
        if let Some(slot) = out.iter_mut().find(|e| e.uuid() == machine.uuid()) {
            *slot = machine.clone();
        }
        assert_eq!(out.len(), 1, "still one declaration, not two");
        assert!(
            !out[0].enabled(),
            "the machine asked for it switched off and must be heard"
        );
    }
}

/// Which bundle file declares this thing, if a bundle does — `None` when the
/// machine's own block does.
///
/// Scope decides the verb set, and it is a property of the *declaration*, not of
/// the kind. `KIND_ANSWERS` can only answer per kind, so drift told everyone to
/// run `temper reconcile` for a missing extension, rpm or remote — and
/// reconcile's candidates are machine-scope only, so for a bundle-declared one
/// it silently did nothing. That is the bug this whole model exists to prevent,
/// one notch narrower than the original. Naming the file is the honest answer,
/// and SPEC already claimed drift did it.
pub fn declaring_bundle(
    home: &Path,
    machine: &Machine,
    kind: DeclKind,
    item: &str,
) -> Option<String> {
    for app in &machine.apps {
        let Ok(bundle) = manifest::load_bundle(home, app) else {
            continue;
        };
        if manifest::gated(&bundle.os, &bundle.role, machine) {
            continue;
        }
        let hit = match kind {
            DeclKind::GnomeExtension => bundle.gnome_extensions.iter().any(|e| e.uuid() == item),
            DeclKind::RpmOstree => bundle.rpm_ostree.iter().any(|r| r == item),
            DeclKind::System(pm) => pm.bundle_list(&bundle).iter().any(|r| r == item),
            DeclKind::FlatpakRemote => bundle
                .flatpak_remotes
                .iter()
                .filter_map(|t| parse_remote(t))
                .any(|(n, _)| n == item),
        };
        if hit {
            return Some(format!("apps/{app}.toml"));
        }
    }
    None
}

/// The categories `declaring_bundle` can answer for — the ones whose absorb cell
/// names `temper reconcile`.
#[derive(Clone, Copy)]
pub enum DeclKind {
    GnomeExtension,
    RpmOstree,
    FlatpakRemote,
    System(crate::syspkg::SysPm),
}

/// Every snapshot this machine's declared extensions bring with them.
///
/// Returned alongside `machine.dconf` everywhere a snapshot is captured,
/// compared or restored, so an extension's settings get the same treatment the
/// machine's own subtrees do — the same observability guard, the same ownership
/// filter, the same journaling — without a second implementation.
pub fn extension_snapshots(
    home: &Path,
    machine: &Machine,
) -> Result<Vec<manifest::DconfSnapshot>> {
    let mut out = Vec::new();
    for e in effective_extension_specs(home, machine)? {
        if e.settings_file().is_none() {
            continue;
        }
        match extension_dconf_path(&e) {
            Some(path) => out.extend(e.settings_snapshot(&path)),
            // Declared settings temper cannot place. Skipping silently would
            // capture nothing into a file that reads as "this extension has no
            // settings" — say it instead, and name the field that fixes it.
            None => eprintln!(
                "warning: cannot tell which dconf subtree `{}` stores its \
                 settings in — it is not installed here, or ships no schema. \
                 Its `settings` file is left alone. Declare `settings_path = \
                 \"/org/gnome/shell/extensions/<subtree>/\"` to say where.",
                e.uuid()
            ),
        }
    }
    Ok(out)
}

/// Which dconf subtree an extension keeps its settings in.
///
/// **Not** `/org/gnome/shell/extensions/<uuid>/`. GNOME derives the path from
/// the extension's gschema, and on a real fleet none of nineteen installed
/// extensions matched the uuid: the domain is dropped, case changes, and some
/// bear no resemblance at all (`appindicatorsupport@rgcjonas.gmail.com` →
/// `appindicator`, `rounded-window-corners@fxgn` →
/// `rounded-window-corners-reborn`). Guessing produced empty captures that
/// reported success.
///
/// So this reads the schema the extension ships. A declared `settings_path`
/// wins — it is the answer for an extension that is not installed on this box.
pub fn extension_dconf_path(e: &manifest::GnomeExtension) -> Option<String> {
    if let Some(p) = e.declared_dconf_path() {
        let mut p = p.to_string();
        if !p.ends_with('/') {
            p.push('/');
        }
        return Some(p);
    }
    let home = std::env::var("HOME").ok()?;
    let roots = [
        PathBuf::from(&home).join(".local/share/gnome-shell/extensions"),
        PathBuf::from("/usr/share/gnome-shell/extensions"),
    ];
    for root in roots {
        let schemas = root.join(e.uuid()).join("schemas");
        let Ok(entries) = std::fs::read_dir(&schemas) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|d| d.path())
            .filter(|p| p.extension().is_some_and(|x| x == "xml"))
            .collect();
        // One extension can ship several schemas; sort so the answer does not
        // depend on directory order.
        files.sort();
        for f in files {
            let Ok(text) = std::fs::read_to_string(&f) else {
                continue;
            };
            if let Some(p) = first_shell_extension_path(&text) {
                return Some(p);
            }
        }
    }
    None
}

/// The first `path="/org/gnome/shell/extensions/…/"` in a gschema document.
///
/// Deliberately not an XML parse: the attribute is unambiguous, and a schema
/// file that fails to parse should still yield its path rather than take the
/// feature down. Restricted to the extensions tree so a schema that also
/// declares, say, `/org/gnome/desktop/…` cannot claim shared keyspace.
fn first_shell_extension_path(xml: &str) -> Option<String> {
    const PREFIX: &str = "/org/gnome/shell/extensions/";
    for (i, _) in xml.match_indices("path=\"") {
        let rest = &xml[i + 6..];
        let end = rest.find('"')?;
        let p = &rest[..end];
        if p.starts_with(PREFIX) && p.len() > PREFIX.len() {
            let mut p = p.to_string();
            if !p.ends_with('/') {
                p.push('/');
            }
            return Some(p);
        }
    }
    None
}

/// Just the uuids — what `install` and the extras diff work in.
pub fn effective_extensions(home: &Path, machine: &Machine) -> Result<Vec<String>> {
    Ok(effective_extension_specs(home, machine)?
        .iter()
        .map(|e| e.uuid().to_string())
        .collect())
}

/// What this host can do about GNOME extensions — answered **once**, and
/// consulted by every cell of the matrix.
///
/// The two abilities are genuinely independent, which is why one predicate is
/// not enough and five ad-hoc ones were far too many. `gnome-extensions` ships
/// with gnome-shell and can only **enumerate**; `gext` is a separate install and
/// is the only thing that can install or uninstall. Conflating them produced a
/// host — GNOME with `gnome-extensions` but no `gext`, the ordinary case — where
/// `drift` reported missing extensions forever and `install --packages-only`
/// silently did nothing about them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GextCaps {
    /// Can enumerate what is installed. Required before **any** finding: on a
    /// host that cannot look, "declared but absent" and "I cannot tell" are the
    /// same observation, and only one of them may be acted on.
    pub observe: bool,
    /// Can install and uninstall. Required before naming a converge verb.
    pub converge: bool,
}

pub fn gext_caps() -> GextCaps {
    GextCaps {
        observe: have("gnome-extensions"),
        converge: have("gext"),
    }
}

/// `gnome-extensions list [args]`, three-valued: `None` means *could not ask*
/// (tool absent, or it failed), which is not the same fact as an empty list.
///
/// This mirrors `trusted_taps` — the one place in the tree that already models
/// capability as "the tool ran and said nothing" vs "I could not ask". Folding
/// the two together is what lets an absent lister read as "nothing installed",
/// and from there as "every declared extension is missing".
fn gext_list(args: &[&str]) -> Option<Vec<String>> {
    if !gext_caps().observe {
        return None;
    }
    let out = Command::new("gnome-extensions").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
    )
}

/// Extensions GNOME currently has switched **on**, three-valued.
pub fn gext_enabled() -> Option<Vec<String>> {
    gext_list(&["list", "--enabled"])
}

/// Declared extensions whose switched-on state does not match the declaration.
///
/// Returns `(uuid, should_be_enabled)`. Only *installed* extensions are
/// considered: one that is missing is already reported as `gnome-extension`, and
/// also calling it "not enabled" would be one fact wearing two hats.
///
/// This is the cell that closes the silent soft-failure. "Installed" and
/// "enabled" used to be two unlinked facts — the uuid in `gnome_extensions`, the
/// switch in a captured dconf key — so a uuid enabled in a snapshot but declared
/// nowhere was switched on by `restore` and never installed by `install`. GNOME
/// fails soft, so nothing said a word.
pub fn gext_enable_drift(specs: &[manifest::GnomeExtension]) -> Vec<(String, bool)> {
    if specs.is_empty() {
        return Vec::new();
    }
    let (Some(installed), Some(enabled)) = (gext_list(&["list"]), gext_enabled()) else {
        return Vec::new();
    };
    specs
        .iter()
        .filter(|e| installed.iter().any(|i| i == e.uuid()))
        .filter(|e| enabled.iter().any(|x| x == e.uuid()) != e.enabled())
        .map(|e| (e.uuid().to_string(), e.enabled()))
        .collect()
}

/// Switch declared extensions on or off to match their declaration.
///
/// `gnome-extensions enable/disable` rather than writing `enabled-extensions`
/// directly: the tool owns that key's semantics, and writing the list wholesale
/// would drop the image-baked extensions temper never declared. A **union**, not
/// a replacement — temper asserts its own declarations and leaves the rest alone.
pub fn gext_enable_converge(drift: &[(String, bool)], dry_run: bool) -> Result<usize> {
    if dry_run || drift.is_empty() || !gext_caps().observe {
        return Ok(0);
    }
    let mut done = 0;
    for (uuid, want) in drift {
        let verb = if *want { "enable" } else { "disable" };
        let ok = Command::new("gnome-extensions")
            .args([verb, uuid])
            .stdin(Stdio::null())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            done += 1;
        } else {
            eprintln!(
                "{} gnome-extensions {verb} {uuid} failed — skipped",
                crate::ui::yellow(crate::ui::g_warn())
            );
        }
    }
    Ok(done)
}

/// Extensions installed in the **user** scope (`~/.local/share/...`). System
/// ones are excluded on purpose: those ship with the image, and drift reports
/// image-baked items status-only rather than as something you failed to declare.
///
/// This is where extensions genuinely differ from flatpaks, which `prune` removes
/// from both installations. A system extension lives in `/usr/share/gnome-shell/
/// extensions`, part of the ostree image — removing one means rebuilding the
/// image, not running a command. A system *flatpak* is just where the desktop's
/// storefront puts an app the user chose, so it is ordinary state.
fn gext_installed_user() -> Option<Vec<String>> {
    gext_list(&["list", "--user"])
}

/// User-installed extensions no bundle or machine declares — the extras
/// direction. Honors `[ignore].gext`.
///
/// Absorbed by `reconcile` into the machine's **own** `extensions` list; a
/// bundle's list is shared, so that is where an absorb must never land.
///
/// Gated on the machine declaring at least one extension, exactly like every
/// other manager (SPEC's probe invariant). Without that gate a spec that doesn't
/// manage extensions at all — including a bare test fixture — would report every
/// hand-installed extension on the host, making drift depend on the machine
/// rather than the spec. Declaring one opts in.
pub fn gext_extras(effective: &[String], ignore: &manifest::Ignore) -> Vec<String> {
    if effective.is_empty() {
        return Vec::new();
    }
    let Some(installed_user) = gext_installed_user() else {
        return Vec::new();
    };
    gext_extras_from(&installed_user, effective, &ignore.gnome_extensions)
}

/// The set logic behind `gext_extras`, split from the shell-out so it is
/// unit-testable: user-installed, minus declared, minus ignored, sorted.
fn gext_extras_from(
    installed_user: &[String],
    effective: &[String],
    ignore: &[String],
) -> Vec<String> {
    let mut out: Vec<String> = installed_user
        .iter()
        .filter(|u| !effective.contains(u) && !ignore.contains(u))
        .cloned()
        .collect();
    out.sort();
    out
}

/// Declared extensions that are not installed.
///
/// Requires the **lister**, not the installer: a host with `gext` alone cannot
/// enumerate, so it would report every declared extension as missing. That was
/// the old predicate, and it made drift permanently red on a host where the
/// named remedy provably could not work.
pub fn gext_missing(effective: &[String]) -> Vec<String> {
    if effective.is_empty() {
        return Vec::new();
    }
    let Some(installed) = gext_list(&["list"]) else {
        return Vec::new();
    };
    gext_absent_from(&installed, effective)
}

/// Extensions a machine declares in its **own** `extensions` list that are not
/// installed — the undeclare cell, and the answer to "I removed this on purpose
/// and every converge puts it back".
///
/// Machine-scope only, for the reason every absorb is machine-scope: a bundle's
/// `extensions` is shared, so dropping one there from a single box would change
/// every machine composing that bundle. A bundle-declared extension that is
/// absent stays a hand edit — and `drift` now names the file.
///
/// Requires the lister, for the same reason `gext_missing` does, but the stakes
/// are higher in this direction: on a host that cannot enumerate, an unguarded
/// drop would offer to delete the machine's entire declared list.
/// Declared extensions whose **live enable-state** differs from what the machine
/// declares — as `(uuid, what the machine actually is)`.
///
/// The spec←machine half of `gext_enable_drift`, which returns what the
/// declaration wants. `install` converges toward the declaration; this is the
/// other direction, and without it the enable cell had a converge answer and no
/// absorb: `drift` reported the extension forever, `reconcile` offered nothing,
/// and the only route was a hand edit.
///
/// Restricted to `machine_own` on purpose. A bundle's list is shared, so
/// flipping `enabled` there from one box changes every machine composing it —
/// the same containment rule that keeps package absorbs in the machine's own
/// Brewfile.
pub fn gext_enable_absorbable(
    specs: &[manifest::GnomeExtension],
    machine_own: &[String],
) -> Vec<(String, bool)> {
    gext_enable_drift(specs)
        .into_iter()
        .filter(|(uuid, _)| machine_own.iter().any(|m| m == uuid))
        // `gext_enable_drift` yields what the DECLARATION wants; absorbing means
        // taking what the machine is, which is its negation.
        .map(|(uuid, wanted)| (uuid, !wanted))
        .collect()
}

pub fn gext_machine_absent(machine_own: &[String]) -> Vec<String> {
    if machine_own.is_empty() {
        return Vec::new();
    }
    let Some(installed) = gext_list(&["list"]) else {
        return Vec::new();
    };
    gext_absent_from(&installed, machine_own)
}

/// Declared minus installed, sorted and de-duplicated — the set logic behind
/// both absent-direction queries, split from the shell-out so it is testable.
fn gext_absent_from(installed: &[String], declared: &[String]) -> Vec<String> {
    let mut out: Vec<String> = declared
        .iter()
        .filter(|e| !installed.contains(e))
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Uninstall user-scope extensions via `gext` — the prune side of gext, so an
/// `extension-extra` has a command instead of only a hand edit. Best-effort per
/// UUID (one failure must not strand the rest), and loud about any that failed.
pub fn gext_uninstall(uuids: &[String]) -> Result<()> {
    if uuids.is_empty() {
        return Ok(());
    }
    if !gext_caps().converge {
        bail!("gext not found — cannot uninstall GNOME extensions on this host");
    }
    let landed = batch_then_isolate(uuids, "gext uninstall", false, |batch| {
        let mut c = Command::new("gext");
        c.arg("uninstall");
        for u in batch {
            c.arg(u);
        }
        c
    });
    if landed.len() != uuids.len() {
        let failed: Vec<&String> = uuids.iter().filter(|u| !landed.contains(u)).collect();
        bail!(
            "could not uninstall: {}",
            failed
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

/// Install missing extensions via `gext`. VM-verified.
///
/// One spinner for the whole phase with a counter (the `mas` shape): extensions
/// install one at a time, and `gext`'s own per-extension chatter would otherwise
/// stand in temper's output as if it were temper speaking. A failure is warned
/// and skipped — one unavailable extension must not fail a converge.
pub fn gext_converge(effective: &[String], dry_run: bool, verbose: bool) -> Result<Vec<String>> {
    if dry_run {
        return Ok(Vec::new());
    }
    // No installer: say so instead of returning quietly. Silence here meant
    // `drift` reported missing extensions, named `install --packages-only` as
    // the fix, and the converge that ran did nothing and reported nothing —
    // a permanent red with no way to learn why (Principle #6).
    let missing = gext_missing(effective);
    if !gext_caps().converge {
        if !missing.is_empty() {
            eprintln!(
                "{} gext not found — {} declared GNOME extension(s) cannot be installed on this host",
                crate::ui::yellow(crate::ui::g_warn()),
                missing.len()
            );
        }
        return Ok(Vec::new());
    }
    let pb = (!verbose)
        .then(|| crate::ui::spinner_counted(1, "GNOME extensions"));
    if let Some(pb) = &pb {
        pb.set_message(format!("Installing {} extension(s)", missing.len()));
    }
    let installed = batch_then_isolate(&missing, "gext install", verbose, |batch| {
        let mut c = Command::new("gext");
        c.arg("install");
        for u in batch {
            c.arg(u);
        }
        c
    });
    if let Some(pb) = pb {
        pb.finish_and_clear();
    }
    Ok(installed)
}

// --- rpm repos: where layered packages come from (the prerequisite) ----------

/// The mode/owner every repo and key file is installed with.
///
/// Not configurable. dnf reads these paths as root and a repo file is not a
/// place for per-machine policy, so exposing `mode`/`owner` the way `sysfile`
/// does would only add ways to write a repo dnf then refuses to read.
fn rpm_repo_opts() -> crate::primitives::SysfileOpts<'static> {
    crate::primitives::SysfileOpts {
        mode: Some("0644"),
        owner: Some("root"),
        group: Some("root"),
    }
}

/// What this host can do about RPM repositories, answered once.
///
/// Gated on **a consumer of `/etc/yum.repos.d`, not on atomic-ness**. The
/// directory is dnf's, and means the same thing on Bazzite, on CoreOS and on a
/// plain Fedora or EL box; only which tool resolves from it differs — dnf, or
/// rpm-ostree, which reads it per transaction. Gating on `have("rpm-ostree")`
/// alone would make the category vanish on a dnf host that holds repos
/// perfectly well.
///
/// And not on `rpm` alone either: openSUSE has rpm and resolves through zypper,
/// which reads `/etc/zypp/repos.d`. A repo written there is read by nothing, so
/// on such a host the category is `unavailable` with that reason, rather than a
/// file temper writes, reports in sync, and nothing ever consults.
pub fn rpm_repo_caps() -> RepoCaps {
    let rpm = have("rpm");
    RepoCaps {
        observe: rpm,
        converge: rpm && (have("dnf") || have("yum") || have("rpm-ostree")),
    }
}

/// Why a host with rpm cannot take a repo — said wherever the category is
/// reported unavailable, so the reader is not left to guess.
pub const RPM_REPO_NO_CONSUMER: &str =
    "this host has rpm but neither dnf nor rpm-ostree, so nothing reads /etc/yum.repos.d \
     (openSUSE resolves through zypper and /etc/zypp/repos.d)";

/// What this host can do about apt sources: the directory is apt's, so apt is
/// both the reader and the reason to write.
pub fn apt_source_caps() -> RepoCaps {
    let apt = have("apt-get");
    RepoCaps {
        observe: apt,
        converge: apt,
    }
}

/// Why an apt source is not installed on a host without apt.
pub const APT_SOURCE_NO_APT: &str = "this host has no apt";

/// Its own type, not `RpmOstreeCaps`, for the reason the category exists: a
/// repo's capabilities are not the layering tool's. Sharing the struct would
/// have read as "repos are an rpm-ostree feature" on the one host where that is
/// most wrong — a plain dnf box, which has repos and no layering at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepoCaps {
    /// Can read the repository directory.
    pub observe: bool,
    /// Can write it, and something on this host will read what is written.
    pub converge: bool,
}

/// A declared repository file and the key it cites, installed root-owned to
/// fixed destinations: an `rpm_repos` entry or an `apt_sources` one. The two
/// differ in where the files go and which tool reads them, and in nothing this
/// module does with them — so the state, the dedup and the converge are written
/// once.
pub trait RepoFile: Clone {
    /// Where the repository file itself goes — the dedup key.
    fn dest(&self) -> PathBuf;
    /// Source/destination pairs in apply order: the key before the file that
    /// cites it.
    fn files(&self) -> Vec<(String, PathBuf)>;
}

impl RepoFile for manifest::RpmRepo {
    fn dest(&self) -> PathBuf {
        manifest::RpmRepo::dest(self)
    }
    fn files(&self) -> Vec<(String, PathBuf)> {
        manifest::RpmRepo::files(self)
    }
}

impl RepoFile for manifest::AptSource {
    fn dest(&self) -> PathBuf {
        manifest::AptSource::dest(self)
    }
    fn files(&self) -> Vec<(String, PathBuf)> {
        manifest::AptSource::files(self)
    }
}

/// A declared repo and the spec unit that declared it.
///
/// The owner travels with the repo because the dedup below is what decides it:
/// re-deriving "who declared this" from a second walk of the bundles can name a
/// different file than the one whose copy actually converges, and a finding that
/// names the wrong file to edit is worse than one that names none.
#[derive(Debug, Clone)]
pub struct OwnedRepo<R = manifest::RpmRepo> {
    /// The bundle that declared it, or `None` for the machine's own list.
    pub from: Option<String>,
    pub repo: R,
}

/// One declared repo/key file: where it goes, how it compares, and who declared
/// it. A repo and the key it cites are one declaration, so both rows carry the
/// same owner.
#[derive(Debug, Clone)]
pub struct RepoState {
    /// The bundle that declared it, or `None` for the machine's own `rpm_repos`.
    pub from: Option<String>,
    pub dest: PathBuf,
    pub state: crate::primitives::FileState,
}

/// Every repo this machine declares — its bundles' (gated), then its own.
///
/// Deduped by **destination**, not by source path: two bundles naming the same
/// vendor file from different asset paths still install one repo, and the first
/// declaration wins, which is the rule `effective_rpm` and `effective_remotes`
/// already use. The winner is the one that carries its declaring bundle forward,
/// so the report names the file whose bytes are on disk.
pub fn effective_rpm_repos(home: &Path, machine: &Machine) -> Result<Vec<OwnedRepo>> {
    effective_repo_files(home, machine, |b| &b.rpm_repos, &machine.rpm_repos)
}

/// Every apt source this machine declares, by the same rule as its repos.
pub fn effective_apt_sources(
    home: &Path,
    machine: &Machine,
) -> Result<Vec<OwnedRepo<manifest::AptSource>>> {
    effective_repo_files(home, machine, |b| &b.apt_sources, &machine.apt_sources)
}

fn effective_repo_files<R: RepoFile>(
    home: &Path,
    machine: &Machine,
    of_bundle: impl Fn(&manifest::Bundle) -> &[R],
    own: &[R],
) -> Result<Vec<OwnedRepo<R>>> {
    let mut out: Vec<OwnedRepo<R>> = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |from: Option<&str>, r: &R, out: &mut Vec<OwnedRepo<R>>| {
        if seen.insert(r.dest()) {
            out.push(OwnedRepo {
                from: from.map(str::to_string),
                repo: r.clone(),
            });
        }
    };
    for app in &machine.apps {
        let bundle = manifest::load_bundle(home, app)?;
        if manifest::gated(&bundle.os, &bundle.role, machine) {
            continue;
        }
        for r in of_bundle(&bundle) {
            push(Some(app), r, &mut out);
        }
    }
    for r in own {
        push(None, r, &mut out);
    }
    Ok(out)
}

/// The drift state of every declared repo/key file, in apply order.
///
/// Three-valued through `FileState`: a destination temper cannot read is
/// `Unavailable`, never `Missing` — reading "cannot look" as "not there" is what
/// makes a converge rewrite state it never inspected. Each row keeps the repo's
/// declaring bundle, since a key and the repo that cites it are one declaration
/// and answer to one file.
pub fn repo_files_state<R: RepoFile>(
    home: &Path,
    effective: &[OwnedRepo<R>],
) -> Result<Vec<RepoState>> {
    let mut out = Vec::new();
    for owned in effective {
        for (src, dest) in owned.repo.files() {
            let state =
                crate::primitives::sysfile_state(&home.join(&src), &dest, &rpm_repo_opts())?;
            out.push(RepoState {
                from: owned.from.clone(),
                dest,
                state,
            });
        }
    }
    Ok(out)
}

/// The destinations a converge would write — what the root ask has to cover.
///
/// `plan::root_steps` derives the "one password for the whole run" ask from the
/// step list, and a repo is not a step. Without this the ask misses it, and a
/// machine with no root-needing casks and no `sudo` step gets its password
/// prompt in the middle of the converge instead of at the keyboard before
/// anything downloads.
pub fn rpm_repos_pending(home: &Path, effective: &[OwnedRepo]) -> Vec<String> {
    pending_writes(home, effective, rpm_repo_caps())
}

/// `rpm_repos_pending` for apt sources.
pub fn apt_sources_pending(home: &Path, effective: &[OwnedRepo<manifest::AptSource>]) -> Vec<String> {
    pending_writes(home, effective, apt_source_caps())
}

fn pending_writes<R: RepoFile>(home: &Path, effective: &[OwnedRepo<R>], caps: RepoCaps) -> Vec<String> {
    if !caps.converge {
        return Vec::new();
    }
    repo_files_state(home, effective)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.state != crate::primitives::FileState::InSync)
        .map(|r| r.dest.display().to_string())
        .collect()
}

/// Install every declared repo, key first. Returns the destinations written.
///
/// Runs **before** any package converge. A layered package resolves against
/// what is in `/etc/yum.repos.d` at the moment `rpm-ostree install` runs, so a
/// repo written afterwards is a repo that did not exist for the only call that
/// needed it: every package from it fails to resolve, and — because a failed
/// batch is retried per item — fails once each, burying any genuine failure in
/// a wall of expected ones.
pub fn rpm_repos_converge(
    home: &Path,
    effective: &[OwnedRepo],
    dry_run: bool,
) -> Result<Vec<String>> {
    let caps = rpm_repo_caps();
    let why_not = if caps.observe {
        RPM_REPO_NO_CONSUMER
    } else {
        "this host has no rpm"
    };
    repo_files_converge(home, effective, caps, "repo", why_not, dry_run)
}

/// Install every declared apt source, key first, before any apt install — the
/// same contract as `rpm_repos_converge`. The `apt-get update` that makes a new
/// source visible runs as part of the install that needs it.
pub fn apt_sources_converge(
    home: &Path,
    effective: &[OwnedRepo<manifest::AptSource>],
    dry_run: bool,
) -> Result<Vec<String>> {
    repo_files_converge(home, effective, apt_source_caps(), "source", APT_SOURCE_NO_APT, dry_run)
}

fn repo_files_converge<R: RepoFile>(
    home: &Path,
    effective: &[OwnedRepo<R>],
    caps: RepoCaps,
    noun: &str,
    why_not: &str,
    dry_run: bool,
) -> Result<Vec<String>> {
    if dry_run || effective.is_empty() {
        return Ok(Vec::new());
    }
    if !caps.converge {
        // Only the machine's own: a bundle's are inert on a host that cannot
        // read them, as its package lists are, so a mixed fleet's bundle does
        // not warn on every run of every other family's machines.
        let own = effective.iter().filter(|o| o.from.is_none()).count();
        if own > 0 {
            eprintln!(
                "{} {own} declared {noun}(s) not installed: {why_not}",
                crate::ui::yellow(crate::ui::g_warn()),
            );
        }
        return Ok(Vec::new());
    }
    let mut written = Vec::new();
    for owned in effective {
        // `files()` yields the key before the file that cites it. A
        // `gpgkey=file:///etc/pki/rpm-gpg/…` or `Signed-By:` reference is checked
        // when the repo is first consulted, so a key installed after its repo is
        // the same ordering bug one level down.
        //
        // So a key that fails to install stops its repo too: written anyway, the
        // repo would cite a key that is not there, and every package from it
        // would fail signature checks rather than report the repo missing.
        let files = owned.repo.files();
        let last = files.len() - 1;
        for (i, (src, dest)) in files.into_iter().enumerate() {
            match crate::primitives::sysfile_apply(&home.join(&src), &dest, &rpm_repo_opts()) {
                Ok(true) => written.push(dest.display().to_string()),
                Ok(false) => {}
                // Warned and skipped rather than fatal, like a remote that
                // fails to add: one unreadable asset must not strand the repos
                // that are fine, and the package that needed it will report
                // itself missing in the same run.
                Err(e) => {
                    eprintln!(
                        "{} could not install {}: {e}",
                        crate::ui::yellow(crate::ui::g_warn()),
                        dest.display()
                    );
                    if i < last {
                        eprintln!(
                            "{} {} not installed either — it cites that key",
                            crate::ui::yellow(crate::ui::g_warn()),
                            owned.repo.dest().display()
                        );
                    }
                    break;
                }
            }
        }
    }
    Ok(written)
}

// --- pacman repos: where pacman packages come from ---------------------------

/// What this host can do about pacman repositories: pacman reads them.
pub fn pacman_repo_caps() -> RepoCaps {
    let pacman = have("pacman");
    RepoCaps {
        observe: pacman,
        converge: pacman,
    }
}

/// Why a pacman repo is not installed on a host without pacman.
pub const PACMAN_REPO_NO_PACMAN: &str = "this host has no pacman";

impl RepoFile for manifest::PacmanRepo {
    fn dest(&self) -> PathBuf {
        manifest::PacmanRepo::dest(self)
    }
    fn files(&self) -> Vec<(String, PathBuf)> {
        manifest::PacmanRepo::files(self)
    }
}

/// Every pacman repo this machine declares, by the same rule as its rpm repos.
pub fn effective_pacman_repos(
    home: &Path,
    machine: &Machine,
) -> Result<Vec<OwnedRepo<manifest::PacmanRepo>>> {
    effective_repo_files(home, machine, |b| &b.pacman_repos, &machine.pacman_repos)
}

const PACMAN_CONF: &str = "/etc/pacman.conf";
const PACMAN_GNUPG: &str = "/etc/pacman.d/gnupg";
const PACMAN_INCLUDE_MARKER: &str = "pacman-repos";
const PACMAN_PLACEHOLDER_BODY: &str = "\
# temper: keeps the Include in /etc/pacman.conf matching a file. pacman refuses
# its whole configuration when an Include pattern matches none, so this stays
# while temper's Include does. The other files here are temper's pacman_repos.
";

/// The placeholder, then the `Include` — the order they are written in, so the
/// `Include` never matches no file.
fn pacman_plumbing() -> [(PathBuf, String); 2] {
    [
        (
            Path::new(manifest::PACMAN_REPOS_DIR).join(manifest::PACMAN_REPOS_PLACEHOLDER),
            PACMAN_PLACEHOLDER_BODY.to_string(),
        ),
        (
            PathBuf::from(PACMAN_CONF),
            format!("Include = {}/*.conf\n", manifest::PACMAN_REPOS_DIR),
        ),
    ]
}

/// Content temper computes, as a file a primitive can read.
fn staged(what: &str, body: &str) -> Result<PathBuf> {
    let p = std::env::temp_dir().join(format!("temper-{what}-{}", std::process::id()));
    fs::write(&p, body).with_context(|| format!("staging {}", p.display()))?;
    Ok(p)
}

/// The state of what makes pacman read temper's repo directory: the placeholder
/// file, and the `Include` as a marked region of `pacman.conf`.
pub fn pacman_plumbing_state() -> Result<Vec<RepoState>> {
    let mut out = Vec::new();
    for (i, (dest, body)) in pacman_plumbing().into_iter().enumerate() {
        let src = staged("pacman-plumbing", &body)?;
        let state = if i == 0 {
            crate::primitives::sysfile_state(&src, &dest, &rpm_repo_opts())
        } else {
            crate::primitives::block_state(&src, &dest, PACMAN_INCLUDE_MARKER, &rpm_repo_opts())
        };
        let _ = fs::remove_file(&src);
        out.push(RepoState {
            from: None,
            dest,
            state: state?,
        });
    }
    Ok(out)
}

/// Write the placeholder, then the `Include`. Returns whether either changed.
fn pacman_plumbing_apply() -> Result<bool> {
    let mut changed = false;
    for (i, (dest, body)) in pacman_plumbing().into_iter().enumerate() {
        let src = staged("pacman-plumbing", &body)?;
        let wrote = if i == 0 {
            crate::primitives::sysfile_apply(&src, &dest, &rpm_repo_opts())
        } else {
            // A root-owned region is written through `sudo install` and is not
            // journaled, so the journal here records nothing.
            let mut unused = crate::journal::Journal::begin();
            crate::primitives::block_apply(&src, &dest, PACMAN_INCLUDE_MARKER, &rpm_repo_opts(), &mut unused)
        };
        let _ = fs::remove_file(&src);
        changed |= wrote?;
    }
    Ok(changed)
}

/// A command run as root: through `sudo` unless temper already is root.
fn as_root(program: &str) -> Command {
    if crate::syspkg::is_root() {
        Command::new(program)
    } else {
        let mut c = Command::new("sudo");
        c.arg(program);
        c
    }
}

/// How pacman's keyring holds a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    Absent,
    /// In the keyring, but not locally signed, so packages it signed are not
    /// trusted.
    Unsigned,
    Trusted,
}

/// The primary-key fingerprints an exported key file holds — read from the file
/// alone, no keyring needed. `None` when gpg cannot read it.
fn key_fingerprints(file: &Path) -> Option<Vec<String>> {
    let out = Command::new("gpg")
        .args(["--batch", "--with-colons", "--show-keys"])
        .arg(file)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let fprs = primary_fingerprints(&String::from_utf8_lossy(&out.stdout));
    (!fprs.is_empty()).then_some(fprs)
}

/// The `fpr` record that follows each `pub` record in gpg's colon listing.
/// Pure, so it is testable.
fn primary_fingerprints(colons: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut after_pub = false;
    for line in colons.lines() {
        let f: Vec<&str> = line.split(':').collect();
        match f.first().copied() {
            Some("pub") => after_pub = true,
            Some("fpr") if after_pub => {
                if let Some(fpr) = f.get(9).filter(|s| !s.is_empty()) {
                    out.push(fpr.to_string());
                }
                after_pub = false;
            }
            Some("sub") => after_pub = false,
            _ => {}
        }
    }
    out
}

/// How pacman's keyring holds `fpr`, read from the keyring directly — which
/// needs no root. `None` when the keyring cannot be read.
fn pacman_key_state(fpr: &str) -> Option<KeyState> {
    let out = Command::new("gpg")
        .args(["--homedir", PACMAN_GNUPG, "--batch", "--no-auto-check-trustdb", "--with-colons", "--list-sigs", fpr])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return String::from_utf8_lossy(&out.stderr)
            .contains("No public key")
            .then_some(KeyState::Absent);
    }
    Some(key_state_of(&String::from_utf8_lossy(&out.stdout)))
}

/// A key `pacman-key --lsign-key` signed carries a signature whose class ends in
/// `l` (local); an exportable one ends in `x`. Pure, so it is testable.
fn key_state_of(colons: &str) -> KeyState {
    let local = colons.lines().any(|l| {
        let f: Vec<&str> = l.split(':').collect();
        f.first() == Some(&"sig") && f.get(10).is_some_and(|c| c.ends_with('l'))
    });
    if local {
        KeyState::Trusted
    } else {
        KeyState::Unsigned
    }
}

/// Every declared repo key and how pacman's keyring holds it: `(declaring
/// bundle, what to call it, state)`. A key file gpg cannot read, or a keyring it
/// cannot open, is `None` — unavailable, never absent.
pub fn pacman_keys_state(
    home: &Path,
    effective: &[OwnedRepo<manifest::PacmanRepo>],
) -> Vec<(Option<String>, String, Option<KeyState>)> {
    let mut out = Vec::new();
    for owned in effective {
        let Some(key) = &owned.repo.key else { continue };
        let label = |what: &str| format!("{what} (key for {})", owned.repo.dest().display());
        match key_fingerprints(&home.join(key)) {
            None => out.push((owned.from.clone(), label(key), None)),
            Some(fprs) => {
                for fpr in fprs {
                    out.push((owned.from.clone(), label(&fpr), pacman_key_state(&fpr)));
                }
            }
        }
    }
    out
}

/// What a converge would write as root — the root ask has to cover it.
pub fn pacman_repos_pending(
    home: &Path,
    effective: &[OwnedRepo<manifest::PacmanRepo>],
) -> Vec<String> {
    let caps = pacman_repo_caps();
    if !caps.converge || effective.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = pacman_plumbing_state()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.state != crate::primitives::FileState::InSync)
        .map(|r| r.dest.display().to_string())
        .collect();
    out.extend(pending_writes(home, effective, caps));
    out.extend(
        pacman_keys_state(home, effective)
            .into_iter()
            .filter(|(_, _, s)| *s != Some(KeyState::Trusted))
            .map(|(_, what, _)| format!("pacman's keyring: {what}")),
    );
    out
}

/// Where the fingerprints of keys temper imported live — so `prune` deletes
/// only those, never a key the machine had before.
fn pacman_imported_path(machine: &str) -> PathBuf {
    crate::journal::state_root()
        .join("pacman-keys")
        .join(format!("{machine}.json"))
}

/// The keys temper imported into pacman's keyring on this machine.
pub fn pacman_imported_keys(machine: &str) -> BTreeSet<String> {
    fs::read(pacman_imported_path(machine))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_pacman_imported(machine: &str, keys: &BTreeSet<String>) -> Result<()> {
    let p = pacman_imported_path(machine);
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&p, serde_json::to_vec_pretty(keys)?)?;
    Ok(())
}

fn quietly(mut cmd: Command) -> bool {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Put a key into pacman's keyring and sign it locally, so packages it signed
/// are trusted. Returns the fingerprints it added that were not there before —
/// the ones `prune` may delete — or `None` when the import failed.
fn pacman_key_import(file: &Path, fprs: &[String]) -> Option<Vec<String>> {
    let before: Vec<(String, Option<KeyState>)> =
        fprs.iter().map(|f| (f.clone(), pacman_key_state(f))).collect();
    if before.iter().any(|(_, s)| *s == Some(KeyState::Absent)) {
        let mut add = as_root("pacman-key");
        add.arg("--add").arg(file);
        if !quietly(add) {
            return None;
        }
    }
    for (fpr, state) in &before {
        if *state != Some(KeyState::Trusted) {
            let mut sign = as_root("pacman-key");
            sign.args(["--lsign-key", fpr]);
            if !quietly(sign) {
                return None;
            }
        }
    }
    Some(
        before
            .into_iter()
            .filter(|(_, s)| *s == Some(KeyState::Absent))
            .map(|(f, _)| f)
            .collect(),
    )
}

/// Remove keys temper imported from pacman's keyring. Returns the ones removed.
pub fn pacman_keys_remove(machine: &str, fprs: &[String]) -> Vec<String> {
    let mut imported = pacman_imported_keys(machine);
    let mut removed = Vec::new();
    for fpr in fprs {
        let mut del = as_root("pacman-key");
        del.args(["--delete", fpr]);
        if quietly(del) || pacman_key_state(fpr) == Some(KeyState::Absent) {
            imported.remove(fpr);
            removed.push(fpr.clone());
        }
    }
    let _ = save_pacman_imported(machine, &imported);
    removed
}

/// The keys `prune` removes: imported by temper, and no longer the key of any
/// repo this machine declares. `None` when a declared key cannot be read — then
/// which keys are still needed is unknown, and none is removed.
pub fn pacman_keys_unneeded(
    home: &Path,
    machine: &str,
    effective: &[OwnedRepo<manifest::PacmanRepo>],
) -> Option<Vec<String>> {
    let imported = pacman_imported_keys(machine);
    if imported.is_empty() || !pacman_repo_caps().converge {
        return Some(Vec::new());
    }
    let mut needed = HashSet::new();
    for owned in effective {
        if let Some(key) = &owned.repo.key {
            needed.extend(key_fingerprints(&home.join(key))?);
        }
    }
    Some(imported.into_iter().filter(|f| !needed.contains(f)).collect())
}

/// The `[name]` a repo file declares — what pacman calls its database.
fn pacman_repo_name(file: &Path) -> Option<String> {
    fs::read_to_string(file).ok()?.lines().find_map(|l| {
        let l = l.trim();
        (l.starts_with('[') && l.ends_with(']') && l != "[options]")
            .then(|| l[1..l.len() - 1].to_string())
    })
}

/// Fetch one repo's database, and no other: pacman, run with a configuration of
/// its own options and this repo alone. Syncing every database without
/// upgrading is the partial upgrade Arch does not support; fetching the database
/// of a repo pacman has never synced is not that, and without it the packages
/// the same run declares from the repo cannot be found. Returns whether it
/// fetched.
fn pacman_sync_one(repo_file: &Path) -> bool {
    let Ok(conf) = Command::new("pacman-conf").stdin(Stdio::null()).output() else {
        return false;
    };
    // `pacman-conf` prints the effective configuration, `[options]` first.
    let options: String = String::from_utf8_lossy(&conf.stdout)
        .lines()
        .take_while(|l| !l.starts_with('[') || *l == "[options]")
        .map(|l| format!("{l}\n"))
        .collect();
    let Ok(tmp) = staged(
        "pacman-sync.conf",
        &format!("{options}\nInclude = {}\n", repo_file.display()),
    ) else {
        return false;
    };
    let mut sync = as_root("pacman");
    sync.arg("--config").arg(&tmp).args(["-Sy", "--noconfirm"]);
    let ok = quietly(sync);
    let _ = fs::remove_file(&tmp);
    ok
}

/// Whether pacman has a database for the repo a file declares.
fn pacman_has_db(repo_file: &Path) -> bool {
    let Some(name) = pacman_repo_name(repo_file) else {
        return true;
    };
    let db = run_lines("pacman-conf", &["DBPath"])
        .ok()
        .and_then(|l| l.into_iter().next())
        .unwrap_or_else(|| "/var/lib/pacman/".into());
    Path::new(&db).join("sync").join(format!("{name}.db")).exists()
}

/// Install every declared pacman repo — the `Include` and its placeholder, each
/// repo's key into pacman's keyring, then the repo file — before any pacman
/// install, and fetch the database of a repo pacman has not synced. A key that
/// cannot be imported stops its repo, as an apt source's does. Returns what was
/// written.
pub fn pacman_repos_converge(
    home: &Path,
    machine: &str,
    effective: &[OwnedRepo<manifest::PacmanRepo>],
    dry_run: bool,
) -> Result<Vec<String>> {
    if dry_run || effective.is_empty() {
        return Ok(Vec::new());
    }
    let warn = |msg: String| eprintln!("{} {msg}", crate::ui::yellow(crate::ui::g_warn()));
    if !pacman_repo_caps().converge {
        let own = effective.iter().filter(|o| o.from.is_none()).count();
        if own > 0 {
            warn(format!("{own} declared pacman repo(s) not installed: {PACMAN_REPO_NO_PACMAN}"));
        }
        return Ok(Vec::new());
    }
    let mut written = Vec::new();
    match pacman_plumbing_apply() {
        Ok(true) => written.push(PACMAN_CONF.to_string()),
        Ok(false) => {}
        Err(e) => {
            warn(format!(
                "could not make pacman read {}: {e} — no pacman repo is installed",
                manifest::PACMAN_REPOS_DIR
            ));
            return Ok(written);
        }
    }
    let mut imported = pacman_imported_keys(machine);
    for owned in effective {
        let dest = owned.repo.dest();
        if let Some(key) = &owned.repo.key {
            let file = home.join(key);
            let Some(fprs) = key_fingerprints(&file) else {
                warn(format!("could not read the key {key} — {} not installed, it needs that key", dest.display()));
                continue;
            };
            match pacman_key_import(&file, &fprs) {
                Some(added) => imported.extend(added),
                None => {
                    warn(format!(
                        "could not add {key} to pacman's keyring — {} not installed, it needs that key",
                        dest.display()
                    ));
                    continue;
                }
            }
        }
        match crate::primitives::sysfile_apply(&home.join(&owned.repo.repo), &dest, &rpm_repo_opts()) {
            Ok(wrote) => {
                if wrote {
                    written.push(dest.display().to_string());
                }
                if (wrote || !pacman_has_db(&dest)) && !pacman_sync_one(&dest) {
                    warn(format!(
                        "could not fetch the database of {} — its packages are found after `sudo pacman -Syu`",
                        dest.display()
                    ));
                }
            }
            Err(e) => warn(format!("could not install {}: {e}", dest.display())),
        }
    }
    let _ = save_pacman_imported(machine, &imported);
    Ok(written)
}

// --- rpm-ostree: layered rpms that can't be image-baked (Linux) ---------------

pub fn effective_rpm(home: &Path, machine: &Machine) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for app in &machine.apps {
        let bundle = manifest::load_bundle(home, app)?;
        // Bundle-level os/role gate: a server never rpm-ostree-layers a
        // desktop bundle's packages (the proton-vpn footgun in the ROADMAP).
        if manifest::gated(&bundle.os, &bundle.role, machine) {
            continue;
        }
        for pkg in bundle.rpm_ostree {
            if seen.insert(pkg.clone()) {
                out.push(pkg);
            }
        }
    }
    // The machine's own list, unioned last — the same rule `packages` and
    // `gnome_extensions` use, and what gives rpm-ostree a spec column at all.
    for pkg in &machine.rpm_ostree {
        if seen.insert(pkg.clone()) {
            out.push(pkg.clone());
        }
    }
    Ok(out)
}

/// Every installed flatpak app paired with the **installation** holding it,
/// three-valued.
///
/// `flatpak list --app` merges installations, which is right for "is it here?"
/// and silent about "where would I remove it from?". The `installation` column
/// answers the second in the same call, so one invocation serves both — and an
/// app present in *two* installations yields two rows, which is the case that
/// makes subtracting one list from another wrong.
///
/// The value is flatpak's own name for the installation: `system`, `user`, or
/// the name of a custom one from `/etc/flatpak/installations.d/`. Only the first
/// two are removable — see [`FLATPAK_SCOPES`] — because a custom installation
/// needs `--installation=NAME`, and guessing which is not temper's call.
///
/// `None` means the set could not be enumerated, which is never "nothing is
/// installed" (Principle #12).
pub fn flatpak_app_scopes() -> Option<Vec<(String, String)>> {
    if !have("flatpak") {
        return None;
    }
    let out = Command::new("flatpak")
        .args(["list", "--app", "--columns=application,installation"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let (app, inst) = l.split_once('\t')?;
                let (app, inst) = (app.trim(), inst.trim());
                (!app.is_empty() && !inst.is_empty())
                    .then(|| (app.to_string(), inst.to_string()))
            })
            .collect(),
    )
}

/// The installations temper may remove from, and the flag that names each.
///
/// Both, because an extra is an extra wherever it sits: temper *converges* into
/// one installation, but "this should not be here" is a different question from
/// "where do I put things". Removal is one batched call **per installation** —
/// `flatpak uninstall` refuses a combined `--user --system` when an app is in
/// both ("Multiple installed refs match … unable to proceed in non-interactive
/// mode"), so the scope flags do not compose the way `list`'s do. Two calls, each
/// batched over every item in its scope, is still one call per installation
/// rather than one per item (Principle #4).
pub const FLATPAK_SCOPES: [&str; 2] = ["--system", "--user"];

/// Whether this run is in an ssh session. Installing or removing a system-wide
/// flatpak is authorized by polkit, which lets an administrator at the desktop
/// through without a password and, over ssh, wants one it has no way to ask for.
/// A heuristic — a polkit rule can let remote administrators through too — so it
/// warns, and never refuses.
pub fn over_ssh() -> bool {
    ["SSH_CONNECTION", "SSH_TTY"]
        .iter()
        .any(|v| std::env::var_os(v).is_some_and(|x| !x.is_empty()))
}

/// Said when a system-wide flatpak step failed where polkit cannot ask — over
/// ssh, or with no terminal: the reason, and the command that works from here.
pub fn flatpak_polkit_hint(verb: &str, ids: &[&str]) -> Option<String> {
    let unasked = over_ssh() || !std::io::IsTerminal::is_terminal(&std::io::stdin());
    (unasked && !ids.is_empty()).then(|| {
        format!(
            "system-wide flatpaks are authorized by polkit, which has no way to ask for a \
             password over ssh or without a terminal. From here: sudo flatpak {verb} --system {}",
            ids.join(" ")
        )
    })
}

/// The flag that removes from the installation flatpak calls `inst`, or `None`
/// where temper has no way to name it.
///
/// The `None` arm is the honest half: `/etc/flatpak/installations.d/` can define
/// any number of installations, and reaching one needs `--installation=NAME`.
/// Mapping an unrecognised name onto `--system` would remove from an
/// installation the user did not point at.
pub fn flatpak_scope_flag(inst: &str) -> Option<&'static str> {
    match inst {
        "system" => Some("--system"),
        "user" => Some("--user"),
        _ => None,
    }
}

// --- flatpak remotes ------------------------------------------------------
//
// A remote is where a flatpak comes FROM, and it was the one thing about flatpak
// temper did not model: a declared app from a vendor remote simply could not be
// installed, and the converge degraded to a warning. It is also the flatpak
// analogue of tap-trust — the same fleet/group/machine scope question — so it
// gets the same three-scope treatment rather than a fleet list nobody can gate.

/// A declared remote: `"<name> <url>"`. The **name** is the identity (flatpak
/// allows only one remote per name); the url is the value that can drift.
pub fn parse_remote(token: &str) -> Option<(String, String)> {
    let (name, url) = token.split_once(char::is_whitespace)?;
    let (name, url) = (name.trim(), url.trim());
    (!name.is_empty() && !url.is_empty()).then(|| (name.to_string(), url.to_string()))
}

/// Every remote this machine declares: fleet, then its composed bundles (gated
/// with them), then its own. First declaration of a name wins, so a machine
/// cannot silently redefine a group's remote — that would be a spec edit.
pub fn effective_remotes(home: &Path, machine: &Machine) -> Result<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = Vec::new();
    let push = |t: &String, out: &mut Vec<(String, String)>| {
        if let Some((n, u)) = parse_remote(t) {
            if !out.iter().any(|(x, _)| *x == n) {
                out.push((n, u));
            }
        }
    };
    for app in &machine.apps {
        let b = manifest::load_bundle(home, app)?;
        if manifest::gated(&b.os, &b.role, machine) {
            continue;
        }
        for t in &b.flatpak_remotes {
            push(t, &mut out);
        }
    }
    for t in &machine.flatpak_remotes {
        push(t, &mut out);
    }
    Ok(out)
}

/// Every remote in the **system** installation, three-valued.
///
/// The system installation is where `install` puts every flatpak, and an app can
/// only come from a remote in its own installation — so a declared remote is
/// there or it is no use, whatever the user installation holds. That one the
/// image provides there already satisfies a declaration. `None` is "could not
/// ask", never "no remotes".
pub fn flatpak_remotes_installed() -> Option<Vec<(String, String)>> {
    remotes_in_scope(&["--system"])
}

/// Remotes visible in the given scope.
fn remotes_in_scope(scope: &[&str]) -> Option<Vec<(String, String)>> {
    if !have("flatpak") {
        return None;
    }
    let out = Command::new("flatpak")
        .args(["remotes"])
        .args(scope)
        .args(["--columns=name,url"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let (n, u) = l.split_once('\t')?;
                Some((n.trim().to_string(), u.trim().to_string()))
            })
            .collect(),
    )
}

/// Declared remotes the system installation lacks, or holds under a different
/// url.
pub fn remotes_missing(effective: &[(String, String)]) -> Vec<String> {
    if effective.is_empty() {
        return Vec::new();
    }
    let Some(live) = flatpak_remotes_installed() else {
        return Vec::new();
    };
    effective
        .iter()
        .filter(|(n, u)| !live.iter().any(|(ln, lu)| ln == n && remote_url_matches(u, lu)))
        .map(|(n, u)| format!("{n} {u}"))
        .collect()
}

/// Whether a declared remote's url describes the one configured. A
/// `.flatpakrepo` url names the file the remote is made from, not the repository
/// — flatpak stores the `Url=` inside the file — so such a remote matches by name
/// alone, and is never re-pointed at the file. A repository url must match.
fn remote_url_matches(declared: &str, live: &str) -> bool {
    is_flatpakrepo(declared) || declared.trim_end_matches('/') == live.trim_end_matches('/')
}

fn is_flatpakrepo(url: &str) -> bool {
    url.to_ascii_lowercase().ends_with(".flatpakrepo")
}

/// What a converge would add as root — the root ask has to cover it.
pub fn remotes_pending(effective: &[(String, String)]) -> Vec<String> {
    remotes_missing(effective)
        .into_iter()
        .map(|r| format!("flatpak remote {r}"))
        .collect()
}

/// Where the names of the remotes temper added live — so `prune` removes only
/// those, never one the image or a person put there, flathub above all.
fn remotes_added_path(machine: &str) -> PathBuf {
    crate::journal::state_root()
        .join("flatpak-remotes")
        .join(format!("{machine}.json"))
}

/// The remotes temper added to the system installation on this machine.
pub fn remotes_added(machine: &str) -> BTreeSet<String> {
    fs::read(remotes_added_path(machine))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_remotes_added(machine: &str, names: &BTreeSet<String>) -> Result<()> {
    let p = remotes_added_path(machine);
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&p, serde_json::to_vec_pretty(names)?)?;
    Ok(())
}

/// Remotes temper added that nothing declares any more, and that are still
/// there. Only those: an undeclared remote temper did not add — flathub, which
/// every installed app updates from — is not an extra, however it got there.
pub fn remotes_extras(
    effective: &[(String, String)],
    ignore: &manifest::Ignore,
    machine: &str,
) -> Vec<String> {
    let added = remotes_added(machine);
    if added.is_empty() {
        return Vec::new();
    }
    let Some(live) = flatpak_remotes_installed() else {
        return Vec::new();
    };
    added
        .into_iter()
        .filter(|n| {
            live.iter().any(|(ln, _)| ln == n)
                && !effective.iter().any(|(en, _)| en == n)
                && !ignore.flatpak_remote.contains(n)
        })
        .collect()
}

/// Add declared remotes to the system installation, as root — `sudo`, the same
/// one ask as a repo file, rather than polkit, which has no way to ask over ssh.
/// A remote already there under another url is re-pointed (`remote-modify`):
/// the name is the identity and the url is what drifts. Only a remote this adds
/// is recorded as temper's.
pub fn remotes_converge(
    effective: &[(String, String)],
    machine: &str,
    dry_run: bool,
) -> Result<Vec<String>> {
    if dry_run || effective.is_empty() || !have("flatpak") {
        return Ok(Vec::new());
    }
    let missing = remotes_missing(effective);
    if missing.is_empty() {
        return Ok(Vec::new());
    }
    let present: Vec<String> = flatpak_remotes_installed()
        .unwrap_or_default()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    let mut added = remotes_added(machine);
    let mut changed = Vec::new();
    for token in &missing {
        let Some((name, url)) = parse_remote(token) else {
            continue;
        };
        let exists = present.contains(&name);
        let mut cmd = as_root("flatpak");
        if exists {
            cmd.args(["remote-modify", "--system", "--url", &url, &name]);
        } else {
            cmd.args(["remote-add", "--system", "--if-not-exists", &name, &url]);
        }
        let out = cmd.stdin(Stdio::null()).output();
        match out {
            Ok(o) if o.status.success() => {
                if !exists {
                    added.insert(name.clone());
                }
                changed.push(name);
            }
            Ok(o) => eprintln!(
                "{} flatpak remote {name} not {}: {}",
                crate::ui::yellow(crate::ui::g_warn()),
                if exists { "re-pointed" } else { "added" },
                String::from_utf8_lossy(&o.stderr).trim()
            ),
            Err(e) => eprintln!(
                "{} flatpak remote {name} not added: {e}",
                crate::ui::yellow(crate::ui::g_warn())
            ),
        }
    }
    let _ = save_remotes_added(machine, &added);
    Ok(changed)
}

/// Remove remotes temper added — the prune side. Without `--force`: flatpak
/// refuses to delete a remote installed apps still update from, and that
/// refusal is the guard, said rather than overridden. Returns the ones left.
pub fn remotes_delete(machine: &str, names: &[String]) -> Vec<String> {
    let mut added = remotes_added(machine);
    let mut left = Vec::new();
    for name in names {
        let mut cmd = as_root("flatpak");
        cmd.args(["remote-delete", "--system", name]).stdin(Stdio::null());
        match cmd.output() {
            Ok(o) if o.status.success() => {
                added.remove(name);
            }
            Ok(o) => {
                eprintln!(
                    "{} flatpak remote {name} not removed: {}",
                    crate::ui::yellow(crate::ui::g_warn()),
                    String::from_utf8_lossy(&o.stderr).trim()
                );
                left.push(name.clone());
            }
            Err(e) => {
                eprintln!(
                    "{} flatpak remote {name} not removed: {e}",
                    crate::ui::yellow(crate::ui::g_warn())
                );
                left.push(name.clone());
            }
        }
    }
    let _ = save_remotes_added(machine, &added);
    left
}

/// What this host can do about rpm-ostree layering, answered once.
///
/// `rpm` and `rpm-ostree` are different facts: a plain Fedora or RHEL box has
/// `rpm` and no ostree, and layering there is not a thing temper can do. Gating
/// the whole category on `have("rpm-ostree")` made it *vanish* on such a host
/// rather than report that it does not apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RpmOstreeCaps {
    /// Can enumerate what is layered and what is staged.
    pub observe: bool,
    /// Can layer and unlayer.
    pub converge: bool,
}

pub fn rpm_ostree_caps() -> RpmOstreeCaps {
    let atomic = have("rpm-ostree");
    RpmOstreeCaps {
        observe: atomic,
        converge: atomic,
    }
}

/// The packages `rpm-ostree` has been *asked* to layer, across the booted
/// deployment and any staged one — `requested-packages` from
/// `rpm-ostree status --json`.
///
/// This is the source of truth `rpm -q` is not. `rpm -q` answers about the
/// **booted** deployment, so between layering a package and rebooting, a
/// correctly-layered package reads as `missing` — permanently red on an atomic
/// box in exactly the window where the user has already done the work. Reading
/// the staged deployment closes that, and the same field gives the extras
/// direction for free: layered-but-undeclared was never a design impossibility,
/// just an unread field.
///
/// `None` means the store could not be read (Principle #12) — never an empty
/// set, which every write path would take as "nothing is layered".
pub fn rpm_ostree_requested() -> Option<Vec<String>> {
    if !rpm_ostree_caps().observe {
        return None;
    }
    let out = Command::new("rpm-ostree")
        .args(["status", "--json"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    requested_from_status(&out.stdout)
}


#[cfg(test)]
mod remote_url_tests {
    use super::remote_url_matches;

    /// A `.flatpakrepo` file's url is not the repository url flatpak stores, so
    /// a remote declared by it matches by name; a repository url must match.
    #[test]
    fn a_flatpakrepo_declaration_matches_by_name() {
        let live = "https://dl.flathub.org/beta-repo/";
        assert!(remote_url_matches("https://flathub.org/beta-repo/flathub-beta.flatpakrepo", live));
        assert!(remote_url_matches("https://dl.flathub.org/beta-repo", live));
        assert!(!remote_url_matches("https://mirror.example/beta-repo/", live));
    }
}

#[cfg(test)]
mod pacman_key_tests {
    use super::*;

    /// gpg's colon listing of an exported key: the primary key's fingerprint is
    /// the `fpr` after `pub`, never a subkey's.
    #[test]
    fn a_key_file_names_its_primary_fingerprints() {
        let colons = "pub:-:4096:1:AAAA:1:::-:::scESC::::::23::0:\n\
fpr:::::::::0123456789ABCDEF0123456789ABCDEF01234567:\n\
uid:-::::1::HASH::Vendor <v@example.com>::::::::::0:\n\
sub:-:4096:1:BBBB:1::::::e::::::23:\n\
fpr:::::::::FEDCBA9876543210FEDCBA9876543210FEDCBA98:\n";
        assert_eq!(primary_fingerprints(colons), vec!["0123456789ABCDEF0123456789ABCDEF01234567"]);
        assert!(primary_fingerprints("").is_empty());
    }

    /// `pacman-key --lsign-key` leaves a local signature, class `…l`; without
    /// one, packages the key signed are not trusted.
    #[test]
    fn a_key_is_trusted_once_it_is_locally_signed() {
        let unsigned = "pub:-:4096:1:AAAA:1:::-:::scESC:\nsig:::1:AAAA:1::::Vendor:13x:::::8:\n";
        let signed = "pub:-:4096:1:AAAA:1:::-:::scESC:\nsig:::1:CCCC:1::::Pacman Keyring Master Key:10l:::::8:\n";
        assert_eq!(key_state_of(unsigned), KeyState::Unsigned);
        assert_eq!(key_state_of(signed), KeyState::Trusted);
    }
}

#[cfg(test)]
mod brew_dependency_tests {
    use super::*;

    fn set(xs: &[&str]) -> HashSet<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    /// Homebrew's record, as `brew info --json=v2 --installed` prints it (the
    /// shape measured on Homebrew 7.0.11): `installed_on_request` false is a
    /// dependency, by short and full name; true is asked for, with every name a
    /// Brewfile may use; and what needs each formula comes from the install
    /// records (runtime) and the formula (build), and from a cask's
    /// `depends_on`.
    #[test]
    fn homebrews_record_says_what_was_asked_for_and_what_needs_it() {
        let json = br#"{"formulae":[
            {"name":"jq","full_name":"jq","aliases":["jq@1"],"oldnames":["jqold"],
             "build_dependencies":["autoconf"],
             "installed":[{"installed_on_request":true,
               "runtime_dependencies":[{"full_name":"oniguruma"}]}]},
            {"name":"oniguruma","full_name":"oniguruma","installed":[{"installed_on_request":false}]},
            {"name":"sesh","full_name":"joshmedeski/sesh/sesh","installed":[{"installed_on_request":false}]},
            {"name":"amdl","full_name":"me/tap/amdl","installed":[{"installed_on_request":true,
               "runtime_dependencies":[{"full_name":"me/tap/gamdl"}]}]}
        ],"casks":[{"token":"app","depends_on":{"formula":["libfoo"]}}]}"#;
        let r = parse_brew_record(json).unwrap();
        assert_eq!(r.dependencies, set(&["oniguruma", "sesh", "joshmedeski/sesh/sesh"]));
        let jq = r.asked.iter().find(|f| f.name == "jq").unwrap();
        assert_eq!(jq.names, vec!["jq@1", "jqold", "jq"]);
        let by = |n: &str| r.needed_by[n].iter().cloned().collect::<Vec<_>>();
        assert_eq!(by("oniguruma"), vec!["jq"]);
        assert_eq!(by("autoconf"), vec!["jq"]);
        assert_eq!(by("gamdl"), vec!["amdl"]);
        assert_eq!(by("libfoo"), vec!["app"]);
        assert!(parse_brew_record(b"not json").is_none());
    }

    /// What `prune` marks as a dependency: asked for, declared by no name, not
    /// ignored, not going in this prune — and needed by something that stays.
    /// One needed only by a formula this prune removes goes with it instead, and
    /// one nothing needs is left alone: marking it would hand it to autoremove.
    #[test]
    fn a_formula_asked_for_and_needed_is_marked_as_a_dependency() {
        let asked = |name: &str, names: &[&str]| BrewFormula {
            name: name.into(),
            names: names.iter().map(|s| s.to_string()).collect(),
        };
        let mut r = BrewRecord {
            asked: vec![
                asked("ffmpeg", &["ffmpeg"]),
                asked("python@3.14", &["python3", "python@3.14"]),
                asked("lib", &["lib"]),
                asked("quiet", &["quiet"]),
                asked("lonely", &["lonely"]),
                asked("onlyjq", &["onlyjq"]),
            ],
            ..Default::default()
        };
        for (dep, by) in [
            ("ffmpeg", "amdl"),
            ("python@3.14", "amdl"),
            ("lib", "amdl"),
            ("quiet", "amdl"),
            ("onlyjq", "jq"),
        ] {
            r.needed_by.entry(dep.into()).or_default().insert(by.into());
        }
        let raw = vec![(Manager::Brew, "jq".to_string())];
        let declared = set(&["amdl", "python3"]);
        let ignored: HashSet<&str> = ["quiet"].into_iter().collect();
        assert_eq!(
            asked_but_needed(&r, &raw, &declared, &ignored),
            vec![
                ("ffmpeg".to_string(), vec!["amdl".to_string()]),
                ("lib".to_string(), vec!["amdl".to_string()]),
            ]
        );
    }

    #[test]
    fn brews_autoremove_dry_run_names_its_orphans() {
        let out = "==> Would autoremove 2 unneeded formulae:\ntree\nlibedit\n";
        assert_eq!(parse_autoremove_dry_run(out), set(&["tree", "libedit"]));
        assert!(parse_autoremove_dry_run("").is_empty());
    }

    /// The asked-for formula is the extra; its dependency goes with it; an
    /// orphan brew held as unneeded already is neither. Without brew's orphan
    /// list, the fallback is brew's own autoremove.
    #[test]
    fn cleanup_splits_into_extras_and_the_dependencies_they_free() {
        let raw = vec![
            (Manager::Brew, "jq".to_string()),
            (Manager::Brew, "oniguruma".to_string()),
            (Manager::Brew, "tree".to_string()),
            (Manager::Tap, "user/tap".to_string()),
        ];
        let deps = set(&["oniguruma", "tree"]);
        let split = split_brew_extras(raw.clone(), &deps, Some(&set(&["tree"])));
        assert_eq!(
            split.extras,
            vec![(Manager::Brew, "jq".to_string()), (Manager::Tap, "user/tap".to_string())]
        );
        assert_eq!(split.dependencies, vec!["oniguruma".to_string()]);
        assert!(!split.autoremove);

        let fallback = split_brew_extras(raw, &deps, None);
        assert!(fallback.dependencies.is_empty() && fallback.autoremove);
    }
}

#[cfg(test)]
mod converge_dispatch_tests {
    use super::{converge_via, ConvergeVia};
    use crate::packages::Manager;

    /// Every manager brew can clean has a type flag, and the one it cannot is
    /// removed another way.
    ///
    /// Naming any type flag turns the unnamed ones **off**, so a manager missing
    /// from `cleanup_flag` is one `brew bundle cleanup` will not touch —
    /// previewed to the user, confirmed by them, and quietly skipped. That is
    /// precisely the silent cap the explicit flags were introduced to prevent.
    #[test]
    fn every_manager_is_either_cleaned_by_brew_or_removed_another_way() {
        use super::cleanup_flag;
        for &m in Manager::ALL {
            match converge_via(m) {
                // Anything that rides `brew bundle` on the way in must be
                // named on the way out, or cleanup silently spares it.
                ConvergeVia::BrewBundle | ConvergeVia::Mas => assert!(
                    cleanup_flag(m).is_some(),
                    "`{}` is installed through brew but has no cleanup flag",
                    m.as_str()
                ),
                // flatpak is uninstalled explicitly, scoped to the user
                // installation, so handing brew a flag for it would give away a
                // removal temper reports on itself.
                ConvergeVia::Flatpak => assert!(
                    cleanup_flag(m).is_none(),
                    "`{}` is removed explicitly and must not be handed to cleanup",
                    m.as_str()
                ),
            }
        }
    }

    /// Every manager converges by some route, and each route has packages.
    ///
    /// `converge` used to select its three sets with `matches!` on named
    /// variants, so a manager added later fell through all of them and was
    /// simply never installed — the mirror of the probe gap, and just as quiet:
    /// the package stays declared, stays missing, and every converge reports
    /// success. The match makes the choice compulsory; this checks nobody has
    /// quietly routed everything down one branch.
    #[test]
    fn every_manager_has_a_converge_route_and_every_route_is_used() {
        for &m in Manager::ALL {
            // Exhaustive by construction — this is here to fail loudly if the
            // match is ever given a catch-all arm.
            let _ = converge_via(m);
        }
        for route in [ConvergeVia::BrewBundle, ConvergeVia::Flatpak, ConvergeVia::Mas] {
            assert!(
                Manager::ALL.iter().any(|m| converge_via(*m) == route),
                "a converge route no manager takes is dead code"
            );
        }
        // The split that matters: mas must not ride `brew bundle`, or one App
        // Store failure aborts the whole converge.
        assert_eq!(converge_via(Manager::Mas), ConvergeVia::Mas);
        assert_eq!(converge_via(Manager::Flatpak), ConvergeVia::Flatpak);
        assert_eq!(converge_via(Manager::Vscode), ConvergeVia::BrewBundle);
    }
}

#[cfg(test)]
mod rpm_ostree_status_tests {
    use super::requested_from_status;

    /// The layered set comes from ONE deployment — staged if a change is
    /// pending, else booted. Not the union.
    ///
    /// The shape below is this fleet's real `rpm-ostree status --json` on a
    /// Bazzite box that has un-layered a package: the rollback still lists it,
    /// because a rollback keeps the `requested-packages` it was built with.
    /// Unioning made an un-layered package permanently "layered", so `prune`
    /// re-offered it and reported one item removed on every single run, for as
    /// long as the rollback survived.
    #[test]
    fn only_the_pending_deployment_decides_what_is_layered() {
        let staged_differs = br#"{"deployments":[
            {"staged":true,  "requested-packages":["kept"]},
            {"booted":true,  "requested-packages":["kept","dropped"]},
            {"requested-packages":["kept","dropped","ancient"]}
        ]}"#;
        assert_eq!(
            requested_from_status(staged_differs).unwrap(),
            vec!["kept".to_string()],
            "a staged deployment is what the machine is becoming"
        );

        // Nothing staged: the booted deployment is the answer, and the rollback
        // is still ignored.
        let nothing_staged = br#"{"deployments":[
            {"booted":true, "requested-packages":["vpn"]},
            {"requested-packages":["vpn","gone"]}
        ]}"#;
        assert_eq!(
            requested_from_status(nothing_staged).unwrap(),
            vec!["vpn".to_string()]
        );

        // A deployment layering nothing is an answer, not a failure to read.
        let bare = br#"{"deployments":[{"booted":true}]}"#;
        assert_eq!(requested_from_status(bare).unwrap(), Vec::<String>::new());

        // Unreadable output is `None` — never an empty set (Principle #12).
        assert!(requested_from_status(b"not json").is_none());
        assert!(requested_from_status(br#"{"deployments":[]}"#).is_none());
    }
}

/// The layered set named by ONE deployment. Pure, so the deployment-selection
/// rule is testable without an ostree host.
fn requested_from_status(json: &[u8]) -> Option<Vec<String>> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    let deployments = v.get("deployments")?.as_array()?;
    // Exactly ONE deployment describes what this machine will be: the staged one
    // if a change is pending, else the booted one. Unioning all of them folded
    // in the ROLLBACK, which keeps its own `requested-packages` forever — so an
    // un-layered package stayed "layered", `prune` re-offered it every run, and
    // each run reported one item removed for as long as the rollback survived.
    // A rollback is history, not state.
    let flagged = |key: &str| {
        deployments
            .iter()
            .find(|d| d.get(key).and_then(|b| b.as_bool()).unwrap_or(false))
    };
    let current = flagged("staged")
        .or_else(|| flagged("booted"))
        .or_else(|| deployments.first())?;
    let mut pkgs: Vec<String> = current
        .get("requested-packages")
        .and_then(|r| r.as_array())
        .map(|l| {
            l.iter()
                .filter_map(|p| p.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    pkgs.sort();
    pkgs.dedup();
    Some(pkgs)
}

/// Packages in THIS machine's own `rpm_ostree` list that are not layered — the
/// undeclare cell. Machine scope only; a bundle's list is shared.
///
/// Requires the store to be readable, for the reason every drop does: on a host
/// that cannot enumerate, "declared but absent" and "I cannot tell" are the same
/// observation, and only one of them may be acted on.
pub fn rpm_ostree_machine_absent(machine_own: &[String]) -> Vec<String> {
    if machine_own.is_empty() {
        return Vec::new();
    }
    let Some(requested) = rpm_ostree_requested() else {
        return Vec::new();
    };
    let mut out: Vec<String> = machine_own
        .iter()
        .filter(|p| !requested.iter().any(|r| r == *p))
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Un-layer rpms — the prune side of rpm-ostree.
///
/// Symmetric with `rpm_converge`, which is the point: layering and un-layering
/// are the *same* mechanism. Both stage a new deployment and both need a reboot
/// to take effect, so a prune here is no more exotic than the install that put
/// the package there. (This was briefly written off as "a different shape from
/// every other prune" — a comparison against `brew bundle cleanup` rather than
/// against rpm-ostree's own install, which is the one that matters.)
///
/// `--idempotent` so a package already gone is not an error, `-y` because the
/// caller has already confirmed, and deliberately **no** `-r`: temper reports
/// that a reboot is required and never initiates one.
pub fn rpm_ostree_uninstall(pkgs: &[String], verbose: bool) -> Result<bool> {
    if pkgs.is_empty() {
        return Ok(false);
    }
    if !rpm_ostree_caps().converge {
        bail!("rpm-ostree not found — cannot un-layer packages on this host");
    }
    let mut cmd = Command::new("rpm-ostree");
    cmd.args(["uninstall", "--idempotent", "-y"]);
    for p in pkgs {
        cmd.arg(p);
    }
    // The exit status is the answer, not a detail to discard. `run_child` has
    // always returned it; returning `Ok(true)` regardless meant a failed
    // un-layer was counted as items removed and reported "reboot required
    // (rpm-ostree staged a deployment)" for a deployment that was never staged.
    // The layering side was fixed for exactly this and the un-layering side was
    // not — the mirror-half defect PATTERNS warns about.
    Ok(run_child(
        cmd,
        verbose,
        "rpm-ostree uninstall",
        "un-layering rpms",
    ))
}

/// Layered rpms no bundle or machine declares — the extras direction.
///
/// Gated on the machine declaring at least one, like every other manager
/// (SPEC's probe invariant): without it, a box with hand-layered packages and a
/// spec that ignores layering entirely would report all of them.
pub fn rpm_ostree_extras(effective: &[String], ignore: &manifest::Ignore) -> Vec<String> {
    if effective.is_empty() {
        return Vec::new();
    }
    let Some(requested) = rpm_ostree_requested() else {
        return Vec::new();
    };
    let mut out: Vec<String> = requested
        .into_iter()
        .filter(|p| !effective.contains(p) && !ignore.rpm_ostree.contains(p))
        .collect();
    out.sort();
    out
}

/// Declared rpms not installed (`rpm -q`). Empty where rpm isn't present.
pub fn rpm_missing(effective: &[String]) -> Vec<String> {
    if effective.is_empty() {
        return Vec::new();
    }
    // On an atomic host, "requested" is the answer: it covers the staged
    // deployment, so a package layered but not yet rebooted into is NOT
    // missing. `rpm -q` alone reported it missing until reboot, which is a
    // permanent red in the window where the user has already acted.
    if let Some(requested) = rpm_ostree_requested() {
        return effective
            .iter()
            .filter(|p| !requested.iter().any(|r| r == *p))
            .cloned()
            .collect();
    }
    if !have("rpm") {
        return Vec::new();
    }
    effective
        .iter()
        .filter(|p| {
            // `.output()`, not `.status()`: `rpm -q` prints the NVRA to stdout,
            // which `.status()` would inherit and leak ahead of `--json` output.
            !Command::new("rpm")
                .args(["-q", p])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    /// The busiest verb was the quietest one.
    ///
    /// `temper update` runs `brew upgrade`, whose per-formula header is
    /// `==> Upgrading foo` — and the label parser knew only the *install*
    /// vocabulary, so the one line that says what an upgrade is doing right now
    /// never matched. The region sat on "upgrading packages" for the whole run,
    /// which reads exactly like being wedged and leaves nothing to report when a
    /// run really does stall.
    #[test]
    fn the_upgrade_vocabulary_is_recognised_not_only_the_install_one() {
        // Verbatim from Homebrew's `oh1` calls (upgrade.rb / formula_installer.rb).
        assert_eq!(brew_progress_label("==> Upgrading openssl@3").as_deref(), Some("openssl@3"));
        assert_eq!(brew_progress_label("==> Installing openssl@3").as_deref(), Some("openssl@3"));
        // A URL is not a name: the label stays on the package the `Upgrading` line
        // named, which is what should be on screen while its bottle downloads.
        assert_eq!(brew_progress_label("==> Downloading https://x/y.tar.gz"), None);
        assert_eq!(brew_progress_label("==> Pouring aom--3.15.0.bottle.tar.gz").as_deref(), Some("aom"));
        assert_eq!(brew_progress_label("==> Upgrading llvm dependency: xz").as_deref(), Some("xz"));
        // A list header names no single item: its members each get their own line.
        assert_eq!(brew_progress_label("==> Installing dependencies for llvm"), None);
    }

    /// A child with no parser still says where it is.
    ///
    /// `flatpak update` and `rpm-ostree uninstall` have no `==>` vocabulary, so
    /// before the fallback they held their opening message for the whole run —
    /// indistinguishable from wedged, and with nothing to show once the region
    /// freezes.
    #[test]
    fn a_child_with_no_parser_falls_back_to_its_own_last_line() {
        assert_eq!(
            progress_label("Updating app/com.spotify.Client/x86_64/stable").as_deref(),
            Some("Updating app/com.spotify.Client/x86_64/stable")
        );
        // Nothing worth showing: the label must not flicker to empty or to rules.
        assert_eq!(progress_label(""), None);
        assert_eq!(progress_label("   "), None);
        assert_eq!(progress_label("-------"), None);
        // A warning is printed properly by `noteworthy_lines`, not flashed past in
        // a region that is about to be overwritten.
        assert_eq!(progress_label("Warning: something is off"), None);
    }

    /// A long line is elided rather than shoving the region past the terminal.
    #[test]
    fn a_long_fallback_line_is_elided() {
        let long = "Updating ".to_string() + &"x".repeat(200);
        let got = progress_label(&long).expect("a label");
        assert!(got.chars().count() <= 48, "elided to fit, got {} chars", got.chars().count());
    }

    /// gext was the one manager reporting a single direction: declared-but-
    /// missing, never installed-but-undeclared. The extras side is user-scope
    /// only — system extensions ship with the image, and drift reports
    /// image-baked items status-only rather than as something you failed to
    /// declare (this is what keeps a Bazzite box from listing seventeen).
    /// A batch that works runs exactly once; a batch that fails isolates.
    ///
    /// Both halves matter. Batching is why a converge asks for one password
    /// instead of one per app. Isolation is why a single bad entry does not
    /// strand the rest, and why the run can name which entry it was — a batch
    /// failure alone says nothing about that (Principle #6).
    #[test]
    fn a_batch_runs_once_and_isolates_only_on_failure() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let items: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();

        // Happy path: one invocation, everything lands.
        let calls = AtomicUsize::new(0);
        let landed = batch_then_isolate(&items, "t", false, |batch| {
            calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(batch.len(), 3, "the happy path must not split the batch");
            let mut c = Command::new("true");
            c.arg(batch.len().to_string());
            c
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(landed, items);

        // Failure path: the batch is retried per item, and only the ones that
        // succeed are returned — so a failed install never gets journaled.
        let landed = batch_then_isolate(&items, "t", false, |batch| {
            // Fails for the whole batch and for "b" alone; succeeds otherwise.
            let fail = batch.len() > 1 || batch[0] == "b";
            Command::new(if fail { "false" } else { "true" })
        });
        assert_eq!(landed, vec!["a".to_string(), "c".to_string()]);

        assert!(batch_then_isolate(&[], "t", false, |_| Command::new("true")).is_empty());
    }

    #[test]
    fn declared_minus_installed_is_sorted_and_deduped() {
        let installed = vec!["here@x".to_string()];
        let declared = vec![
            "gone@x".to_string(),
            "here@x".to_string(),
            "also@x".to_string(),
            "gone@x".to_string(),
        ];
        assert_eq!(
            gext_absent_from(&installed, &declared),
            vec!["also@x".to_string(), "gone@x".to_string()]
        );
        // Nothing declared for this machine → nothing to drop, whatever is on
        // the host. The machine's OWN list is the candidate set, which is also
        // why `init` (a machine block it just created empty) cannot drop.
        assert!(gext_absent_from(&installed, &[]).is_empty());
        assert!(gext_machine_absent(&[]).is_empty());
    }

    #[test]
    fn gext_extras_are_user_scope_minus_declared_minus_ignored() {
        let installed_user = vec![
            "declared@x".to_string(),
            "ignored@x".to_string(),
            "stray@x".to_string(),
        ];
        let declared = vec!["declared@x".to_string()];
        let ignored = vec!["ignored@x".to_string()];
        assert_eq!(
            gext_extras_from(&installed_user, &declared, &ignored),
            vec!["stray@x".to_string()]
        );
        // Nothing installed in the user scope → nothing to report, even with a
        // machine that declares extensions it hasn't installed yet.
        assert!(gext_extras_from(&[], &declared, &[]).is_empty());
        // A machine that declares no extensions opts out entirely (the probe
        // invariant): without this, any spec that ignores extensions would
        // report every hand-installed one on the host, and drift would depend on
        // the machine instead of the spec.
        assert!(gext_extras(&[], &crate::manifest::Ignore::default()).is_empty());
    }

    /// SPEC.md states, as a documented invariant, that a manager is only probed
    /// if at least one of its packages is declared — that is why a VS Code
    /// Settings Sync setup needs no opt-out setting: with nothing declared,
    /// temper never runs `code --list-extensions` and never reports an
    /// extension as an extra. If a refactor ever probes unconditionally, the
    /// doc silently becomes a lie, so pin it here.
    #[test]
    fn an_undeclared_manager_is_never_probed() {
        let declared = vec![crate::packages::parse("brew \"jq\"").unwrap()];
        let inst = probe(&declared).unwrap();
        for m in [Manager::Vscode, Manager::Flatpak, Manager::Mas] {
            assert!(
                !inst.probed(m),
                "{} was probed despite nothing being declared for it",
                m.as_str()
            );
        }
        // …and an unprobed manager can therefore never yield an extra.
        let extras = crate::packages::extras(&declared, &inst, &crate::manifest::Ignore::default());
        assert!(
            !extras.iter().any(|(m, _)| *m == Manager::Vscode),
            "vscode extra reported without a declaration: {extras:?}"
        );
    }

    #[test]
    fn labels_the_package_being_installed() {
        // Real `brew install` ohai lines — the spinner's whole input.
        assert_eq!(brew_progress_label("==> Fetching llvm").as_deref(), Some("llvm"));
        assert_eq!(
            brew_progress_label("==> Installing Cask zoom").as_deref(),
            Some("zoom")
        );
        assert_eq!(
            brew_progress_label("==> Pouring llvm--22.1.8.arm64_tahoe.bottle.tar.gz").as_deref(),
            Some("llvm")
        );
        // A dependency being built names the DEP, not its parent — that's what's
        // actually taking the time.
        assert_eq!(
            brew_progress_label("==> Installing llvm dependency: xz").as_deref(),
            Some("xz")
        );
        // The pkg-installer line (the one that used to mean a password prompt).
        assert_eq!(
            brew_progress_label("==> Running installer for mactex; your password may be necessary.")
                .as_deref(),
            Some("mactex")
        );
    }

    #[test]
    fn list_lines_and_plain_output_label_nothing() {
        // Lists: each member arrives again on its own line, so showing the list
        // would flash a name the user can't read and isn't being installed yet.
        assert_eq!(brew_progress_label("==> Installing dependencies for llvm: xz, zstd"), None);
        assert_eq!(brew_progress_label("==> Fetching downloads for: llvm, xz"), None);
        // Not an ohai line at all.
        assert_eq!(brew_progress_label("Using wget"), None);
        assert_eq!(brew_progress_label(""), None);
        assert_eq!(brew_progress_label("🍺  /opt/homebrew/Cellar/llvm/22.1.8: 8,000 files"), None);
    }

    #[test]
    fn capture_keeps_everything_for_the_failure_replay() {
        // The exec-step contract: quiet on success, the full story on failure —
        // so a failing converge must hand back every line, both streams.
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            "echo '==> Installing Cask zoom'; echo 'Error: nope' 1>&2; exit 3",
        ]);
        let (ok, log) = run_with_spinner(cmd, "test child", "testing").unwrap();
        assert!(!ok, "non-zero exit must report failure");
        assert!(log.contains("Installing Cask zoom"), "stdout lost: {log:?}");
        assert!(log.contains("Error: nope"), "stderr lost: {log:?}");
    }

    #[test]
    fn capture_reports_success_and_drains_both_streams() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo out; echo err 1>&2"]);
        let (ok, log) = run_with_spinner(cmd, "test child", "testing").unwrap();
        assert!(ok);
        assert!(log.contains("out") && log.contains("err"), "{log:?}");
    }

    #[test]
    fn warnings_survive_a_successful_capture() {
        // Quiet-on-success must not swallow advisories the streamed path showed.
        assert!(is_noteworthy("Warning: wget 1.25.0 is already installed"));
        assert!(is_noteworthy("  Error: cask 'x' is unavailable"));
        // flatpak lower-cases its prefixes — an EOL-runtime advisory must survive
        // the quiet path exactly like Homebrew's.
        assert!(is_noteworthy("error: Failed to install org.x.App"));
        assert!(is_noteworthy("warning: org.gnome.Platform is end-of-life"));
        assert!(!is_noteworthy("==> Fetching llvm"));
        assert!(!is_noteworthy("Using wget"));
        assert!(!is_noteworthy("Nothing to update."));
    }

    #[test]
    fn upgrade_count_is_a_version_diff_not_a_head_count() {
        let snap = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let before = snap(&[("brew:node", "26.6.0"), ("brew:jq", "1.7"), ("flatpak:org.x.A", "1.0")]);
        let after = snap(&[
            ("brew:node", "26.7.0"), // upgraded
            ("brew:jq", "1.7"),      // untouched
            ("flatpak:org.x.A", "1.0"),
            ("brew:libnew", "1.0"), // a dependency pulled in — not an upgrade
        ]);
        assert_eq!(upgraded_between(&before, &after), 1);
        // Nothing moved → nothing claimed.
        assert_eq!(upgraded_between(&before, &before), 0);
        // A failed upgrade leaves versions where they were.
        assert_eq!(upgraded_between(&after, &after), 0);
    }

    #[test]
    fn captured_children_get_a_closed_stdin() {
        // Inherited stdin is an invisible hang: the child blocks on the tty while
        // its prompt vanishes into the pipe. Closed stdin makes it fail fast.
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "if read line; then echo \"got: $line\"; else echo eof; fi"]);
        let (ok, log) = run_with_spinner(cmd, "test child", "testing").unwrap();
        assert!(ok);
        assert!(log.contains("eof"), "stdin was not closed: {log:?}");
    }

    #[test]
    fn a_warning_keeps_its_body() {
        // Verbatim from a real `brew bundle` run on this fleet: the remedy is in
        // the body, so surfacing only the "Warning:" line is worse than useless.
        let log = "\
==> Fetching jq
Warning: Formulae dependency graph sorting found a circular dependency:
  libtiff, webp
If it persists, run the following commands and try again:
  brew update

==> Installing Cask zoom
Using wget
";
        assert_eq!(
            noteworthy_lines(log),
            vec![
                "Warning: Formulae dependency graph sorting found a circular dependency:",
                "  libtiff, webp",
                "If it persists, run the following commands and try again:",
                "  brew update",
            ]
        );
    }

    #[test]
    fn a_progress_line_ends_a_warning_block() {
        // No blank line to close the block — the next `==>` must, or every
        // remaining line of the converge would be reported as part of the warning.
        let log = "Warning: something\n  detail\n==> Installing Cask zoom\n🍺 done\n";
        assert_eq!(noteworthy_lines(log), vec!["Warning: something", "  detail"]);
    }
}

#[cfg(test)]
mod mas_tests {
    use super::*;

    #[test]
    fn parse_mas_rows() {
        assert_eq!(
            parse_mas_line("497799835  Xcode (14.0)"),
            Some(("497799835".into(), "Xcode".into()))
        );
        // multi-word name, version stripped
        assert_eq!(
            parse_mas_line("1234  The Unarchiver (4.3.9)"),
            Some(("1234".into(), "The Unarchiver".into()))
        );
        // no trailing version
        assert_eq!(
            parse_mas_line("55  Bear"),
            Some(("55".into(), "Bear".into()))
        );
        assert_eq!(parse_mas_line(""), None);
        assert_eq!(parse_mas_line("justoneword"), None);
    }
}

#[cfg(test)]
mod gating_tests {
    use super::*;
    use crate::manifest::Machine;

    fn machine(name: &str, os: &str, role: &str, apps: &[&str]) -> Machine {
        Machine {
            name: name.into(),
            os: os.into(),
            role: Some(role.into()),
            apps: apps.iter().map(|s| s.to_string()).collect(),
            packages: vec![],
            gnome_extensions: Vec::new(),
            brewfile: None,
            vars: Default::default(),
            brew_trust: Vec::new(),
            rpm_ostree: Vec::new(),
            dnf: Vec::new(),
            apt: Vec::new(),
            pacman: Vec::new(),
            flatpak_remotes: Vec::new(),
            rpm_repos: Vec::new(),
            apt_sources: Vec::new(),
            pacman_repos: Vec::new(),
            retire: Vec::new(),
            retire_packages: Vec::new(),
            ignore: Default::default(),
            dconf: vec![],
            git: None,
        }
    }

    #[test]
    fn desktop_bundle_gated_off_for_server() {
        let home = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(home.path().join("apps")).unwrap();
        fs::write(
            home.path().join("apps/gnome.toml"),
            "os = \"linux\"\nrole = \"desktop\"\nextensions = [\"a@x\", \"b@y\"]\nrpm = [\"proton-vpn\"]\n",
        )
        .unwrap();

        // Desktop composes it → extensions + rpm are aggregated.
        let desktop = machine("d", "linux", "desktop", &["gnome"]);
        assert_eq!(
            effective_extensions(home.path(), &desktop).unwrap(),
            vec!["a@x", "b@y"]
        );
        assert_eq!(
            effective_rpm(home.path(), &desktop).unwrap(),
            vec!["proton-vpn"]
        );

        // Server composes the SAME bundle → gated off (empty), the ROADMAP footgun.
        let server = machine("s", "linux", "server", &["gnome"]);
        assert!(effective_extensions(home.path(), &server)
            .unwrap()
            .is_empty());
        assert!(effective_rpm(home.path(), &server).unwrap().is_empty());
    }
}

/// Layer missing rpms via `rpm-ostree install --idempotent`. Returns whether a
/// reboot is needed. VM-verified.
///
/// Captured like every other converge child (see `run_child`) — rpm-ostree is
/// chatty and slow, so it gets the spinner rather than the terminal.
pub fn rpm_converge(effective: &[String], dry_run: bool, verbose: bool) -> Result<Vec<String>> {
    let missing = rpm_missing(effective);
    if dry_run || missing.is_empty() {
        return Ok(Vec::new());
    }
    if !rpm_ostree_caps().converge {
        eprintln!(
            "{} rpm-ostree not found — {} declared rpm(s) cannot be layered on this host",
            crate::ui::yellow(crate::ui::g_warn()),
            missing.len()
        );
        return Ok(Vec::new());
    }
    // `batch_then_isolate` so a single bad package name does not strand the
    // rest, and — the part that was missing — so the return value is what
    // actually landed. The exit status used to be discarded: a failed layering
    // was journaled as installed (undo would then try to un-layer packages that
    // were never there) and reported "reboot required (rpm-ostree layered a
    // package)" for a deployment that was never staged.
    let landed = batch_then_isolate(&missing, "rpm-ostree install", verbose, |items| {
        let mut cmd = Command::new("rpm-ostree");
        cmd.args(["install", "--idempotent"]);
        for p in items {
            cmd.arg(p);
        }
        cmd
    });
    // A non-empty set is also the reboot signal: layering stages a deployment.
    Ok(landed)
}

// --- dependency-aware brew extras (read-only) ---------------------------------

/// Formulae/casks/taps installed but not needed by the declared set, per
/// `brew bundle cleanup` (no `--force`, so read-only). Dependency-aware: a kept
/// package's transitive deps are NOT reported — unlike a naive set-diff. Each
/// extra is tagged with its manager: a `tap` orphan keeps its full `user/repo`
/// name (it round-trips as a `tap` line); a formula/cask keeps brew's short name
/// and is tagged `Brew` for the caller to reclassify/qualify. `[ignore]` applied.
///
/// A formula asked for and still needed by a declared one comes last: it is an
/// extra too, and `reconcile` declares it like any other.
pub fn brew_extras(effective: &[Pkg], ignore: &manifest::Ignore) -> Result<Vec<(Manager, String)>> {
    let split = brew_extras_split(effective, ignore, false)?;
    let mut out = split.extras;
    out.extend(split.demote.into_iter().map(|(n, _)| (Manager::Brew, n)));
    Ok(out)
}

/// What `brew bundle cleanup` would remove, split the way Homebrew's own
/// records split it.
///
/// `cleanup` lists every installed formula the Brewfile does not account for,
/// dependencies included: with `jq` installed by hand it lists `oniguruma`
/// beside it. Reported as extras, a dependency was offered to `reconcile`,
/// seeded by `init` and listed by `prune` as though someone had asked for it.
/// Homebrew records the difference (`installed_on_request`), so a formula held
/// only as a dependency is never an extra: it belongs to whatever needed it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BrewExtras {
    /// Formulae installed on request, casks and taps — what drift reports,
    /// reconcile offers and prune removes by name.
    pub extras: Vec<(Manager, String)>,
    /// Formulae held as dependencies that removing `extras` leaves unneeded —
    /// not the ones `brew autoremove` would take already.
    pub dependencies: Vec<String>,
    /// Brew could not list what it holds as unneeded, so the dependencies of
    /// the extras cannot be told apart from older orphans. Removing the extras
    /// then takes every unneeded formula — brew's own autoremove, said so in
    /// the preview.
    pub autoremove: bool,
    /// Formulae installed on request that the Brewfile does not declare, and
    /// that a formula or cask staying here needs — each with what needs it.
    /// `brew bundle cleanup` keeps them, so they are not in `extras`; Homebrew
    /// still holds them as asked for, so its autoremove never takes them. `prune`
    /// marks them as dependencies: they stay, and leave with what needs them.
    pub demote: Vec<(String, Vec<String>)>,
    /// Every name `brew bundle cleanup` would remove, short and full — `None`
    /// when it was not asked. A formula or cask it does not list is one Homebrew
    /// keeps, because something declared needs it, so `prune` cannot remove it.
    pub listed: Option<HashSet<String>>,
}

/// `brew_extras`, with the dependencies `prune` removes alongside and the
/// formulae it marks as dependencies.
pub fn brew_extras_split(
    effective: &[Pkg],
    ignore: &manifest::Ignore,
    seed: bool,
) -> Result<BrewExtras> {
    let Some(raw) = brew_cleanup_list(effective, ignore, seed)? else {
        return Ok(BrewExtras::default());
    };
    let listed: HashSet<String> = raw
        .iter()
        .flat_map(|(_, n)| [n.clone(), n.rsplit('/').next().unwrap_or(n).to_string()])
        .collect();
    // Without Homebrew's record, a dependency cannot be told from a formula
    // someone asked for, and every one reads as an extra, as it always has.
    let Some(record) = brew_record() else {
        return Ok(BrewExtras {
            extras: raw,
            listed: Some(listed),
            ..Default::default()
        });
    };
    let mut split = if raw.is_empty() {
        BrewExtras::default()
    } else {
        split_brew_extras(raw.clone(), &record.dependencies, brew_orphans().as_ref())
    };
    let ignored: HashSet<&str> = ignore.brew.iter().map(String::as_str).collect();
    split.demote = asked_but_needed(&record, &raw, &declared_formulae(effective), &ignored);
    split.listed = Some(listed);
    Ok(split)
}

/// Every name the effective set declares a formula by: as written, and short.
fn declared_formulae(effective: &[Pkg]) -> HashSet<String> {
    effective
        .iter()
        .filter(|p| p.manager == Manager::Brew)
        .flat_map(|p| [p.name.clone(), p.match_name()])
        .collect()
}

/// Formulae Homebrew holds as asked for that nothing declares, and that
/// something staying needs: not in the cleanup list (`raw`), so a declared
/// formula or cask accounts for them, and named as a dependency in Homebrew's
/// own records — the same records its autoremove reads, so marking one as a
/// dependency never makes it an orphan. Pure, so it is testable.
fn asked_but_needed(
    record: &BrewRecord,
    raw: &[(Manager, String)],
    declared: &HashSet<String>,
    ignored: &HashSet<&str>,
) -> Vec<(String, Vec<String>)> {
    // Cleanup names a tap's formula in full; the records name what needs it short.
    let going: HashSet<&str> = raw
        .iter()
        .flat_map(|(_, n)| [n.as_str(), n.rsplit('/').next().unwrap_or(n)])
        .collect();
    let mut out = Vec::new();
    for f in &record.asked {
        let names = || std::iter::once(&f.name).chain(&f.names);
        if names().any(|n| going.contains(n.as_str()) || declared.contains(n) || ignored.contains(n.as_str())) {
            continue;
        }
        let needed_by: Vec<String> = record
            .needed_by
            .get(&f.name)
            .into_iter()
            .flatten()
            .filter(|d| !going.contains(d.as_str()))
            .cloned()
            .collect();
        if !needed_by.is_empty() {
            out.push((f.name.clone(), needed_by));
        }
    }
    out
}

/// The split itself, apart from brew, so it is testable.
fn split_brew_extras(
    raw: Vec<(Manager, String)>,
    deps: &HashSet<String>,
    orphans: Option<&HashSet<String>>,
) -> BrewExtras {
    let (candidates, extras): (Vec<_>, Vec<_>) = raw
        .into_iter()
        .partition(|(m, n)| *m == Manager::Brew && deps.contains(n));
    let candidates: Vec<String> = candidates.into_iter().map(|(_, n)| n).collect();
    match orphans {
        Some(o) => BrewExtras {
            extras,
            dependencies: candidates.into_iter().filter(|n| !o.contains(n)).collect(),
            ..Default::default()
        },
        None => BrewExtras {
            extras,
            autoremove: true,
            ..Default::default()
        },
    }
}

/// What Homebrew records about the formulae it has installed: whether each was
/// asked for, and what needs it.
#[derive(Debug, Default)]
struct BrewRecord {
    /// Held as somebody's dependency — not installed on request — by name and
    /// full name.
    dependencies: HashSet<String>,
    /// Installed on request.
    asked: Vec<BrewFormula>,
    /// Formula (short name) → the installed formulae and casks that need it, to
    /// run or to build. Read from the install records, as `brew autoremove`
    /// reads them.
    needed_by: HashMap<String, BTreeSet<String>>,
}

#[derive(Debug)]
struct BrewFormula {
    name: String,
    /// Its full name, aliases and old names — whatever a Brewfile line may call it.
    names: Vec<String>,
}

/// `None` when Homebrew cannot be asked.
fn brew_record() -> Option<BrewRecord> {
    let out = Command::new("brew")
        .args(["info", "--json=v2", "--installed"])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_brew_record(&out.stdout)
}

fn parse_brew_record(json: &[u8]) -> Option<BrewRecord> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    let short = |n: &str| n.rsplit('/').next().unwrap_or(n).to_string();
    let strings = |v: Option<&serde_json::Value>| -> Vec<String> {
        v.and_then(|a| a.as_array())
            .map(|a| a.iter().filter_map(|s| s.as_str()).map(str::to_string).collect())
            .unwrap_or_default()
    };
    let mut record = BrewRecord::default();
    for f in v.get("formulae")?.as_array()? {
        let Some(name) = f.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let install = f
            .get("installed")
            .and_then(|i| i.as_array())
            .and_then(|i| i.first());
        let mut needs: Vec<String> = install
            .and_then(|r| r.get("runtime_dependencies"))
            .and_then(|d| d.as_array())
            .into_iter()
            .flatten()
            .filter_map(|d| d.get("full_name").and_then(|n| n.as_str()))
            .map(short)
            .collect();
        needs.extend(strings(f.get("build_dependencies")).iter().map(|n| short(n)));
        for dep in needs {
            record.needed_by.entry(dep).or_default().insert(name.to_string());
        }
        let mut names = strings(f.get("aliases"));
        names.extend(strings(f.get("oldnames")));
        if let Some(full) = f.get("full_name").and_then(|n| n.as_str()) {
            names.push(full.to_string());
        }
        match install
            .and_then(|r| r.get("installed_on_request"))
            .and_then(|b| b.as_bool())
        {
            Some(false) => {
                record.dependencies.insert(name.to_string());
                record.dependencies.extend(f.get("full_name").and_then(|n| n.as_str()).map(str::to_string));
            }
            Some(true) => record.asked.push(BrewFormula { name: name.to_string(), names }),
            None => {}
        }
    }
    for c in v.get("casks").and_then(|c| c.as_array()).into_iter().flatten() {
        let Some(token) = c.get("token").and_then(|t| t.as_str()) else {
            continue;
        };
        for dep in strings(c.get("depends_on").and_then(|d| d.get("formula"))) {
            record.needed_by.entry(short(&dep)).or_default().insert(token.to_string());
        }
    }
    Some(record)
}

/// Mark formulae as dependencies (`brew tab --no-installed-on-request`): they
/// stay installed, and Homebrew's autoremove takes them once nothing needs
/// them. Returns the ones its records then hold as dependencies.
pub fn brew_mark_dependencies(names: &[String]) -> Vec<String> {
    if names.is_empty() {
        return Vec::new();
    }
    let _ = Command::new("brew")
        .args(["tab", "--no-installed-on-request", "--formula"])
        .args(names)
        .stdin(Stdio::null())
        .output();
    let held = brew_record().map(|r| r.dependencies).unwrap_or_default();
    names.iter().filter(|n| held.contains(*n)).cloned().collect()
}

/// The formulae `brew autoremove` would remove now: held as dependencies, and
/// needed by nothing. `None` when brew could not say.
pub fn brew_orphans() -> Option<HashSet<String>> {
    if !have("brew") {
        return None;
    }
    let out = Command::new("brew")
        .args(["autoremove", "--dry-run"])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_autoremove_dry_run(&String::from_utf8_lossy(&out.stdout)))
}

/// Names under brew's "==> Would autoremove N unneeded formula(e):" heading.
fn parse_autoremove_dry_run(text: &str) -> HashSet<String> {
    let mut on = false;
    let mut out = HashSet::new();
    for line in text.lines() {
        if line.starts_with("==>") {
            on = line.contains("Would autoremove");
            continue;
        }
        let name = line.trim();
        if on && !name.is_empty() && !name.contains(' ') {
            out.insert(name.to_string());
        }
    }
    out
}

/// `brew uninstall` of what a run installed, scoped like a prune.
///
/// Homebrew runs a whole-system autoremove after every uninstall: reverting
/// `jq` took `oniguruma` with it, which is right, and an unrelated orphan too,
/// which no undo had shown. So the autoremove is switched off, the named
/// packages go, and then only the formulae that became unneeded because they
/// went — brew's own list after, less its list before. If brew cannot list
/// them, its autoremove runs as it would have. Returns whether the named
/// packages were uninstalled.
pub fn brew_uninstall(kind: &str, names: &[String]) -> bool {
    if names.is_empty() {
        return true;
    }
    let before = brew_orphans();
    let mut cmd = Command::new("brew");
    cmd.args(["uninstall", kind]).args(names);
    if before.is_some() {
        cmd.env("HOMEBREW_NO_AUTOREMOVE", "1");
    }
    let ok = cmd
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let Some(before) = before else {
        return ok;
    };
    match brew_orphans() {
        Some(after) => {
            let freed: Vec<&String> = after.difference(&before).collect();
            if !freed.is_empty() {
                let _ = Command::new("brew")
                    .args(["uninstall", "--formula"])
                    .args(&freed)
                    .env("HOMEBREW_NO_AUTOREMOVE", "1")
                    .stdout(Stdio::null())
                    .status();
            }
        }
        None => {
            let _ = Command::new("brew")
                .arg("autoremove")
                .stdout(Stdio::null())
                .status();
        }
    }
    ok
}

/// `brew_extras` for the SEED case, where the empty declared set is the point
/// rather than a reason to stay quiet.
///
/// The probe invariant ("a manager is only probed if you declare at least one of
/// its packages") is right for drift: it is what stops a spec that declares no
/// packages from reporting the whole machine as extras. It is exactly wrong for
/// `init`, whose entire job is to discover what is here — so `init` seeded
/// **nothing**, on a host with 200 formulae, while the docs promised "full
/// manager coverage".
pub fn brew_extras_seeding(ignore: &manifest::Ignore) -> Result<Vec<(Manager, String)>> {
    Ok(brew_extras_split(&[], ignore, true)?.extras)
}

/// What `brew bundle cleanup` lists for the declared set: `None` when it was
/// not asked — no brew, nothing brew-managed declared — or could not answer,
/// which is not the same as an empty list.
fn brew_cleanup_list(
    effective: &[Pkg],
    ignore: &manifest::Ignore,
    seed: bool,
) -> Result<Option<Vec<(Manager, String)>>> {
    if !have("brew") {
        return Ok(None);
    }
    let body: String = effective
        .iter()
        .filter(|p| {
            matches!(
                p.manager,
                Manager::Brew | Manager::Cask | Manager::Tap | Manager::Mas | Manager::Vscode
            )
        })
        .map(|p| format!("{}\n", p.raw))
        .collect();
    if body.is_empty() && !seed {
        return Ok(None);
    }
    let tmp = std::env::temp_dir().join(format!("temper-Brewfile-drift-{}", std::process::id()));
    fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    let out = Command::new("brew")
        .args([
            "bundle",
            "cleanup",
            "--formula",
            "--cask",
            "--tap",
            "--file",
        ])
        .arg(&tmp)
        .output()
        .context("running brew bundle cleanup")?;
    let _ = fs::remove_file(&tmp);

    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    // `cleanup` without `--force` exits **non-zero when it finds orphans** —
    // that is its normal result, the way `diff` and `grep` report a match. So
    // the exit code alone cannot say whether it worked: what distinguishes a
    // real failure is that it named nothing.
    //
    //   exit 0                      → ran, nothing to clean
    //   exit non-zero + a section   → ran, and these are the extras
    //   exit non-zero + no section  → could not run
    //
    // Reading a non-zero exit as failure reported **zero extras on every machine
    // that had any**, which looks exactly like a clean one.
    let named_something = text.contains("Would uninstall") || text.contains("Would untap");
    if !out.status.success() && !named_something {
        eprintln!(
            "{} `brew bundle cleanup` could not run — brew extras not computed \
             this run",
            crate::ui::yellow(crate::ui::g_warn())
        );
        return Ok(None);
    }
    let ignored: HashSet<&str> = ignore
        .brew
        .iter()
        .chain(&ignore.cask)
        .chain(&ignore.tap)
        .map(String::as_str)
        .collect();
    Ok(Some(parse_cleanup_extras(&text, &ignored)))
}

/// Parse `brew bundle cleanup`'s "Would uninstall …" (formulae/casks) and
/// "Would untap …" (taps) sections into manager-tagged extras. Pure → unit-tested.
///
/// The two sections need DIFFERENT name handling, and conflating them is the
/// migrated-to-core tap loop: a formula line may carry a tap prefix
/// (`user/tap/name`) that we strip to brew's short name, but a TAP line is itself
/// `user/repo` and must be kept WHOLE. Stripping a tap to its last path segment
/// (`joshmedeski/sesh` → `sesh`) mints a bogus formula name that collides with a
/// real formula, is written as a bare `brew "sesh"` that `cleanup` can't match to
/// the tap, and so is re-offered every run. So we track which section owns a line.
fn parse_cleanup_extras(text: &str, ignored: &HashSet<&str>) -> Vec<(Manager, String)> {
    #[derive(Clone, Copy, PartialEq)]
    enum Sec {
        None,
        Uninstall,
        Untap,
    }
    let mut sec = Sec::None;
    let mut extras = Vec::new();
    for line in text.lines() {
        if line.starts_with("Would uninstall") {
            sec = Sec::Uninstall;
            continue;
        }
        if line.starts_with("Would untap") {
            sec = Sec::Untap;
            continue;
        }
        if sec == Sec::None {
            continue;
        }
        let first = line.chars().next();
        let is_name = matches!(first, Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
            && line.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '/' | '-')
            });
        if is_name {
            if sec == Sec::Untap {
                // A tap `user/repo`, kept whole and matched against `[ignore].tap`.
                if !ignored.contains(line) {
                    extras.push((Manager::Tap, line.to_string()));
                }
            } else {
                // A formula/cask: strip any tap prefix to brew's short name. Tagged
                // `Brew`; the caller reclassifies cask-vs-formula and qualifies it.
                let name = line.rsplit('/').next().unwrap_or(line);
                if !ignored.contains(name) && !ignored.contains(line) {
                    extras.push((Manager::Brew, name.to_string()));
                }
            }
        } else if matches!(first, Some(c) if c.is_ascii_uppercase()) {
            sec = Sec::None;
        }
    }
    extras
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;

    fn ignored<'a>(items: &[&'a str]) -> HashSet<&'a str> {
        items.iter().copied().collect()
    }

    #[test]
    fn untap_line_kept_whole_not_split_to_a_formula() {
        // Regression: when a formula migrates into homebrew-core its old tap is
        // orphaned, and `brew bundle cleanup` reports it under "Would untap" as
        // `user/repo`. Splitting that to the last segment (`sesh`) minted a bogus
        // `brew "sesh"` add that cleanup could never match — an infinite reconcile
        // loop. A tap must be kept whole and tagged as a tap.
        let text = "Would untap:\njoshmedeski/sesh\n";
        assert_eq!(
            parse_cleanup_extras(text, &ignored(&[])),
            vec![(Manager::Tap, "joshmedeski/sesh".to_string())]
        );
    }

    #[test]
    fn uninstall_formula_stripped_to_short_name() {
        // Under "Would uninstall", a tap-qualified formula IS stripped to brew's
        // short name (the caller re-qualifies it) — the opposite of a tap line.
        let text = "Would uninstall these formulae:\nowner/tap/foo\nbar\n";
        assert_eq!(
            parse_cleanup_extras(text, &ignored(&[])),
            vec![
                (Manager::Brew, "foo".to_string()),
                (Manager::Brew, "bar".to_string()),
            ]
        );
    }

    #[test]
    fn sections_classified_independently() {
        let text = "Would uninstall these formulae:\nripgrep\nWould untap:\njoshmedeski/sesh\n";
        assert_eq!(
            parse_cleanup_extras(text, &ignored(&[])),
            vec![
                (Manager::Brew, "ripgrep".to_string()),
                (Manager::Tap, "joshmedeski/sesh".to_string()),
            ]
        );
    }

    #[test]
    fn ignore_suppresses_tap_by_full_name_and_formula_by_short() {
        let text = "Would untap:\njoshmedeski/sesh\nWould uninstall these formulae:\nbar\n";
        assert!(parse_cleanup_extras(text, &ignored(&["joshmedeski/sesh", "bar"])).is_empty());
    }

    #[test]
    fn uppercase_summary_line_ends_a_section() {
        // A trailing summary ("Untapped …") starts uppercase and must not be read
        // as a name.
        let text = "Would untap:\njoshmedeski/sesh\nUntapped 1 tap\n";
        assert_eq!(
            parse_cleanup_extras(text, &ignored(&[])),
            vec![(Manager::Tap, "joshmedeski/sesh".to_string())]
        );
    }
}

#[cfg(test)]
mod declared_trust_tests {
    use super::with_declared_trust;

    /// A tap the machine's Brewfile already lists gains the option rather than
    /// gaining a second line — brew would otherwise see the tap twice.
    #[test]
    fn an_existing_tap_line_gains_the_option() {
        let out = with_declared_trust(
            "tap \"vendor/tap\"\nbrew \"jq\"\n",
            &["vendor/tap".to_string()],
        );
        assert_eq!(out, "tap \"vendor/tap\", trusted: true\nbrew \"jq\"\n");
    }

    /// The case that actually bit: trust declared in `[brew].trust`, and the
    /// machine's Brewfile lists no taps at all. Without a line here, `cleanup
    /// --force` calls `Trust.replace!` with nothing and empties the store.
    #[test]
    fn a_tap_declared_only_as_trust_gets_a_line() {
        let out = with_declared_trust("brew \"jq\"\n", &["vendor/tap".to_string()]);
        assert!(
            out.contains("tap \"vendor/tap\", trusted: true"),
            "a tap declared only via [brew].trust must still reach the file: {out}"
        );
        assert!(out.contains("brew \"jq\""), "the rest of the body survives: {out}");
    }

    /// Nothing declared means nothing added — this must not invent trust.
    #[test]
    fn no_declared_trust_changes_nothing() {
        let body = "brew \"jq\"\ncask \"ghostty\"\n";
        assert_eq!(with_declared_trust(body, &[]), body);
    }

    /// A tap being untrusted by this very run is trusted-but-*undeclared*, so it
    /// is absent from the declared list and must not be re-added — otherwise
    /// prune would hand back the trust it was asked to remove.
    #[test]
    fn a_tap_being_untrusted_is_not_reinstated() {
        let out = with_declared_trust(
            "tap \"keep/tap\"\nbrew \"jq\"\n",
            &["keep/tap".to_string()],
        );
        assert!(!out.contains("drop/tap"), "only declared taps appear: {out}");
        assert!(out.contains("tap \"keep/tap\", trusted: true"));
    }

    /// Idempotent: a line that already carries the option is left alone rather
    /// than growing a second one.
    #[test]
    fn an_already_trusted_line_is_not_doubled() {
        let body = "tap \"vendor/tap\", trusted: true\n";
        assert_eq!(with_declared_trust(body, &["vendor/tap".to_string()]), body);
    }
}
