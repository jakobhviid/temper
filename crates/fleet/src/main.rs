//! fleet — converge a machine to a declared spec kept in a folder of
//! human-readable files.
//!
//! This is the thin CLI layer; all logic lives in `fleet-core`. Each command
//! resolves the fleet-home folder + this machine's identity, calls into core,
//! and renders the result as a human summary or (`--json`) a machine-readable
//! document.
//!
//! Status: the `copy` vertical (install / drift / undo) is live end-to-end;
//! update / prune / backup / adopt are stubs pending later primitives.

use std::io;
use std::process::ExitCode;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;

use fleet_core::{discovery, journal, machine, manifest, plan};

const REPO_URL: &str = "https://github.com/jakobhviid/fleet";

#[derive(Parser)]
#[command(
    name = "fleet",
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
    /// Upgrade packages + re-apply managed config; install-if-missing only for
    /// the `ensure` allowlist. Does not add newly-declared apps wholesale.
    Update,
    /// Show what's out of sync (read-only): package set, managed files, keys,
    /// and assertions. Reports present-&-drifted vs absent-&-N/A.
    Drift {
        /// Machine name (default: resolved from hostname).
        machine: Option<String>,
    },
    /// Remove installed-but-not-declared packages (dependency-aware; honors the
    /// machine's `[ignore]` baseline). Journaled.
    Prune,
    /// Snapshot live machine state back into the folder (effective package set,
    /// dconf snapshot through the strip filter).
    Backup {
        /// Machine name (default: resolved from hostname).
        machine: Option<String>,
    },
    /// Interactively adopt machine reality into the spec: for each drifted
    /// extra, add it to a bundle, the machine's loose list, or `[ignore]`.
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
    /// Print the man page (roff).
    #[command(hide = true)]
    Man,
}

fn main() -> ExitCode {
    // `--llm` is a documentation flag like `--help`: works from anywhere, needs
    // no subcommand, so intercept it before clap enforces one.
    if std::env::args().skip(1).any(|a| a == "--llm") {
        print!("{}", llm_guide());
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
            clap_complete::generate(shell, &mut cmd, "fleet", &mut io::stdout());
        }
        Some(Cmd::Man) => {
            clap_mangen::Man::new(Cli::command()).render(&mut io::stdout())?;
        }
        Some(Cmd::Install { machine, dry_run }) => cmd_install(machine, dry_run, json)?,
        Some(Cmd::Drift { machine }) => cmd_drift(machine, json)?,
        Some(Cmd::Undo { dry_run, .. }) => cmd_undo(dry_run, json)?,
        Some(other) => {
            let verb = match other {
                Cmd::Update => "update",
                Cmd::Prune => "prune",
                Cmd::Backup { .. } => "backup",
                Cmd::Adopt => "adopt",
                _ => unreachable!(),
            };
            anyhow::bail!(
                "`fleet {verb}` is scaffolded but not implemented yet — \
                 primitives land incrementally (see README.md build sequence)."
            );
        }
    }
    Ok(())
}

fn cmd_install(machine: Option<String>, dry_run: bool, json: bool) -> Result<()> {
    let home = discovery::find_home()?;
    let ft = manifest::load_fleet(&home)?;
    let m = machine::resolve(&ft, machine.as_deref())?;
    let r = plan::run_install(&home, &m, &ft.vars, dry_run)?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "machine": m.name, "packages": r.packages,
                "changed": r.steps_changed, "total": r.steps_total, "dry_run": dry_run
            })
        );
    } else {
        let verb = if dry_run { "would apply" } else { "applied" };
        println!(
            "install {}: {} package(s), {verb} {} of {} config step(s)",
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

fn cmd_undo(dry_run: bool, json: bool) -> Result<()> {
    let (reverted, skipped) = journal::undo(dry_run)?;
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
        "fleet {} — {}\n\nThe same reference as `man fleet`, laid out plainly \
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
        out.push_str(&format!("\n--- fleet {} ---\n", sub.get_name()));
        out.push_str(&sub.render_long_help().to_string());
    }

    out.push_str("\n\n=== ARCHITECTURE ===\n\n");
    out.push_str(include_str!("../../../ARCHITECTURE.md"));
    out.push_str("\n\n=== PRINCIPLES ===\n\n");
    out.push_str(include_str!("../../../PRINCIPLES.md"));
    out
}
