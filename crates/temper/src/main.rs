//! temper — converge a machine to a declared spec kept in a folder of
//! human-readable files.
//!
//! This is the thin CLI layer; all logic lives in `temper-core`. Each command
//! resolves the temper-home folder + this machine's identity, calls into core,
//! and renders the result as a human summary or (`--json`) a machine-readable
//! document.
//!
//! Status: all verbs are live. Read-only paths (drift, dry-run, package probe)
//! are verified against a real machine, and many write paths are now verified
//! live on a Bazzite host (brew-bundle converge, dconf snapshot/restore, dconf
//! setkey journaling+undo, sysfile drift). `defaults`/mas and full flatpak
//! converge await a Mac / a fuller run.

use std::io;
use std::io::IsTerminal;
use std::process::ExitCode;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;

use temper_core::{discovery, git, journal, machine, manifest, plan, reconcile, ui};

const REPO_URL: &str = "https://github.com/jakobhviid/temper";

#[derive(Parser)]
#[command(
    name = "temper",
    version,
    about = "Converge a machine to a declared spec kept in a folder of human-readable files.",
    disable_help_subcommand = true
)]
struct Cli {
    /// Emit machine-readable JSON instead of human output.
    #[arg(long, global = true)]
    json: bool,

    /// Print the full LLM-readable guide (every command + the design) and exit.
    #[arg(long, global = true)]
    llm: bool,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Converge a machine to its spec (add packages + apply all config).
    ///
    /// Full converge: add missing packages, apply everything, run one-time
    /// setup. Defaults to this machine (resolved by hostname).
    Install {
        /// Machine name (default: resolved from hostname). Passing a name that
        /// isn't this host is allowed, but a live install asks to confirm first
        /// (temper converges the LOCAL machine).
        machine: Option<String>,
        /// Show what would change without touching anything.
        #[arg(long)]
        dry_run: bool,
        /// Converge only packages (add missing; never remove), skipping the
        /// config-step phase — the additive, no-churn "install-missing" flow.
        #[arg(long)]
        packages_only: bool,
        /// Skip the confirmation when the named machine isn't this host.
        #[arg(long)]
        yes: bool,
    },
    /// Upgrade packages + re-apply `always` config (adds no new apps).
    ///
    /// Re-apply the `always` config steps and upgrade declared packages
    /// (`brew upgrade` + `flatpak update`). Does not add newly-declared apps.
    Update,
    /// Show what's out of sync (read-only), with the commands to fix it.
    ///
    /// Read-only: package set, managed files, keys, and assertions. Ends with a
    /// "Next steps" summary — the exact command for each way out of the drift.
    Drift {
        /// Machine name (default: resolved from hostname).
        machine: Option<String>,
    },
    /// Remove installed-but-undeclared packages (dependency-aware).
    ///
    /// Honors the machine's `[ignore]` baseline; a kept package's transitive
    /// dependencies are not flagged as extras.
    Prune {
        /// List what would be removed without removing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Dump live package state (+ dconf snapshots) into the folder.
    ///
    /// `brew bundle dump` → the machine's own `brewfile` (the file it reads;
    /// falls back to machines/<name>/Brewfile if none), plus each declared
    /// `[[machine.dconf]]` snapshot (filtered). Spec←machine, wholesale, journaled.
    Backup {
        /// Machine name (default: resolved from hostname).
        machine: Option<String>,
    },
    /// List installed packages not in the spec (advisory, non-mutating).
    ///
    /// Report installed extras so you can add them to a bundle, the machine's
    /// loose list, or `[ignore]` — or run `reconcile` to act on them per-item.
    Adopt,
    /// Interactively absorb extras / drop missing entries (spec←machine).
    ///
    /// Reconcile the machine's Brewfile with reality: add installed-but-
    /// undeclared extras, drop declared-but-absent entries, or route a flatpak
    /// extra to `[ignore]`. Edits only the machine's own Brewfile; `--json`
    /// previews the plan without prompting.
    Reconcile {
        /// Machine name (default: resolved from hostname).
        machine: Option<String>,
    },
    /// Load dconf snapshot(s) back into live dconf (confirm-gated).
    ///
    /// spec→machine. Clobbers live desktop tweaks, so it is never part of
    /// `update`. Use after a reinstall, or to reset the desktop to the snapshot.
    Restore {
        /// Machine name (default: resolved from hostname).
        machine: Option<String>,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Revert the last mutating run.
    Undo {
        /// Run id to revert (default: the most recent).
        run: Option<String>,
        /// List revertible runs instead of reverting.
        #[arg(long)]
        list: bool,
        /// Show what would be reverted without touching anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Pick or record which temper-home folder to use.
    ///
    /// Provision this machine's temper-home: pick from discovered libraries (or
    /// paste a path) and save it as the default pointer
    /// (`$XDG_CONFIG_HOME/temper/home`), so temper finds it from anywhere. Omit
    /// the dir to choose interactively.
    #[command(alias = "use")]
    Setup {
        /// The temper-home to use. Omit to auto-discover and pick.
        dir: Option<String>,
    },
    /// Fetch calibrated speaker profiles into the folder.
    ///
    /// Pull them from the configured repo (`[eq_import]` in temper.toml), ready
    /// for the `speaker-eq` step. Folder-authoring — it writes into your config
    /// folder, not a machine.
    EqImport,
    /// Commit (and push) spec changes to the git-backed home.
    ///
    /// `git add -A && commit && push` in your temper-home, so the folder doesn't
    /// drift after a `reconcile`/`backup`/`eq-import` or a hand edit. The commit
    /// message is generated from what changed unless you pass `-m`. Pulls
    /// `--ff-only` before pushing. A no-op if the home isn't a git repo.
    Save {
        /// Commit message (default: auto-generated from the changed files).
        #[arg(short, long)]
        message: Option<String>,
        /// Commit but don't push.
        #[arg(long)]
        no_push: bool,
    },
    /// Show or configure git automation for the home.
    ///
    /// With no subcommand: show whether the home is git, its branch/ahead-behind,
    /// and the current `[git]` settings. `enable`/`disable` write `[git]` in
    /// temper.toml so temper can auto-commit (and optionally push/pull) around
    /// spec-writing verbs.
    Git {
        #[command(subcommand)]
        action: Option<GitAction>,
    },
    /// Print a shell completion script.
    Completions {
        /// The shell to generate for.
        shell: Shell,
    },
}

#[derive(Subcommand)]
enum GitAction {
    /// Turn on auto-commit after a spec-writing verb (optionally push/pull).
    Enable {
        /// Also push after each auto-commit and on `save`.
        #[arg(long)]
        push: bool,
        /// Also `git pull --ff-only` before a run (warns if it can't).
        #[arg(long)]
        pull: bool,
    },
    /// Turn off all git automation (back to hint-only).
    Disable,
}

fn main() -> ExitCode {
    // `--llm` and `--man` are documentation flags like `--help`: they work from
    // anywhere and need no subcommand, so intercept them before clap enforces
    // one. `--man` is a flag (not a subcommand) so it never leaks into the shell
    // completion list, which hidden subcommands do.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--llm") {
        print!("{}", llm_guide());
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--man") {
        if clap_mangen::Man::new(Cli::command())
            .render(&mut io::stdout())
            .is_err()
        {
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    }
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("error: {e:#}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn run(cli: Cli) -> Result<()> {
    let json = cli.json;
    match cli.cmd {
        None => {
            Cli::command().print_help()?;
            println!();
        }
        Some(Cmd::Completions { shell }) => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "temper", &mut io::stdout());
        }
        Some(Cmd::Install { machine, dry_run, packages_only, yes }) => {
            cmd_install(machine, dry_run, packages_only, yes, json)?
        }
        Some(Cmd::Update) => cmd_update(json)?,
        Some(Cmd::Drift { machine }) => cmd_drift(machine, json)?,
        Some(Cmd::Undo { run, list, dry_run }) => cmd_undo(run, list, dry_run, json)?,
        Some(Cmd::Prune { dry_run }) => cmd_prune(dry_run, json)?,
        Some(Cmd::Backup { machine }) => cmd_backup(machine, json)?,
        Some(Cmd::Adopt) => cmd_adopt(json)?,
        Some(Cmd::Reconcile { machine }) => cmd_reconcile(machine, json)?,
        Some(Cmd::Restore { machine, yes }) => cmd_restore(machine, yes, json)?,
        Some(Cmd::EqImport) => cmd_eq_import(json)?,
        Some(Cmd::Save { message, no_push }) => cmd_save(message, no_push, json)?,
        Some(Cmd::Git { action }) => cmd_git(action, json)?,
        Some(Cmd::Setup { dir }) => cmd_setup(dir, json)?,
    }
    Ok(())
}

/// Save a temper-home as the default (a saved pointer discovery reads).
/// Find the temper-home, pulling `--ff-only` first when fleet `[git].auto_pull`
/// is on (so a run works on the latest spec). A pull failure only warns.
fn find_home_pulling() -> Result<std::path::PathBuf> {
    let home = discovery::find_home()?;
    if manifest::peek_auto_pull(&home) {
        if let git::Pull::Warn(w) = git::pull_ff(&home) {
            eprintln!("{} couldn't pull — working on a possibly-stale spec: {w}", ui::yellow("⚠"));
        }
    }
    Ok(home)
}

/// After a spec-writing verb, either auto-commit (per `[git]`) or hint. A no-op
/// on a non-git home. All output goes to stderr so `--json` stdout stays clean.
fn after_repo_change(home: &std::path::Path, gc: &manifest::GitConfig, auto_msg: &str) {
    if !git::is_repo(home) {
        return; // dormant on a non-git folder
    }
    if gc.auto_commit {
        match git::save(home, auto_msg, gc.auto_push) {
            Ok(r) => {
                if r.committed {
                    eprintln!("{} committed: {}", ui::green("✓"), r.message);
                }
                if r.pushed {
                    eprintln!("{} pushed", ui::green("✓"));
                }
                if let Some(w) = r.warning {
                    eprintln!("{} {w}", ui::yellow("⚠"));
                }
            }
            Err(e) => eprintln!("{} auto-commit failed: {e:#}", ui::yellow("⚠")),
        }
    } else if gc.remind && git::is_dirty(home) {
        let name = home
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| home.display().to_string());
        eprintln!(
            "{} {name} has uncommitted spec changes — {} to commit + push (or edit + commit yourself).",
            ui::cyan("ⓘ"),
            ui::bold("temper save")
        );
    }
}

/// Commit (and push) the home's pending spec changes.
fn cmd_save(message: Option<String>, no_push: bool, json: bool) -> Result<()> {
    let home = discovery::find_home()?;
    if !git::is_repo(&home) {
        if json {
            println!("{}", serde_json::json!({ "saved": false, "reason": "not a git repo" }));
            return Ok(());
        }
        anyhow::bail!("{} is not a git repo — nothing to save", home.display());
    }
    let msg = message.unwrap_or_else(|| git::message_from_changes(&home));
    let r = git::save(&home, &msg, !no_push)?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "saved": r.committed, "pushed": r.pushed,
                "message": r.message, "warning": r.warning
            })
        );
    } else {
        if r.committed {
            println!("{} committed: {}", ui::green("✓"), r.message);
        } else {
            println!("nothing to commit — the folder is clean.");
        }
        if r.pushed {
            println!("{} pushed", ui::green("✓"));
        }
        if let Some(w) = r.warning {
            eprintln!("{} {w}", ui::yellow("⚠"));
        }
    }
    Ok(())
}

