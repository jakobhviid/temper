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

use temper_core::{discovery, journal, machine, manifest, plan};

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
        Some(Cmd::Install { machine, dry_run }) => cmd_install(machine, dry_run, json)?,
        Some(Cmd::Update) => cmd_update(json)?,
        Some(Cmd::Drift { machine }) => cmd_drift(machine, json)?,
        Some(Cmd::Undo { run, list, dry_run }) => cmd_undo(run, list, dry_run, json)?,
        Some(Cmd::Prune { dry_run }) => cmd_prune(dry_run, json)?,
        Some(Cmd::Backup { machine }) => cmd_backup(machine, json)?,
        Some(Cmd::Adopt) => cmd_adopt(json)?,
    }
    Ok(())
}

fn cmd_install(machine: Option<String>, dry_run: bool, json: bool) -> Result<()> {
    let home = discovery::find_home()?;
    let ft = manifest::load_fleet(&home)?;
    let m = machine::resolve(&ft, machine.as_deref())?;
    let r = plan::run_install(&home, &m, &ft.vars, &ft.brew.trust, dry_run)?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "machine": m.name, "packages": r.packages,
                "changed": r.steps_changed, "total": r.steps_total,
                "reboot": r.reboot, "dry_run": dry_run
            })
        );
    } else {
        let verb = if dry_run { "would apply" } else { "applied" };
        println!(
            "install {}: {} package(s), {verb} {} of {} config step(s)",
            m.name, r.packages, r.steps_changed, r.steps_total
        );
        if r.reboot {
            println!("  ⚠ reboot required (rpm-ostree layered a package)");
        }
    }
    Ok(())
}

fn cmd_update(json: bool) -> Result<()> {
    let home = discovery::find_home()?;
    let ft = manifest::load_fleet(&home)?;
    let m = machine::resolve(&ft, None)?;
    let r = plan::run_update(&home, &m, &ft.vars, &ft.brew.trust)?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "machine": m.name, "packages": r.packages,
                "reapplied": r.steps_changed, "total": r.steps_total
            })
        );
    } else {
        println!(
            "update {}: upgraded {} package set, re-applied {} of {} always-step(s)",
            m.name, r.packages, r.steps_changed, r.steps_total
        );
    }
    Ok(())
}

fn cmd_drift(machine: Option<String>, json: bool) -> Result<()> {
    let home = discovery::find_home()?;
    let ft = manifest::load_fleet(&home)?;
    let m = machine::resolve(&ft, machine.as_deref())?;
    let items = plan::run_drift(&home, &m, &ft.vars, &ft.ignore)?;
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
        println!(
            "{}",
            serde_json::json!({ "machine": m.name, "out_of_sync": out_of_sync, "items": arr })
        );
    } else {
        for f in &items {
            let mark = if f.ok { "✓" } else { "✗" };
            println!("  {mark} {:<20} {} [{}] ({})", f.status, f.target, f.kind, f.app);
        }
        println!(
            "drift {}: {} ok, {} out of sync",
            m.name,
            items.len() - out_of_sync,
            out_of_sync
        );
    }
    Ok(())
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
    let path = plan::run_backup(&home, &m)?;
    if json {
        println!(
            "{}",
            serde_json::json!({ "machine": m.name, "brewfile": path.display().to_string() })
        );
    } else {
        println!("backup {}: dumped package state to {}", m.name, path.display());
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
