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

use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, Read};
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
fn batch_then_isolate<F>(items: &[String], label: &str, build: F) -> Vec<String>
where
    F: Fn(&[String]) -> Command,
{
    if items.is_empty() {
        return Vec::new();
    }
    let ok = |mut c: Command| {
        c.stdin(Stdio::null())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if ok(build(items)) {
        return items.to_vec();
    }
    // The batch failed: find out which ones, rather than reporting the whole set
    // as lost or the whole set as fine.
    let mut landed = Vec::new();
    for item in items {
        if ok(build(std::slice::from_ref(item))) {
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
    for prefix in ["Installing Cask ", "Installing ", "Fetching "] {
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

    // stderr is drained on its own thread: a full pipe buffer on either stream
    // would deadlock the child while we block reading the other.
    let errs = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut buf);
        buf
    });

    let pb = crate::ui::spinner(initial);
    let mut log = String::new();
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Some(label) = brew_progress_label(&line) {
            pb.set_message(format!("Installing {label}"));
        }
        log.push_str(&line);
        log.push('\n');
    }
    let ok = child
        .wait()
        .with_context(|| format!("waiting for {what}"))?
        .success();
    let log = format!("{log}{}", errs.join().unwrap_or_default());
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
fn run_child(mut cmd: Command, verbose: bool, what: &str, initial: &str) -> bool {
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

pub fn converge(effective: &[Pkg], dry_run: bool, verbose: bool) -> Result<usize> {
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
            // Replay everything the capture swallowed, then fail (the exec-step
            // contract: quiet on success, the full story on failure).
            if !log.is_empty() {
                eprint!("{log}");
            }
            bail!("brew bundle failed");
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
        // best-effort: a missing remote or app shouldn't abort the whole run —
        // but it must be *reported*, not swallowed the way `let _ = status()` did.
        run_child(cmd, verbose, "flatpak install", "installing flatpaks");
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
        batch_then_isolate(&ids, "mas install", |batch| {
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

    Ok(effective.len())
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
pub fn upgrade(verbose: bool) -> Result<()> {
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
        run_child(cmd, verbose, "brew upgrade", "upgrading packages");
    }
    if have("flatpak") {
        let mut cmd = Command::new("flatpak");
        cmd.args(["update", "-y", "--noninteractive"]);
        // Captured (see `run_child`): flatpak prints `Nothing to update.` whenever
        // its remotes carry nothing new, which — mid-run, in flatpak's voice —
        // reads as temper's verdict on a converge that is about to install and
        // upgrade plenty. The download progress it prints instead becomes a
        // spinner, and a real failure is now reported rather than discarded.
        run_child(cmd, verbose, "flatpak update", "upgrading flatpaks");
    }
    Ok(())
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

pub fn prune_apply(
    effective: &[Pkg],
    extras: &[(Manager, String)],
    ignore: &crate::manifest::Ignore,
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
        let status = Command::new("brew")
            .args(["bundle", "cleanup", "--force"])
            .args(&flags)
            .arg("--file")
            .arg(&tmp)
            .stdout(if crate::ui::json_mode() { stderr_stdio() } else { Stdio::inherit() })
            .status()
            .context("running brew bundle cleanup")?;
        let _ = fs::remove_file(&tmp);
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
    let landed = batch_then_isolate(uuids, "gext uninstall", |batch| {
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
    let installed = batch_then_isolate(&missing, "gext install", |batch| {
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

/// Every configured remote, **both installations**, three-valued.
///
/// This is the observe side of a *declaration*: "does the machine have the
/// remote I declared?" is answered by either installation having it, which is why
/// it carries no scope flag. Reading only `--user` made a remote the image
/// provides system-wide permanently `missing`, and added a duplicate user copy on
/// every converge. `None` is "could not ask", never "no remotes".
pub fn flatpak_remotes_installed() -> Option<Vec<(String, String)>> {
    remotes_in_scope(&[])
}

/// The remotes temper may **remove** — the user installation only, because that
/// is the only one `remotes_delete` writes to.
///
/// Deliberately narrower than the app path, which removes from both
/// installations: deleting a *system* remote is not the app case scaled up.
/// `remote-delete` is passed `--force`, which overrides flatpak's own refusal to
/// drop a remote that installed apps still update from — so on a spec declaring
/// one vendor remote, an undeclared `flathub` would become an extra prune offers
/// to force-delete out from under everything installed from it. That is a fleet
/// decision, recorded in ROADMAP under "Bugs", not a symmetry to complete here.
pub fn flatpak_user_remotes() -> Option<Vec<(String, String)>> {
    remotes_in_scope(&["--user"])
}

/// Remotes visible in the given scope. With no scope flag `flatpak remotes`
/// spans **both** installations, which is what a declaration has to be checked
/// against: reading only `--user` made a declared remote that exists system-wide
/// permanently `missing`, and every converge added a duplicate user-scope copy
/// of a remote the machine already had.
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

/// Declared remotes that are absent, or present under a different url.
pub fn remotes_missing(effective: &[(String, String)]) -> Vec<String> {
    if effective.is_empty() {
        return Vec::new();
    }
    let Some(live) = flatpak_remotes_installed() else {
        return Vec::new();
    };
    effective
        .iter()
        .filter(|(n, u)| !live.iter().any(|(ln, lu)| ln == n && lu.trim_end_matches('/') == u.trim_end_matches('/')))
        .map(|(n, u)| format!("{n} {u}"))
        .collect()
}

/// Remotes configured but declared nowhere. Gated on declaring at least one, the
/// probe invariant every other manager follows.
pub fn remotes_extras(effective: &[(String, String)], ignore: &manifest::Ignore) -> Vec<String> {
    if effective.is_empty() {
        return Vec::new();
    }
    // Only the user installation, because that is the only one `remotes_delete`
    // can act on. Offering a system remote would be a removal with no code path
    // behind it — the defect the feature interface exists to catch.
    let Some(live) = flatpak_user_remotes() else {
        return Vec::new();
    };
    let mut out: Vec<String> = live
        .into_iter()
        .map(|(n, _)| n)
        .filter(|n| {
            !effective.iter().any(|(en, _)| en == n) && !ignore.flatpak_remote.contains(n)
        })
        .collect();
    out.sort();
    out
}

/// Add declared remotes. `--if-not-exists` makes it idempotent, `--user` keeps
/// it in the scope temper owns.
pub fn remotes_converge(effective: &[(String, String)], dry_run: bool) -> Result<Vec<String>> {
    if dry_run || effective.is_empty() || !have("flatpak") {
        return Ok(Vec::new());
    }
    let missing = remotes_missing(effective);
    // One invocation per remote is unavoidable here: `remote-add` takes exactly
    // one name/url pair, unlike every other converge on this page.
    let mut added = Vec::new();
    for token in &missing {
        let Some((name, url)) = parse_remote(token) else {
            continue;
        };
        // `--if-not-exists` exits 0 doing nothing when the name is already
        // configured with a DIFFERENT url — and the url is exactly what
        // `remotes_missing` compares, so such a remote reported missing forever
        // and every converge claimed to have added it. The name is the identity
        // and the url is the thing that drifts, so converging one means setting
        // it: `remote-modify` where it exists, `remote-add` where it does not.
        let exists = flatpak_user_remotes()
            .unwrap_or_default()
            .iter()
            .any(|(n, _)| *n == name);
        let args: Vec<&str> = if exists {
            vec!["remote-modify", "--user", "--url", &url, &name]
        } else {
            vec!["remote-add", "--user", "--if-not-exists", &name, &url]
        };
        let ok = Command::new("flatpak")
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            added.push(name);
        } else {
            eprintln!(
                "{} flatpak remote-add {name} failed — skipped",
                crate::ui::yellow(crate::ui::g_warn())
            );
        }
    }
    Ok(added)
}

/// Remove undeclared remotes — the prune side.
pub fn remotes_delete(names: &[String]) -> Result<()> {
    if names.is_empty() {
        return Ok(());
    }
    if !have("flatpak") {
        bail!("flatpak not found — cannot remove remotes on this host");
    }
    let landed = batch_then_isolate(names, "flatpak remote-delete", |batch| {
        let mut c = Command::new("flatpak");
        c.args(["remote-delete", "--user", "--force"]);
        for n in batch {
            c.arg(n);
        }
        c
    });
    if landed.len() != names.len() {
        bail!("could not remove every remote");
    }
    Ok(())
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
    run_child(cmd, verbose, "rpm-ostree uninstall", "un-layering rpms");
    Ok(true) // a staged deployment needs a reboot, same as layering
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
        let landed = batch_then_isolate(&items, "t", |batch| {
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
        let landed = batch_then_isolate(&items, "t", |batch| {
            // Fails for the whole batch and for "b" alone; succeeds otherwise.
            let fail = batch.len() > 1 || batch[0] == "b";
            Command::new(if fail { "false" } else { "true" })
        });
        assert_eq!(landed, vec!["a".to_string(), "c".to_string()]);

        assert!(batch_then_isolate(&[], "t", |_| Command::new("true")).is_empty());
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
            flatpak_remotes: Vec::new(),
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
    let _ = verbose;
    let landed = batch_then_isolate(&missing, "rpm-ostree install", |items| {
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
pub fn brew_extras(effective: &[Pkg], ignore: &manifest::Ignore) -> Result<Vec<(Manager, String)>> {
    brew_extras_inner(effective, ignore, false)
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
    brew_extras_inner(&[], ignore, true)
}

fn brew_extras_inner(
    effective: &[Pkg],
    ignore: &manifest::Ignore,
    seed: bool,
) -> Result<Vec<(Manager, String)>> {
    if !have("brew") {
        return Ok(Vec::new());
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
        return Ok(Vec::new());
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
        return Ok(Vec::new());
    }
    let ignored: HashSet<&str> = ignore
        .brew
        .iter()
        .chain(&ignore.cask)
        .chain(&ignore.tap)
        .map(String::as_str)
        .collect();
    Ok(parse_cleanup_extras(&text, &ignored))
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