/// Show or configure the home's git automation.
fn cmd_git(action: Option<GitAction>, json: bool) -> Result<()> {
    let home = discovery::find_home()?;
    match action {
        None => {
            let is_repo = git::is_repo(&home);
            let ft = manifest::load_fleet(&home)?;
            let gc = manifest::effective_git(&ft.git, &None);
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "git_repo": is_repo,
                        "status": is_repo.then(|| git::status_line(&home)),
                        "remind": gc.remind, "auto_commit": gc.auto_commit,
                        "auto_push": gc.auto_push, "auto_pull": gc.auto_pull
                    })
                );
            } else if is_repo {
                println!("{} {}", ui::bold("git:"), git::status_line(&home));
                println!(
                    "settings: remind={} auto_commit={} auto_push={} auto_pull={}",
                    gc.remind, gc.auto_commit, gc.auto_push, gc.auto_pull
                );
            } else {
                println!("{} not a git repo — git automation is dormant.", home.display());
            }
        }
        Some(GitAction::Enable { push, pull }) => {
            git::write_config(&home, true, true, push, pull)?;
            println!(
                "{} git automation enabled (auto_commit{}{})",
                ui::green("✓"),
                if push { " + push" } else { "" },
                if pull { " + pull" } else { "" }
            );
        }
        Some(GitAction::Disable) => {
            git::write_config(&home, true, false, false, false)?;
            println!("{} git automation disabled (hint-only)", ui::green("✓"));
        }
    }
    Ok(())
}

