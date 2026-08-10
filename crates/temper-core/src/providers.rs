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
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::manifest::{self, Machine};
use crate::packages::{Installed, Manager, Pkg};
use crate::primitives::which;

fn have(cmd: &str) -> bool {
    which(cmd).is_some()
}

/// Run a command and return its non-empty output lines (trimmed). A non-zero
/// exit yields an empty list rather than an error — probing is best-effort.
fn run_lines(cmd: &str, args: &[&str]) -> Result<Vec<String>> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("running {cmd} {args:?}"))?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// Snapshot what's installed, but only for managers that (a) appear in the
/// effective set and (b) have their CLI present. Absent managers stay unprobed
/// (so nothing is reported as missing/extra for them).
pub fn probe(effective: &[Pkg]) -> Result<Installed> {
    let managers: HashSet<Manager> = effective.iter().map(|p| p.manager).collect();
    let mut inst = Installed::default();

    if have("brew") {
        if managers.contains(&Manager::Brew) {
            inst.set(Manager::Brew, run_lines("brew", &["list", "--formula"])?);
        }
        if managers.contains(&Manager::Cask) {
            inst.set(Manager::Cask, run_lines("brew", &["list", "--cask"])?);
        }
        if managers.contains(&Manager::Tap) {
            inst.set(Manager::Tap, run_lines("brew", &["tap"])?);
        }
    }
    if managers.contains(&Manager::Flatpak) && have("flatpak") {
        inst.set(
            Manager::Flatpak,
            run_lines("flatpak", &["list", "--app", "--columns=application"])?,
        );
    }
    if managers.contains(&Manager::Mas) && have("mas") {
        // `mas list` rows look like: "497799835  Xcode (14.0)" → first token.
        let ids = run_lines("mas", &["list"])?
            .into_iter()
            .filter_map(|l| l.split_whitespace().next().map(String::from));
        inst.set(Manager::Mas, ids.collect::<Vec<_>>());
    }
    if managers.contains(&Manager::Vscode) && have("code") {
        let exts = run_lines("code", &["--list-extensions"])?
            .into_iter()
            .map(|s| s.to_lowercase());
        inst.set(Manager::Vscode, exts.collect::<Vec<_>>());
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
        // The user asked to see it — stream live, exactly as before.
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
pub fn converge(effective: &[Pkg], dry_run: bool, verbose: bool) -> Result<usize> {
    // mas is converged SEPARATELY (below), not via the aggregate brew bundle:
    // it is the flakiest provider (no App Store sign-in, an app not tied to the
    // Apple ID), and riding brew bundle means one mas failure aborts the whole
    // converge. Split out, its failures are warned and skipped (Principle #6).
    let brewish: Vec<&Pkg> = effective
        .iter()
        .filter(|p| {
            matches!(
                p.manager,
                Manager::Brew | Manager::Cask | Manager::Tap | Manager::Vscode
            )
        })
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
        .filter(|p| p.manager == Manager::Flatpak)
        .map(|p| p.name.as_str())
        .collect();
    if !flatpaks.is_empty() && have("flatpak") && !dry_run {
        let mut cmd = Command::new("flatpak");
        cmd.args(["install", "-y", "--noninteractive"]);
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
        .filter(|p| p.manager == Manager::Mas)
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
        // One spinner for the whole App Store phase: these are installed strictly
        // one at a time (mas has no batch mode), each is a full download, and a
        // big one otherwise looks like a hang. The counter says how far in we are.
        let pb = (!verbose && !todo.is_empty())
            .then(|| crate::ui::spinner_counted(todo.len() as u64, "App Store"));
        for p in &todo {
            let id = p.id.as_deref().unwrap_or(&p.name);
            if let Some(pb) = &pb {
                pb.set_message(format!("Installing {}", p.name));
            }
            let mut cmd = Command::new("mas");
            // Quiet by default: mute mas's post-install "not indexed in Spotlight"
            // warnings. `--verbose` lets them through.
            if !verbose {
                cmd.env("MAS_NO_AUTO_INDEX", "1");
            }
            cmd.args(["install", id]);
            // Captured on the quiet path so mas's own progress doesn't fight the
            // spinner for the cursor; streamed under `--verbose`, as before.
            let ok = if verbose {
                cmd.status().map(|s| s.success()).unwrap_or(false)
            } else {
                cmd.output().map(|o| o.status.success()).unwrap_or(false)
            };
            if let Some(pb) = &pb {
                pb.inc(1);
            }
            if !ok {
                let warn = || {
                    eprintln!(
                        "! mas install {} (id {id}) failed — skipped (App Store sign-in, or an \
                         Apple/iWork app mas can't install — get it from the App Store directly).",
                        p.name
                    )
                };
                // Inside `suspend`, so the warning doesn't land on the spinner's line.
                match &pb {
                    Some(pb) => pb.suspend(warn),
                    None => warn(),
                }
            }
        }
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
pub fn prune_apply(effective: &[Pkg], extras: &[(Manager, String)]) -> Result<()> {
    let has_brewish = effective.iter().any(|p| {
        matches!(
            p.manager,
            Manager::Brew | Manager::Cask | Manager::Tap | Manager::Mas | Manager::Vscode
        )
    });
    if has_brewish && have("brew") {
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
        for (m, flag) in [
            (Manager::Brew, "--formula"),
            (Manager::Cask, "--cask"),
            (Manager::Tap, "--tap"),
            (Manager::Mas, "--mas"),
            (Manager::Vscode, "--vscode"),
        ] {
            if effective.iter().any(|p| p.manager == m) {
                flags.push(flag);
            }
        }
        let status = Command::new("brew")
            .args(["bundle", "cleanup", "--force"])
            .args(&flags)
            .arg("--file")
            .arg(&tmp)
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
    if !flatpaks.is_empty() && have("flatpak") {
        let mut cmd = Command::new("flatpak");
        cmd.args(["uninstall", "-y", "--noninteractive"]);
        for f in &flatpaks {
            cmd.arg(f);
        }
        // Streamed on purpose (unlike the converge children): `prune` is manual,
        // confirmed and destructive, the user is at the keyboard, and *what was
        // removed* is the deliverable. Only the discarded exit code was a bug.
        if !cmd.status().map(|s| s.success()).unwrap_or(false) {
            eprintln!(
                "{} flatpak uninstall failed — {} extra(s) may remain",
                crate::ui::yellow(crate::ui::g_warn()),
                flatpaks.len()
            );
        }
    }
    Ok(())
}

// --- gext: GNOME extensions (Linux desktop) -----------------------------------

/// Union of a machine's composed apps' `extensions`, de-duplicated.
pub fn effective_extensions(home: &Path, machine: &Machine) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for app in &machine.apps {
        let bundle = manifest::load_bundle(home, app)?;
        // Bundle-level os/role gate: a server (or a Mac) never layers a
        // desktop-Linux bundle's GNOME extensions, even if it composes it.
        if manifest::gated(&bundle.os, &bundle.role, machine) {
            continue;
        }
        for uuid in bundle.extensions {
            if seen.insert(uuid.clone()) {
                out.push(uuid);
            }
        }
    }
    // The machine's own list, unioned last — same rule `packages` uses.
    for uuid in &machine.extensions {
        if seen.insert(uuid.clone()) {
            out.push(uuid.clone());
        }
    }
    Ok(out)
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

/// Extensions installed in the **user** scope (`~/.local/share/...`). System
/// ones are excluded on purpose: those ship with the image, and drift reports
/// image-baked items status-only rather than as something you failed to declare.
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
    gext_extras_from(&installed_user, effective, &ignore.gext)
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
    let mut failed = Vec::new();
    for uuid in uuids {
        let ok = Command::new("gext")
            .args(["uninstall", uuid])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            failed.push(uuid.clone());
        }
    }
    if !failed.is_empty() {
        bail!("could not uninstall: {}", failed.join(", "));
    }
    Ok(())
}

/// Install missing extensions via `gext`. VM-verified.
///
/// One spinner for the whole phase with a counter (the `mas` shape): extensions
/// install one at a time, and `gext`'s own per-extension chatter would otherwise
/// stand in temper's output as if it were temper speaking. A failure is warned
/// and skipped — one unavailable extension must not fail a converge.
pub fn gext_converge(effective: &[String], dry_run: bool, verbose: bool) -> Result<()> {
    if dry_run {
        return Ok(());
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
        return Ok(());
    }
    let pb = (!verbose && !missing.is_empty())
        .then(|| crate::ui::spinner_counted(missing.len() as u64, "GNOME extensions"));
    for uuid in &missing {
        if let Some(pb) = &pb {
            pb.set_message(format!("Installing {uuid}"));
        }
        let mut cmd = Command::new("gext");
        cmd.args(["install", uuid]);
        let ok = if verbose {
            cmd.status().map(|s| s.success()).unwrap_or(false)
        } else {
            cmd.stdin(Stdio::null())
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if let Some(pb) = &pb {
            pb.inc(1);
        }
        if !ok {
            let warn = || eprintln!("{} gext install {uuid} failed — skipped", crate::ui::yellow(crate::ui::g_warn()));
            match &pb {
                Some(pb) => pb.suspend(warn),
                None => warn(),
            }
        }
    }
    if let Some(pb) = pb {
        pb.finish_and_clear();
    }
    Ok(())
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
        for pkg in bundle.rpm {
            if seen.insert(pkg.clone()) {
                out.push(pkg);
            }
        }
    }
    Ok(out)
}

/// Declared rpms not installed (`rpm -q`). Empty where rpm isn't present.
pub fn rpm_missing(effective: &[String]) -> Vec<String> {
    if effective.is_empty() || !have("rpm") {
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
            extensions: Vec::new(),
            brewfile: None,
            vars: Default::default(),
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
pub fn rpm_converge(effective: &[String], dry_run: bool, verbose: bool) -> Result<bool> {
    let missing = rpm_missing(effective);
    if dry_run || missing.is_empty() || !have("rpm-ostree") {
        return Ok(false);
    }
    let mut cmd = Command::new("rpm-ostree");
    cmd.args(["install", "--idempotent"]);
    for p in &missing {
        cmd.arg(p);
    }
    run_child(cmd, verbose, "rpm-ostree install", "layering rpms");
    Ok(true) // layered rpms require a reboot to take effect
}

// --- dependency-aware brew extras (read-only) ---------------------------------

/// Formulae/casks/taps installed but not needed by the declared set, per
/// `brew bundle cleanup` (no `--force`, so read-only). Dependency-aware: a kept
/// package's transitive deps are NOT reported — unlike a naive set-diff. Each
/// extra is tagged with its manager: a `tap` orphan keeps its full `user/repo`
/// name (it round-trips as a `tap` line); a formula/cask keeps brew's short name
/// and is tagged `Brew` for the caller to reclassify/qualify. `[ignore]` applied.
pub fn brew_extras(effective: &[Pkg], ignore: &manifest::Ignore) -> Result<Vec<(Manager, String)>> {
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
    if body.is_empty() {
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
