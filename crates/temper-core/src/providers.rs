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
fn is_noteworthy(line: &str) -> bool {
    let l = line.trim_start();
    l.starts_with("Warning:") || l.starts_with("Error:")
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
fn run_with_spinner(mut cmd: Command, initial: &str) -> Result<(bool, String)> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().context("spawning brew bundle")?;
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
    let ok = child.wait().context("waiting for brew bundle")?.success();
    let log = format!("{log}{}", errs.join().unwrap_or_default());
    pb.finish_and_clear();

    if ok {
        for line in noteworthy_lines(&log) {
            eprintln!("{line}"); // stderr: keeps `--json` clean
        }
    }
    Ok((ok, log))
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
            let (ok, log) = run_with_spinner(cmd, "converging packages")?;
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
        // best-effort: a missing remote or app shouldn't abort the whole run
        let _ = cmd.status();
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
                        "⚠ mas install {} (id {id}) failed — skipped (App Store sign-in, or an \
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
    let _ = cmd.status();
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
        let _ = cmd.status();
    }
    if have("flatpak") {
        let _ = Command::new("flatpak")
            .args(["update", "-y", "--noninteractive"])
            .status();
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
        let status = Command::new("brew")
            .args(["bundle", "cleanup", "--force", "--file"])
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
        let _ = cmd.status();
    }
    Ok(())
}

/// Dump live package state into the folder at `machines/<name>/Brewfile` via
/// `brew bundle dump`. Returns the written path. VM-verified.
/// `brew bundle dump --force` to `dest` (creating parent dirs). The caller
/// picks `dest` — for `backup` that's the machine's own `brewfile` so the dump
/// lands in the file the machine actually reads.
pub fn dump_to(dest: &Path) -> Result<()> {
    if !have("brew") {
        bail!("brew not found — cannot dump package state");
    }
    if let Some(p) = dest.parent() {
        fs::create_dir_all(p).with_context(|| format!("creating {}", p.display()))?;
    }
    let status = Command::new("brew")
        .args(["bundle", "dump", "--force", "--no-vscode", "--file"])
        .arg(dest)
        .status()
        .context("running brew bundle dump")?;
    if !status.success() {
        bail!("brew bundle dump failed");
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
    Ok(out)
}

fn gext_installed() -> Vec<String> {
    // `gnome-extensions list` prints one UUID per line.
    if have("gnome-extensions") {
        run_lines("gnome-extensions", &["list"]).unwrap_or_default()
    } else {
        Vec::new()
    }
}

/// Declared extensions not installed. Empty (no-op) where GNOME isn't present.
pub fn gext_missing(effective: &[String]) -> Vec<String> {
    if effective.is_empty() || (!have("gext") && !have("gnome-extensions")) {
        return Vec::new();
    }
    let installed = gext_installed();
    effective
        .iter()
        .filter(|e| !installed.contains(e))
        .cloned()
        .collect()
}

/// Install missing extensions via `gext`. VM-verified.
pub fn gext_converge(effective: &[String], dry_run: bool) -> Result<()> {
    if dry_run || !have("gext") {
        return Ok(());
    }
    for uuid in gext_missing(effective) {
        let _ = Command::new("gext").args(["install", &uuid]).status();
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
        let (ok, log) = run_with_spinner(cmd, "test").unwrap();
        assert!(!ok, "non-zero exit must report failure");
        assert!(log.contains("Installing Cask zoom"), "stdout lost: {log:?}");
        assert!(log.contains("Error: nope"), "stderr lost: {log:?}");
    }

    #[test]
    fn capture_reports_success_and_drains_both_streams() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo out; echo err 1>&2"]);
        let (ok, log) = run_with_spinner(cmd, "test").unwrap();
        assert!(ok);
        assert!(log.contains("out") && log.contains("err"), "{log:?}");
    }

    #[test]
    fn warnings_survive_a_successful_capture() {
        // Quiet-on-success must not swallow advisories the streamed path showed.
        assert!(is_noteworthy("Warning: wget 1.25.0 is already installed"));
        assert!(is_noteworthy("  Error: cask 'x' is unavailable"));
        assert!(!is_noteworthy("==> Fetching llvm"));
        assert!(!is_noteworthy("Using wget"));
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
pub fn rpm_converge(effective: &[String], dry_run: bool) -> Result<bool> {
    let missing = rpm_missing(effective);
    if dry_run || missing.is_empty() || !have("rpm-ostree") {
        return Ok(false);
    }
    let mut cmd = Command::new("rpm-ostree");
    cmd.args(["install", "--idempotent"]);
    for p in &missing {
        cmd.arg(p);
    }
    let _ = cmd.status();
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