fn cmd_setup(dir: Option<String>, json: bool) -> Result<()> {
    // Explicit path → save it directly (scriptable form).
    if let Some(d) = dir {
        return save_and_report(&manifest::expand_tilde(&d), json);
    }

    let candidates = discovery::scan_candidates();

    // Non-interactive (piped/`--json`) can't prompt: report candidates for `--json`,
    // otherwise tell the user to pass a path.
    if json {
        let arr: Vec<_> = candidates.iter().map(|p| p.display().to_string()).collect();
        println!("{}", serde_json::json!({ "candidates": arr }));
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        let found = if candidates.is_empty() {
            "none discovered".to_string()
        } else {
            candidates.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
        };
        anyhow::bail!("not a terminal — run `temper setup <dir>` with an explicit path ({found})");
    }

    // Interactive picker.
    println!("{}", ui::bold("Set up temper — choose your temper-home:"));
    for (i, c) in candidates.iter().enumerate() {
        println!("  {}) {}", ui::cyan(&format!("{}", i + 1)), c.display());
    }
    println!("  {}) paste a path", ui::cyan("p"));
    println!("  {}) cancel", ui::cyan("q"));
    print!("> ");
    let reply = read_reply();

    let chosen = if reply.is_empty() || reply == "q" {
        println!("cancelled — nothing changed.");
        return Ok(());
    } else if reply == "p" {
        print!("path> ");
        let p = read_line_raw();
        if p.is_empty() {
            println!("cancelled — nothing changed.");
            return Ok(());
        }
        manifest::expand_tilde(&p)
    } else if let Ok(n) = reply.parse::<usize>() {
        match candidates.get(n.wrapping_sub(1)) {
            Some(p) => p.clone(),
            None => anyhow::bail!("no candidate {n} (pick 1..{})", candidates.len()),
        }
    } else {
        anyhow::bail!("unrecognized choice `{reply}` — pick a number, `p`, or `q`");
    };
    save_and_report(&chosen, json)
}

