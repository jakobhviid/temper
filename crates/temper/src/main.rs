//! temper — converge a machine to a declared spec kept in a folder of
//! human-readable files.
//!
//! This is the thin CLI layer; all logic lives in `temper-core`. Each command
//! resolves the temper-home folder + this machine's identity, calls into core,
//! and renders the result as a human summary or (`--json`) a machine-readable
//! document.
//!
//! Status: all verbs are live. Read-only paths (drift, dry-run, package probe)
//! are verified against a real machine; the writing paths of the platform
//! providers (dconf/defaults/gext/rpm-ostree, live package converge) await a VM.

use std::io;
use std::process::ExitCode;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;

use temper_core::{discovery, journal, machine, manifest, plan, reconcile, ui};

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
    /// Converge a machine to its spec — full: add missing packages, apply
    /// everything, run one-time setup. Defaults to this machine.
    Install {
        /// Machine name (default: resolved from hostname).
        machine: Option<String>,
        /// Show what would change without touching anything.
        #[arg(long)]
        dry_run: bool,
        /// Converge only packages (add missing; never remove), skipping the
        /// config-step phase — the additive, no-churn "install-missing" flow.
        #[arg(long)]
        packages_only: bool,
    },
    /// Re-apply the `always` config steps and upgrade declared packages
    /// (`brew upgrade` + `flatpak update`). Does not add newly-declared apps.
    Update,
    /// Show what's out of sync (read-only): package set, managed files, keys,
    /// and assertions. Reports present-&-drifted vs absent-&-N/A.
    Drift {
        /// Machine name (default: resolved from hostname).
        machine: Option<String>,
    },
    /// Remove installed-but-not-declared packages (dependency-aware; honors the
    /// machine's `[ignore]` baseline).
    Prune {
        /// List what would be removed without removing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Dump the machine's live package state to a Brewfile in the folder
    /// (`brew bundle dump` → machines/<name>/Brewfile). (dconf snapshot: Linux/VM.)
    Backup {
        /// Machine name (default: resolved from hostname).
        machine: Option<String>,
    },
    /// Report installed packages not in the spec (advisory) so you can add them
    /// to a bundle, the machine's loose list, or `[ignore]`. Non-mutating.
    Adopt,
    /// Interactively reconcile the machine's Brewfile with reality (spec←machine):
    /// absorb installed-but-undeclared extras, drop declared-but-absent entries,
    /// or route a flatpak extra to `[ignore]`. Edits only the machine's own
    /// Brewfile. `--json` previews the plan without prompting.
    Reconcile {
        /// Machine name (default: resolved from hostname).
        machine: Option<String>,
    },
    /// Load this machine's dconf snapshot(s) back into live dconf (spec→machine).
    /// Confirm-gated — it clobbers live desktop tweaks, so it is never part of
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
    /// Print a shell completion script.
    Completions {
        /// The shell to generate for.
        shell: Shell,
    },
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
        Some(Cmd::Install { machine, dry_run, packages_only }) => {
            cmd_install(machine, dry_run, packages_only, json)?
        }
        Some(Cmd::Update) => cmd_update(json)?,
        Some(Cmd::Drift { machine }) => cmd_drift(machine, json)?,
        Some(Cmd::Undo { run, list, dry_run }) => cmd_undo(run, list, dry_run, json)?,
        Some(Cmd::Prune { dry_run }) => cmd_prune(dry_run, json)?,
        Some(Cmd::Backup { machine }) => cmd_backup(machine, json)?,
        Some(Cmd::Adopt) => cmd_adopt(json)?,
        Some(Cmd::Reconcile { machine }) => cmd_reconcile(machine, json)?,
        Some(Cmd::Restore { machine, yes }) => cmd_restore(machine, yes, json)?,
    }
    Ok(())
}

fn cmd_install(
    machine: Option<String>,
    dry_run: bool,
    packages_only: bool,
    json: bool,
) -> Result<()> {
    let home = discovery::find_home()?;
    let ft = manifest::load_fleet(&home)?;
    let m = machine::resolve(&ft, machine.as_deref())?;
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
    let home = discovery::find_home()?;
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
    let home = discovery::find_home()?;
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
        let rem: Vec<_> = plan::remediations(&items, &m.name)
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
        let labels: Vec<String> = status_only
            .iter()
            .map(|f| format!("{}:{}", f.app, f.kind))
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
    let rem = plan::remediations(items, machine);
    if !rem.is_empty() {
        println!("\n{}", ui::bold("Next steps"));
        for r in &rem {
            println!("  {} {}", ui::cyan("→"), r.label);
            println!("    {}", ui::dim(&r.command));
        }
    }
}

fn cmd_prune(dry_run: bool, json: bool) -> Result<()> {
    let home = discovery::find_home()?;
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
    let home = discovery::find_home()?;
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
    Ok(())
}

fn cmd_adopt(json: bool) -> Result<()> {
    let home = discovery::find_home()?;
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
             and the rest to `[ignore].{}` in temper.toml.",
            "<manager>"
        );
    }
    Ok(())
}

/// Interactive spec←machine reconcile. Under `--json` it previews the plan and
/// prompts for nothing.
fn cmd_reconcile(machine: Option<String>, json: bool) -> Result<()> {
    let home = discovery::find_home()?;
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

    // Write the Brewfile (drops first, then adds).
    let new_bf = reconcile::brewfile_with_adds(
        &reconcile::brewfile_without(&original, &chosen_drops),
        &chosen_adds,
    );
    if new_bf != original {
        std::fs::write(&bf_path, &new_bf)
            .map_err(|e| anyhow::anyhow!("writing {}: {e}", bf_path.display()))?;
    }
    // Write [ignore] additions (comment-preserving).
    if !chosen_ignores.is_empty() {
        let tt_path = home.join("temper.toml");
        let mut tt = std::fs::read_to_string(&tt_path)?;
        for name in &chosen_ignores {
            tt = reconcile::append_ignore(&tt, "flatpak", name)?;
        }
        std::fs::write(&tt_path, tt)?;
    }
    println!(
        "{} reconcile {}: {} added, {} dropped, {} ignored.",
        ui::green("✓"),
        m.name,
        chosen_adds.len(),
        chosen_drops.len(),
        chosen_ignores.len()
    );
    Ok(())
}

/// Load dconf snapshots back into live dconf. Confirm-gated (clobbers live
/// desktop state); `--yes` or `--json` skips the prompt.
fn cmd_restore(machine: Option<String>, yes: bool, json: bool) -> Result<()> {
    let home = discovery::find_home()?;
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
    out.push_str("\n\n=== MANIFEST SCHEMA (authoritative — matches the parser; unknown fields error) ===\n\n");
    out.push_str(include_str!("../../../SPEC.md"));
    out.push_str("\n\n=== IMPLEMENTATION STATUS (what is built vs designed) ===\n\n");
    out.push_str(include_str!("../../../README.md"));
    // The design docs describe intent; the SCHEMA + STATUS above are what's real.
    out.push_str("\n\n=== ARCHITECTURE (design intent — trust SCHEMA + STATUS above for what's implemented) ===\n\n");
    out.push_str(include_str!("../../../ARCHITECTURE.md"));
    out.push_str("\n\n=== PRINCIPLES (design intent) ===\n\n");
    out.push_str(include_str!("../../../PRINCIPLES.md"));
    out
}