/// Save the chosen temper-home as the pointer and report it.
fn save_and_report(target: &std::path::Path, json: bool) -> Result<()> {
    let pointer = discovery::save_pointer(target)?;
    if json {
        println!(
            "{}",
            serde_json::json!({ "home": target.display().to_string(), "pointer": pointer.display().to_string() })
        );
    } else {
        println!("{} temper home set to {}", ui::green("✓"), target.display());
    }
    Ok(())
}

/// Folder-authoring: fetch calibrated speaker profiles into the folder.
fn cmd_eq_import(json: bool) -> Result<()> {
    let home = find_home_pulling()?;
    let ft = manifest::load_fleet(&home)?;
    let cfg = ft.eq_import.ok_or_else(|| {
        anyhow::anyhow!(
            "no [eq_import] in temper.toml — add `repo = \"...\"` (and optional `dest`) to import"
        )
    })?;
    let written = temper_core::eq_import::run(&home, &cfg)?;
    let paths: Vec<String> = written.iter().map(|p| p.display().to_string()).collect();
    if json {
        println!("{}", serde_json::json!({ "repo": cfg.repo, "imported": paths }));
    } else {
        for p in &paths {
            println!("{} imported {}", ui::green("✓"), p);
        }
        println!(
            "eq-import: {} profile(s) from {} → review, then run the `speaker-eq` step.",
            paths.len(),
            cfg.repo
        );
    }
    let gc = manifest::effective_git(&ft.git, &None);
    after_repo_change(&home, &gc, &format!("eq-import: {} profile(s)", paths.len()));
    Ok(())
}

fn cmd_install(
    machine: Option<String>,
    dry_run: bool,
    packages_only: bool,
    yes: bool,
    json: bool,
) -> Result<()> {
    let home = find_home_pulling()?;
    let ft = manifest::load_fleet(&home)?;
    let m = machine::resolve(&ft, machine.as_deref())?;

    // A live install with an *explicit* name that isn't this host is a footgun:
    // temper converges the LOCAL machine, so it would apply the named machine's
    // spec to whatever box you're on. Allow it (you may mean it — e.g. imaging a
    // renamed box), but confirm first. Read-only/dry-run paths never gate.
    if !dry_run && machine.is_some() {
        let host = machine::hostname();
        let is_this_host = host.as_deref().is_some_and(|h| m.name.eq_ignore_ascii_case(h));
        if !is_this_host {
            let host_label = host.as_deref().unwrap_or("an unknown hostname");
            let warn = format!(
                "installing as '{}', but this machine is '{}' — temper converges the \
                 LOCAL box, so this applies {}'s spec here, not to a remote '{}'",
                m.name, host_label, m.name, m.name
            );
            if yes {
                eprintln!("{} {warn} (--yes)", ui::yellow("⚠"));
            } else if json {
                anyhow::bail!("{warn}; pass --yes to confirm");
            } else {
                eprintln!("{} {warn}", ui::yellow("⚠"));
                if !prompt_no("proceed anyway?") {
                    println!("aborted — nothing changed.");
                    return Ok(());
                }
            }
        }
    }

    let vars = manifest::effective_vars(&ft.vars, &m);
    let r = plan::run_install(&home, &m, &vars, &ft.brew.trust, dry_run, packages_only)?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "machine": m.name, "packages": r.packages,
                "changed": r.steps_changed, "total": r.steps_total,
                "reboot": r.reboot, "dry_run": dry_run, "packages_only": packages_only,
                "skipped": r.skipped
            })
        );
    } else if packages_only {
        let verb = if dry_run { "would converge" } else { "converged" };
        println!("install-missing {}: {verb} {} declared package(s), config skipped", m.name, r.packages);
        if r.reboot {
            println!("  ⚠ reboot required (rpm-ostree layered a package)");
        }
    } else {
        let verb = if dry_run { "would apply" } else { "applied" };
        println!(
            "install {}: {} package(s), {verb} {} of {} config step(s)",
            m.name, r.packages, r.steps_changed, r.steps_total
        );
        announce_skipped(&r.skipped);
        if r.reboot {
            println!("  ⚠ reboot required (rpm-ostree layered a package)");
        }
    }
    Ok(())
}

/// Loudly report steps skipped by a failed `when` presence gate (Principle #6).
fn announce_skipped(skipped: &[String]) {
    for s in skipped {
        println!("  {} skipped: {s} absent", ui::yellow("⚠"));
    }
}

fn cmd_update(json: bool) -> Result<()> {
    let home = find_home_pulling()?;
    let ft = manifest::load_fleet(&home)?;
    let m = machine::resolve(&ft, None)?;
    let vars = manifest::effective_vars(&ft.vars, &m);
    let r = plan::run_update(&home, &m, &vars, &ft.brew.trust)?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "machine": m.name, "packages": r.packages,
                "reapplied": r.steps_changed, "total": r.steps_total,
                "skipped": r.skipped
            })
        );
    } else {
        println!(
            "update {}: upgraded {} package set, re-applied {} of {} always-step(s)",
            m.name, r.packages, r.steps_changed, r.steps_total
        );
        announce_skipped(&r.skipped);
    }
    Ok(())
}

fn cmd_drift(machine: Option<String>, json: bool) -> Result<()> {
    let home = find_home_pulling()?;
    let ft = manifest::load_fleet(&home)?;
    let m = machine::resolve(&ft, machine.as_deref())?;
    let vars = manifest::effective_vars(&ft.vars, &m);
    let items = plan::run_drift(&home, &m, &vars, &ft.ignore)?;
    let out_of_sync = items.iter().filter(|f| !f.ok).count();

    if json {
        let arr: Vec<_> = items
            .iter()
            .map(|f| {
                serde_json::json!({
                    "app": f.app, "kind": f.kind, "target": f.target,
                    "ok": f.ok, "status": f.status,
                })
            })
            .collect();
        let rem: Vec<_> = plan::remediations(&items)
            .iter()
            .map(|r| serde_json::json!({ "label": r.label, "command": r.command }))
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "machine": m.name, "out_of_sync": out_of_sync,
                "items": arr, "remediation": rem
            })
        );
    } else {
        render_drift(&m.name, &items);
    }
    Ok(())
}

/// Human drift view: grouped by app, drift surfaced first (red), fully-in-sync
/// apps collapsed to one green line, status-only items (manual / unavailable /
/// no-drift-check / profile) called out separately so they read as neither
/// green nor red. `--json` never reaches here, so ANSI is safe (and gated on a
/// real tty by `ui`).
fn render_drift(machine: &str, items: &[plan::Finding]) {
    use std::collections::HashMap;

    // Group findings by app, preserving first-seen order.
    let mut order: Vec<&str> = Vec::new();
    let mut groups: HashMap<&str, Vec<&plan::Finding>> = HashMap::new();
    for f in items {
        if !groups.contains_key(f.app.as_str()) {
            order.push(f.app.as_str());
        }
        groups.entry(f.app.as_str()).or_default().push(f);
    }

    println!("{} {}\n", ui::dim("drift ·"), ui::bold(machine));

    let mut clean_apps: Vec<&str> = Vec::new();
    let mut drifted_groups = 0usize;
    for app in &order {
        let g = &groups[app];
        let drifted: Vec<&&plan::Finding> = g.iter().filter(|f| !f.ok).collect();
        if drifted.is_empty() {
            // Collapse to the in-sync line only if something was actually
            // verified — an app that is *entirely* status-only belongs solely
            // in the status-only line, not counted as "in sync".
            if g.iter().any(|f| f.ok && !f.status_only()) {
                clean_apps.push(app);
            }
            continue;
        }
        drifted_groups += 1;
        println!("  {}", ui::bold(app));
        for f in &drifted {
            println!(
                "    {} {:<32} {} {}",
                ui::red("✗"),
                f.target,
                ui::yellow(&f.status),
                ui::dim(&format!("[{}]", f.kind)),
            );
        }
        let in_sync = g.len() - drifted.len();
        if in_sync > 0 {
            println!("    {}", ui::dim(&format!("… {in_sync} more in sync")));
        }
    }

    let status_only: Vec<&plan::Finding> = items.iter().filter(|f| f.status_only()).collect();

    if !clean_apps.is_empty() {
        if drifted_groups > 0 {
            println!();
        }
        println!(
            "  {} {}",
            ui::green(&format!("✓ {} app(s) in sync:", clean_apps.len())),
            ui::dim(&clean_apps.join(", ")),
        );
    }
    if !status_only.is_empty() {
        // Show the reason for actionable ones (an `unavailable` — e.g. a missing
        // secret or an absent backend tool); keep boring ones (no-drift-check /
        // manual) compact as `app:kind`.
        let labels: Vec<String> = status_only
            .iter()
            .map(|f| {
                if f.status.starts_with("unavailable") {
                    format!("{}:{} ({})", f.app, f.kind, f.status)
                } else {
                    format!("{}:{}", f.app, f.kind)
                }
            })
            .collect();
        println!("  {} {}", ui::cyan("ℹ status-only:"), ui::dim(&labels.join(", ")));
    }

    // Footer — always carries the literal "<n> out of sync".
    let out = items.iter().filter(|f| !f.ok).count();
    let so = status_only.len();
    let ok = items.len() - out - so;
    println!();
    if out == 0 {
        println!(
            "  {} {}",
            ui::green("✓ all in sync"),
            ui::dim(&format!("· {ok} checks · 0 out of sync · {so} status-only")),
        );
    } else {
        println!(
            "  {} · {} · {}",
            ui::green(&format!("{ok} ok")),
            ui::red(&format!("{out} out of sync")),
            ui::dim(&format!("{so} status-only")),
        );
    }

    // "What to run next" — both directions out of the drift, RIS-style: a cyan
    // arrow + label with the exact command dimmed beneath it.
    let rem = plan::remediations(items);
    if !rem.is_empty() {
        println!("\n{}", ui::bold("Next steps"));
        for r in &rem {
            println!("  {} {}", ui::cyan("→"), r.label);
            println!("    {}", ui::dim(&r.command));
        }
    }
}

fn cmd_prune(dry_run: bool, json: bool) -> Result<()> {
    let home = find_home_pulling()?;
    let ft = manifest::load_fleet(&home)?;
    let m = machine::resolve(&ft, None)?;
    let extras = plan::run_prune(&home, &m, &ft.ignore, dry_run)?;
    if json {
        let arr: Vec<_> = extras
            .iter()
            .map(|(mgr, name)| serde_json::json!({ "manager": mgr.as_str(), "name": name }))
            .collect();
        println!(
            "{}",
            serde_json::json!({ "machine": m.name, "extras": arr, "dry_run": dry_run })
        );
    } else {
        for (mgr, name) in &extras {
            println!("  - {} {}", mgr.as_str(), name);
        }
        let tail = if dry_run {
            "(dry-run, nothing removed)"
        } else {
            "removed"
        };
        println!("prune {}: {} extra(s) {tail}", m.name, extras.len());
    }
    Ok(())
}

fn cmd_backup(machine: Option<String>, json: bool) -> Result<()> {
    let home = find_home_pulling()?;
    let ft = manifest::load_fleet(&home)?;
    let m = machine::resolve(&ft, machine.as_deref())?;
    let r = plan::run_backup(&home, &m)?;
    let dconf: Vec<String> = r.dconf.iter().map(|p| p.display().to_string()).collect();
    if json {
        println!(
            "{}",
            serde_json::json!({
                "machine": m.name, "brewfile": r.brewfile.display().to_string(),
                "dconf": dconf
            })
        );
    } else {
        println!("backup {}: dumped package state to {}", m.name, r.brewfile.display());
        for d in &dconf {
            println!("  dconf snapshot → {d}");
        }
    }
    let gc = manifest::effective_git(&ft.git, &m.git);
    let msg = format!("backup {}: Brewfile + {} dconf snapshot(s)", m.name, r.dconf.len());
    after_repo_change(&home, &gc, &msg);
    Ok(())
}

fn cmd_adopt(json: bool) -> Result<()> {
    let home = find_home_pulling()?;
    let ft = manifest::load_fleet(&home)?;
    let m = machine::resolve(&ft, None)?;
    let extras = plan::run_adopt(&home, &m, &ft.ignore)?;
    if json {
        let arr: Vec<_> = extras
            .iter()
            .map(|(mgr, name)| serde_json::json!({ "manager": mgr.as_str(), "name": name }))
            .collect();
        println!("{}", serde_json::json!({ "machine": m.name, "adoptable": arr }));
    } else if extras.is_empty() {
        println!("adopt {}: nothing to adopt — machine matches its spec", m.name);
    } else {
        println!("adopt {}: {} installed package(s) not in the spec:", m.name, extras.len());
        for (mgr, name) in &extras {
            println!("  {} \"{}\"", mgr.as_str(), name);
        }
        println!(
            "\nAdd the ones you want to a bundle or the machine's loose `packages`, \
             and the rest to `[ignore].<manager>` in temper.toml — or run \
             `temper reconcile` to add/drop them interactively."
        );
    }
    Ok(())
}

/// Interactive spec←machine reconcile. Under `--json` it previews the plan and
/// prompts for nothing.
fn cmd_reconcile(machine: Option<String>, json: bool) -> Result<()> {
    let home = find_home_pulling()?;
    let ft = manifest::load_fleet(&home)?;
    let m = machine::resolve(&ft, machine.as_deref())?;
    let plan = reconcile::plan(&home, &m, &ft.ignore)?;

    if json {
        let adds: Vec<_> = plan
            .adds
            .iter()
            .map(|a| serde_json::json!({ "manager": a.manager.as_str(), "name": a.name, "token": a.token }))
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "machine": m.name, "brewfile": plan.brewfile_rel,
                "adds": adds, "drops": plan.drops
            })
        );
        return Ok(());
    }

    if plan.adds.is_empty() && plan.drops.is_empty() {
        println!("reconcile {}: already in sync — nothing to absorb or drop.", m.name);
        return Ok(());
    }

    let bf_path = home.join(&plan.brewfile_rel);
    let original = std::fs::read_to_string(&bf_path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", bf_path.display()))?;

    // Missing → keep/drop (default KEEP).
    let mut chosen_drops = Vec::new();
    if !plan.drops.is_empty() {
        println!("\n{}", ui::bold("Declared in the Brewfile but not installed:"));
        for line in &plan.drops {
            if !prompt_yes(&format!("  keep {:?} in the Brewfile?", line.trim())) {
                chosen_drops.push(line.clone());
            }
        }
    }

    // Extras → add / (flatpak) ignore / skip (default SKIP).
    let mut chosen_adds = Vec::new();
    let mut chosen_ignores: Vec<String> = Vec::new(); // flatpak app ids
    if !plan.adds.is_empty() {
        println!("\n{}", ui::bold("Installed but not in the spec:"));
        for a in &plan.adds {
            match prompt_add(&a.token, a.is_flatpak) {
                AddChoice::Add => chosen_adds.push(a.token.clone()),
                AddChoice::Ignore => chosen_ignores.push(a.name.clone()),
                AddChoice::Skip => {}
            }
        }
    }

    if chosen_drops.is_empty() && chosen_adds.is_empty() && chosen_ignores.is_empty() {
        println!("\nNothing selected — nothing changed.");
        return Ok(());
    }

    // Preview.
    println!("\n{}", ui::bold("Proposed changes"));
    for t in &chosen_adds {
        println!("  {} {}  {}", ui::green("+"), t, ui::dim(&format!("→ {}", plan.brewfile_rel)));
    }
    for d in &chosen_drops {
        println!("  {} {}  {}", ui::red("-"), d.trim(), ui::dim(&format!("→ {}", plan.brewfile_rel)));
    }
    for name in &chosen_ignores {
        println!("  {} flatpak {}  {}", ui::yellow("~"), name, ui::dim("→ [ignore].flatpak in temper.toml"));
    }

    if !prompt_no("\napply these changes?") {
        println!("aborted — nothing changed.");
        return Ok(());
    }

    // Write the Brewfile + [ignore] edits THROUGH the journal, so `temper undo`
    // can revert a reconcile (it edits real folder files, so it's journalable).
    let mut jrnl = journal::Journal::begin();
    let new_bf = reconcile::brewfile_with_adds(
        &reconcile::brewfile_without(&original, &chosen_drops),
        &chosen_adds,
    );
    if new_bf != original {
        jrnl.record_write(&bf_path, Some(original.as_bytes()), new_bf.as_bytes())?;
        std::fs::write(&bf_path, &new_bf)
            .map_err(|e| anyhow::anyhow!("writing {}: {e}", bf_path.display()))?;
    }
    // [ignore] additions (comment-preserving).
    if !chosen_ignores.is_empty() {
        let tt_path = home.join("temper.toml");
        let before_tt = std::fs::read_to_string(&tt_path)?;
        let mut tt = before_tt.clone();
        for name in &chosen_ignores {
            tt = reconcile::append_ignore(&tt, "flatpak", name)?;
        }
        if tt != before_tt {
            jrnl.record_write(&tt_path, Some(before_tt.as_bytes()), tt.as_bytes())?;
            std::fs::write(&tt_path, tt)?;
        }
    }
    jrnl.commit()?;
    println!(
        "{} reconcile {}: {} added, {} dropped, {} ignored.",
        ui::green("✓"),
        m.name,
        chosen_adds.len(),
        chosen_drops.len(),
        chosen_ignores.len()
    );
    let gc = manifest::effective_git(&ft.git, &m.git);
    let msg = format!(
        "reconcile {}: +{} -{} ~{}",
        m.name,
        chosen_adds.len(),
        chosen_drops.len(),
        chosen_ignores.len()
    );
    after_repo_change(&home, &gc, &msg);
    Ok(())
}

/// Load dconf snapshots back into live dconf. Confirm-gated (clobbers live
/// desktop state); `--yes` or `--json` skips the prompt.
fn cmd_restore(machine: Option<String>, yes: bool, json: bool) -> Result<()> {
    let home = find_home_pulling()?;
    let ft = manifest::load_fleet(&home)?;
    let m = machine::resolve(&ft, machine.as_deref())?;

    if m.dconf.is_empty() {
        if json {
            println!("{}", serde_json::json!({ "machine": m.name, "restored": [] }));
        } else {
            println!("restore {}: no dconf snapshots declared for this machine.", m.name);
        }
        return Ok(());
    }

    if !yes && !json {
        println!("{}", ui::bold(&format!("restore {} — loads snapshots into LIVE dconf:", m.name)));
        for snap in &m.dconf {
            println!("  {} {}  {}", ui::cyan("→"), snap.path, ui::dim(&snap.file));
        }
        println!("{}", ui::yellow("This overwrites live desktop tweaks under those paths."));
        if !prompt_no("apply?") {
            println!("aborted — nothing changed.");
            return Ok(());
        }
    }

    let loaded = plan::run_restore(&home, &m)?;
    let paths: Vec<String> = loaded.iter().map(|p| p.display().to_string()).collect();
    if json {
        println!("{}", serde_json::json!({ "machine": m.name, "restored": paths }));
    } else {
        println!("{} restore {}: loaded {} snapshot(s).", ui::green("✓"), m.name, paths.len());
    }
    Ok(())
}

enum AddChoice {
    Add,
    Ignore,
    Skip,
}

/// Read a reply from stdin (flushing the prompt first), trimmed + lowercased.
fn read_reply() -> String {
    use std::io::Write;
    let _ = io::stdout().flush();
    let mut s = String::new();
    let _ = io::stdin().read_line(&mut s);
    s.trim().to_lowercase()
}

/// Like `read_reply` but case-preserving — for pasted paths, which are
/// case-sensitive and must not be lowercased.
fn read_line_raw() -> String {
    use std::io::Write;
    let _ = io::stdout().flush();
    let mut s = String::new();
    let _ = io::stdin().read_line(&mut s);
    s.trim().to_string()
}

/// `[Y/n]` — default yes.
fn prompt_yes(msg: &str) -> bool {
    print!("{msg} [Y/n] ");
    !read_reply().starts_with('n')
}

/// `[y/N]` — default no.
fn prompt_no(msg: &str) -> bool {
    print!("{msg} [y/N] ");
    read_reply().starts_with('y')
}

/// `[y/N]` (or `[y/N/i]` for flatpak) — default skip.
fn prompt_add(token: &str, flatpak: bool) -> AddChoice {
    if flatpak {
        print!("  add {token}? [y/N/i]  (i = add to [ignore]) ");
        let r = read_reply();
        if r.starts_with('y') {
            AddChoice::Add
        } else if r.starts_with('i') {
            AddChoice::Ignore
        } else {
            AddChoice::Skip
        }
    } else {
        print!("  add {token}? [y/N] ");
        if read_reply().starts_with('y') {
            AddChoice::Add
        } else {
            AddChoice::Skip
        }
    }
}

fn cmd_undo(run: Option<String>, list: bool, dry_run: bool, json: bool) -> Result<()> {
    if list {
        let runs = journal::list_runs()?;
        if json {
            println!("{}", serde_json::json!({ "runs": runs }));
        } else if runs.is_empty() {
            println!("undo: no revertible runs");
        } else {
            println!("revertible runs (newest first):");
            for r in &runs {
                println!("  {r}");
            }
        }
        return Ok(());
    }
    let (reverted, skipped) = journal::undo(run.as_deref(), dry_run)?;
    if json {
        println!(
            "{}",
            serde_json::json!({ "reverted": reverted, "skipped": skipped, "dry_run": dry_run })
        );
    } else {
        let suffix = if dry_run { " (dry-run)" } else { "" };
        println!("undo: reverted {reverted}, skipped {skipped}{suffix}");
    }
    Ok(())
}

/// The self-contained guide printed by `--llm`: command reference rendered from
/// clap, then the architecture + principles docs embedded at compile time.
fn llm_guide() -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "temper {} — {}\n\nThe same reference as `man temper`, laid out plainly \
         for LLM reading.\n\n",
        env!("CARGO_PKG_VERSION"),
        REPO_URL
    ));

    out.push_str("=== COMMAND REFERENCE ===\n\n");
    let mut cmd = Cli::command();
    out.push_str(&cmd.render_long_help().to_string());
    for sub in cmd.get_subcommands_mut() {
        if sub.is_hide_set() {
            continue;
        }
        out.push_str(&format!("\n--- temper {} ---\n", sub.get_name()));
        out.push_str(&sub.render_long_help().to_string());
    }

    // Authoritative, matches-the-parser content first, so an agent can both
    // OPERATE (command reference) and AUTHOR (schema) a temper folder.
    out.push_str("\n\n=== WORKFLOWS (how to OPERATE temper — the day-to-day loops) ===\n\n");
    out.push_str(include_str!("../../../WORKFLOWS.md"));
    out.push_str("\n\n=== MANIFEST SCHEMA (authoritative — matches the parser; unknown fields error) ===\n\n");
    out.push_str(include_str!("../../../SPEC.md"));
    out.push_str("\n\n=== README (overview + implementation status) ===\n\n");
    out.push_str(include_str!("../../../README.md"));
    // The design docs describe intent; the SCHEMA + STATUS above are what's real.
    out.push_str("\n\n=== ARCHITECTURE (design intent — trust SCHEMA + STATUS above for what's implemented) ===\n\n");
    out.push_str(include_str!("../../../ARCHITECTURE.md"));
    out.push_str("\n\n=== PRINCIPLES (design intent) ===\n\n");
    out.push_str(include_str!("../../../PRINCIPLES.md"));
    out
}
