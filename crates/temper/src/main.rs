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

use temper_core::{
    discovery, git, journal, machine, manifest, packages, plan, providers, reconcile, settings, ui,
};

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

    /// Show the underlying tools' full output (brew/flatpak/mas/gext/rpm-ostree)
    /// and stream `exec` scripts live. Runs are quiet by default — only real
    /// installs, changes, warnings, and errors are shown; an idempotent `exec`'s
    /// chatter is captured and surfaced only if the script fails, and every
    /// package converge/upgrade shows a spinner naming what it is working on
    /// right now instead of the package manager's own output (replayed in full if
    /// it fails). A tool's own "already up to date" verdict is never temper's, so
    /// it is not shown.
    #[arg(short = 'v', long, global = true)]
    verbose: bool,

    /// Force a `git pull` of the home before this run, even if `[git].auto_pull`
    /// is off (uses `--rebase` when `[git].auto_rebase`). Applies to any verb.
    #[arg(long, global = true, conflicts_with = "no_pull")]
    pull: bool,

    /// Skip the pre-run `git pull` for this run, even if `[git].auto_pull` is on
    /// (e.g. you're offline). Applies to any verb.
    #[arg(long, global = true)]
    no_pull: bool,

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
    #[command(alias = "upgrade")]
    Update,
    /// Show what's out of sync (read-only), with the commands to fix it.
    ///
    /// Read-only: package set, managed files, keys, and assertions. Ends with a
    /// "Next steps" summary — the exact command for each way out of the drift.
    Drift {
        /// Machine name (default: resolved from hostname).
        machine: Option<String>,
    },
    /// Remove installed-but-undeclared packages + untrust undeclared taps.
    ///
    /// Dependency-aware (a kept package's transitive deps are not flagged) and
    /// honors the machine's `[ignore]` baseline. Also `brew untrust`s any tap
    /// trusted on the machine but not in `[brew].trust` — the machine→spec
    /// mirror of `reconcile` absorbing a trusted tap. Confirms first.
    Prune {
        /// List what would be removed without removing anything.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt (removal is destructive; default asks).
        #[arg(long)]
        yes: bool,
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
    /// Pull the latest spec into the git-backed home (from anywhere).
    ///
    /// `git pull` in your temper-home — the pull-side counterpart to `save`, so
    /// you can grab a fleet change without hunting for where the folder lives or
    /// `cd`-ing into it. `--rebase` (or `[git].auto_rebase`) rebases instead of
    /// fast-forward-only. Reports how many commits landed, or that the spec was
    /// already current. A no-op that just says so if the home isn't a git repo.
    #[command(alias = "pull")]
    Refresh {
        /// Pull with `--rebase` instead of `--ff-only` (overrides `[git].auto_rebase`).
        #[arg(long)]
        rebase: bool,
    },
    /// Show the home's state + settings: path, git, resolved machine, update mode.
    ///
    /// Read-only "where do I stand" overview. Reads the fleet-level `[git]` and
    /// `[update]` settings (change them with `temper configure set`).
    Status,
    /// Get/set the home's scalar settings (`[git]` toggles, `[update].mode`).
    ///
    /// One validated key/value surface for the fleet-wide automation knobs, e.g.
    /// `temper configure set git.auto_push true` or `… set update.mode auto`.
    /// Run `temper configure keys` for the full list. Structured config
    /// (`[brew].trust`, `[ignore]`, `[vars]`, machines) is hand-edited or managed
    /// by `reconcile`/`prune`.
    Configure {
        #[command(subcommand)]
        action: ConfigureAction,
    },
    /// Print a shell completion script.
    Completions {
        /// The shell to generate for.
        shell: Shell,
    },
}

#[derive(Subcommand)]
enum ConfigureAction {
    /// Set a setting's value (writes temper.toml, comment-preserving).
    Set {
        /// The setting key (run `temper configure keys` for the list).
        #[arg(value_parser = setting_keys())]
        key: String,
        /// The value (bools: on/off; update.mode: off|warn|prompt|auto).
        value: String,
    },
    /// Print a setting's current value (bare — composes in scripts).
    Get {
        #[arg(value_parser = setting_keys())]
        key: String,
    },
    /// Reset a setting to its default (drop the override).
    Unset {
        #[arg(value_parser = setting_keys())]
        key: String,
    },
    /// List every setting with its current value.
    List,
    /// List every settable key with a one-line description.
    Keys,
}

/// The settable keys, as a clap value-set — validates the `key` argument AND
/// feeds shell completion (so `temper configure set <TAB>` lists every key).
/// Sourced from `settings::SETTINGS` so there's one source of truth.
fn setting_keys() -> clap::builder::PossibleValuesParser {
    clap::builder::PossibleValuesParser::new(settings::keys())
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
    let _ = JSON.set(cli.json);
    if let Err(e) = run(cli) {
        // A folder written by a NEWER temper gets a tailored path (and, per
        // `[update].mode`, an offer to self-update) instead of the raw parser
        // error. `handle_skew` may not return — on a taken update it re-execs.
        if let Some(nv) = e.downcast_ref::<manifest::NewerVersion>() {
            handle_skew(&nv.required, nv.mode, true, Some(&nv.parse_error));
            return ExitCode::FAILURE;
        }
        eprintln!("error: {e:#}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// The global `--json` flag, stashed for `handle_skew` (which fires from deep in
/// a command, or from `main`'s error path, and must stay silent/non-interactive
/// under `--json`). Set once at startup.
static JSON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// The per-run pull override from the global `--pull`/`--no-pull` flags:
/// `Some(true)` = force, `Some(false)` = skip, `None` = follow `[git].auto_pull`.
/// Set once in `run` (a CLI parses args once at startup) and read by
/// `find_home_pulling`, so the flags reach the pull without threading an extra
/// parameter through every verb.
static PULL_OVERRIDE: std::sync::OnceLock<Option<bool>> = std::sync::OnceLock::new();

fn run(cli: Cli) -> Result<()> {
    let json = cli.json;
    let verbose = cli.verbose;
    // Before any output: the core's live renderers (progress regions, per-item
    // lines) must know to stay off stdout so `--json` is one document.
    ui::set_json(json);
    let _ = PULL_OVERRIDE.set(match (cli.pull, cli.no_pull) {
        (true, _) => Some(true),
        (_, true) => Some(false),
        _ => None,
    });
    match cli.cmd {
        None => {
            Cli::command().print_help()?;
            println!();
        }
        Some(Cmd::Completions { shell }) => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "temper", &mut io::stdout());
        }
        Some(Cmd::Install {
            machine,
            dry_run,
            packages_only,
            yes,
        }) => cmd_install(machine, dry_run, packages_only, yes, json, verbose)?,
        Some(Cmd::Update) => cmd_update(json, verbose)?,
        Some(Cmd::Drift { machine }) => cmd_drift(machine, json)?,
        Some(Cmd::Undo { run, list, dry_run }) => cmd_undo(run, list, dry_run, json)?,
        Some(Cmd::Prune { dry_run, yes }) => cmd_prune(dry_run, yes, json)?,
        Some(Cmd::Backup { machine }) => cmd_backup(machine, json)?,
        Some(Cmd::Adopt) => cmd_adopt(json)?,
        Some(Cmd::Reconcile { machine }) => cmd_reconcile(machine, json)?,
        Some(Cmd::Restore { machine, yes }) => cmd_restore(machine, yes, json)?,
        Some(Cmd::EqImport) => cmd_eq_import(json)?,
        Some(Cmd::Save { message, no_push }) => cmd_save(message, no_push, json)?,
        Some(Cmd::Refresh { rebase }) => cmd_refresh(rebase, json)?,
        Some(Cmd::Status) => cmd_status(json)?,
        Some(Cmd::Configure { action }) => cmd_configure(action, json)?,
        Some(Cmd::Setup { dir }) => cmd_setup(dir, json)?,
    }
    Ok(())
}

/// Save a temper-home as the default (a saved pointer discovery reads).
/// Find the temper-home, pulling first when `[git].auto_pull` is on (so a run
/// works on the latest spec) — `--rebase` if `auto_rebase`, else `--ff-only`. A
/// pull failure only warns.
fn find_home_pulling() -> Result<std::path::PathBuf> {
    let home = discovery::find_home()?;
    // `--pull` forces a pull (honoring auto_rebase) even when auto_pull is off;
    // `--no-pull` skips it even when on; otherwise follow `[git]`.
    let mode = match PULL_OVERRIDE.get().copied().flatten() {
        Some(false) => manifest::PullMode::Off,
        Some(true) if manifest::peek_auto_rebase(&home) => manifest::PullMode::Rebase,
        Some(true) => manifest::PullMode::FastForward,
        None => manifest::peek_pull_mode(&home),
    };
    if mode != manifest::PullMode::Off {
        // A pull reaches the network, so it can take seconds — long enough that
        // silence reads as a hang at the very start of a run. The region names what
        // it is doing and is erased the moment it's done.
        //
        // It must also be strictly scoped: a version skew handled just below may
        // `exec` a freshly upgraded temper, and an exec never runs a destructor —
        // an open progress region would leave the terminal with its cursor hidden.
        let quiet = ui::json_mode();
        let pb = (!quiet).then(|| ui::spinner(&format!("pulling {}", home.display())));
        let outcome = git::pull(&home, mode == manifest::PullMode::Rebase);
        if let Some(pb) = pb {
            pb.finish_and_clear();
        }
        match outcome {
            // Silence when nothing landed: the spec was already current, and the
            // run is about the machine, not about git.
            git::Pull::UpToDate | git::Pull::NotRepo => {}
            git::Pull::Updated(n) if !quiet => {
                let s = if n == 1 { "commit" } else { "commits" };
                println!("  {} spec updated ({n} {s})", ui::green("✓"));
            }
            git::Pull::Updated(_) => {}
            git::Pull::Warn(w) => eprintln!(
                "{} couldn't pull — working on a possibly-stale spec: {w}",
                ui::yellow("⚠")
            ),
        }
    }
    Ok(home)
}

/// Load the fleet manifest, layering the newer-temper check over the core parse.
/// A parse *failure* caused by a version skew surfaces as `manifest::NewerVersion`
/// (handled in `main`); a *successful* parse of a folder that a newer temper
/// nonetheless wrote still gets the `[update]` treatment here — outdated is
/// outdated — but the command carries on afterwards (it parsed, so it works).
fn load_fleet(home: &std::path::Path) -> Result<manifest::TemperToml> {
    let ft = manifest::load_fleet(home)?;
    if let Ok(src) = std::fs::read_to_string(home.join("temper.toml")) {
        if let Some(stamp) = manifest::peek_version_stamp(&src) {
            let mode = manifest::peek_update_mode(&src);
            if mode != manifest::UpdateMode::Off && manifest::version_is_newer(&stamp, manifest::VERSION)
            {
                // Not blocked (it parsed) — offer/auto per mode, then continue.
                handle_skew(&stamp, mode, false, None);
            }
        }
    }
    Ok(ft)
}

/// Explain a "written by a newer temper" skew and act on `[update].mode`. Shared
/// by the blocked path (parse failed — `main`) and the unblocked one (parse ok —
/// `load_fleet`). May not return: on a taken Homebrew upgrade it re-execs the
/// fresh binary. `mode` is never `Off` here (callers gate on it).
fn handle_skew(
    required: &str,
    mode: manifest::UpdateMode,
    blocked: bool,
    parse_error: Option<&str>,
) {
    let running = manifest::VERSION;
    eprintln!(
        "{} this temper-home was written by temper {} — you're running {running}.",
        ui::yellow("⚠"),
        ui::bold(required)
    );
    if let Some(pe) = parse_error {
        eprintln!("  it uses something this version can't parse:");
        eprintln!("    {}", ui::dim(pe));
    }

    let json = JSON.get().copied().unwrap_or(false);
    let brew = installed_via_brew();
    let already_tried = std::env::var_os("TEMPER_SELF_UPDATED").is_some();
    // Only actually run the upgrade when we can: a Homebrew install, not already
    // retried this run, and not under --json (no interactive/noisy work there).
    let do_update = brew
        && !already_tried
        && !json
        && match mode {
            manifest::UpdateMode::Auto => true,
            manifest::UpdateMode::Prompt => {
                io::stdin().is_terminal()
                    && prompt_yes(&format!(
                        "\nUpdate temper now via Homebrew ({})?",
                        ui::bold("brew upgrade temper")
                    ))
            }
            _ => false, // warn (off never reaches here)
        };

    if do_update {
        match self_update() {
            Ok(()) => reexec_after_update(), // replaces this process on success
            Err(e) => eprintln!("{} self-update failed: {e:#}", ui::yellow("⚠")),
        }
    }

    // Didn't (or couldn't) update — leave the user a way forward.
    if already_tried {
        eprintln!(
            "\n{} already updated once this run — temper {} may not be on Homebrew yet.",
            ui::cyan("ⓘ"),
            required
        );
    } else if brew {
        eprintln!(
            "\nUpgrade with: {}",
            ui::bold("brew update && brew upgrade temper -y")
        );
    } else {
        eprintln!("\nUpgrade temper (see {REPO_URL}) to match the folder.");
    }
    if blocked {
        eprintln!("{} this version can't read the folder until then.", ui::dim("·"));
    }
}

/// Whether the running temper is a Homebrew install (macOS **or** Linuxbrew): its
/// symlink-resolved path sits under a `/Cellar/`, and `brew` is on PATH to run
/// the upgrade.
fn installed_via_brew() -> bool {
    let under_cellar = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map(|p| p.to_string_lossy().contains("/Cellar/"))
        .unwrap_or(false);
    under_cellar && brew_on_path()
}

fn brew_on_path() -> bool {
    std::process::Command::new("brew")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `brew update && brew upgrade temper -y`, inheriting stdio so the user sees
/// Homebrew's progress. `-y` skips Homebrew's newer confirmation prompt.
fn self_update() -> Result<()> {
    use std::process::Command;
    eprintln!("{} brew update …", ui::cyan("→"));
    if !Command::new("brew").arg("update").status()?.success() {
        anyhow::bail!("`brew update` failed");
    }
    eprintln!("{} brew upgrade temper …", ui::cyan("→"));
    if !Command::new("brew")
        .args(["upgrade", "temper", "-y"])
        .status()?
        .success()
    {
        anyhow::bail!("`brew upgrade temper` failed");
    }
    Ok(())
}

/// Re-run the original command with the freshly-upgraded binary (found via PATH,
/// since the old Cellar path is gone after the upgrade). A `TEMPER_SELF_UPDATED`
/// sentinel stops a loop if the required version isn't on Homebrew yet. Only
/// returns if it can't exec.
///
/// **Invariant: no live progress region may be open here.** `exec` replaces the
/// process image, so no destructor runs — an unfinished indicatif region would
/// leave the user's terminal with a hidden cursor and no way to know why. Every
/// region in this binary is finished before the call that can reach this point
/// (the pre-run pull clears its own; the phase checklists start later). The
/// self-update itself deliberately keeps the terminal: `brew upgrade temper -y`
/// *is* the operation at that moment, not chatter about someone else's run, and
/// capturing a bottle download would only turn it into dead air.
fn reexec_after_update() {
    eprintln!("{} updated — re-running…\n", ui::green("✓"));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let args: Vec<String> = std::env::args().skip(1).collect();
        let err = std::process::Command::new("temper")
            .args(&args)
            .env("TEMPER_SELF_UPDATED", "1")
            .exec();
        eprintln!(
            "{} couldn't re-run automatically ({err}) — re-run your command.",
            ui::yellow("⚠")
        );
    }
    #[cfg(not(unix))]
    eprintln!("updated — re-run your command.");
}

/// After a spec-writing verb, either auto-commit (per `[git]`) or hint. A no-op
/// on a non-git home. All output goes to stderr so `--json` stdout stays clean.
fn after_repo_change(home: &std::path::Path, gc: &manifest::GitConfig, auto_msg: &str) {
    if !git::is_repo(home) {
        return; // dormant on a non-git folder
    }
    if gc.auto_commit {
        match git::save(home, auto_msg, gc.auto_push, gc.auto_rebase) {
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
    } else {
        remind_if_dirty(home, gc);
    }
}

/// Nudge to run `temper save` when the git-backed home has uncommitted spec
/// changes. A no-op on a non-git home, when reminders are off, or when
/// auto-commit handles committing instead. Every verb that reads or applies a
/// spec calls this (not just the spec-writing verbs), so any hand-edit sitting
/// uncommitted is surfaced whichever command you happen to run. Goes to stderr
/// so `--json` stdout stays clean.
fn remind_if_dirty(home: &std::path::Path, gc: &manifest::GitConfig) {
    if !git::is_repo(home) || gc.auto_commit || !gc.remind || !git::is_dirty(home) {
        return;
    }
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

/// Commit (and push) the home's pending spec changes.
fn cmd_save(message: Option<String>, no_push: bool, json: bool) -> Result<()> {
    let home = discovery::find_home()?;
    if !git::is_repo(&home) {
        if json {
            println!(
                "{}",
                serde_json::json!({ "saved": false, "reason": "not a git repo" })
            );
            return Ok(());
        }
        anyhow::bail!("{} is not a git repo — nothing to save", home.display());
    }
    let msg = message.unwrap_or_else(|| git::message_from_changes(&home));
    let r = git::save(&home, &msg, !no_push, manifest::peek_auto_rebase(&home))?;
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

/// Pull the latest spec into the home — the pull-side counterpart to `save`, run
/// from anywhere (temper resolves the folder, so you never `cd` to it). Explicit,
/// so it pulls regardless of `[git].auto_pull`; strategy follows `--rebase` /
/// `[git].auto_rebase`. A non-git home just reports that (nothing to pull).
fn cmd_refresh(rebase: bool, json: bool) -> Result<()> {
    let home = discovery::find_home()?;
    if !git::is_repo(&home) {
        if json {
            println!(
                "{}",
                serde_json::json!({ "refreshed": false, "reason": "not a git repo" })
            );
        } else {
            println!(
                "{} is not a git repo — nothing to refresh (syncing a Nextcloud/USB/plain \
                 folder is that tool's job, not temper's).",
                home.display()
            );
        }
        return Ok(());
    }
    let rebase = rebase || manifest::peek_auto_rebase(&home);
    let pb = (!json).then(|| ui::spinner(&format!("pulling {}", home.display())));
    let outcome = git::pull(&home, rebase);
    if let Some(pb) = pb {
        pb.finish_and_clear();
    }
    // `refresh` is the one verb whose deliverable IS the pull, so here "nothing
    // new" is the answer to the question asked — not a stray verdict mid-converge.
    let (ok, pulled, warning) = match outcome {
        git::Pull::UpToDate => (true, Some(0), None),
        git::Pull::Updated(n) => (true, Some(n), None),
        git::Pull::Warn(w) => (false, None, Some(w)),
        git::Pull::NotRepo => (false, None, Some("not a git repo".into())), // unreachable
    };
    if json {
        println!(
            "{}",
            serde_json::json!({
                "refreshed": ok, "commits": pulled,
                "home": home.display().to_string(), "warning": warning
            })
        );
    } else if ok {
        match pulled {
            Some(0) | None => println!(
                "{} {} is already current — nothing new in the spec.",
                ui::green("✓"),
                home.display()
            ),
            Some(n) => {
                let s = if n == 1 { "commit" } else { "commits" };
                println!(
                    "{} refreshed {} ({n} {s})",
                    ui::green("✓"),
                    home.display()
                );
            }
        }
    } else if let Some(w) = warning {
        eprintln!(
            "{} couldn't refresh {}: {w}",
            ui::yellow("⚠"),
            home.display()
        );
    }
    Ok(())
}

/// Read-only "where do I stand" overview: home path + git state, the resolved
/// machine, the fleet `[git]` settings, and the `[update]` self-update policy.
fn cmd_status(json: bool) -> Result<()> {
    let home = discovery::find_home()?;
    let is_repo = git::is_repo(&home);
    // load_fleet (wrapper) is skew-aware; a version skew surfaces here too.
    let ft = load_fleet(&home)?;
    let machine = machine::resolve(&ft, None).ok();
    // `configure`-managed fleet settings, as a key→value map.
    let sets: std::collections::BTreeMap<&str, String> = settings::list(&home).into_iter().collect();
    let brew = installed_via_brew();
    // A `[machine.git]` override means the fleet `[git]` shown here isn't the
    // whole story for this machine — say so rather than mislead.
    let machine_git_override = machine.as_ref().map(|m| m.git.is_some()).unwrap_or(false);

    if json {
        println!(
            "{}",
            serde_json::json!({
                "home": home.display().to_string(),
                "git_repo": is_repo,
                "git_status": is_repo.then(|| git::status_line(&home)),
                "machine": machine.as_ref().map(|m| serde_json::json!({
                    "name": m.name, "os": m.os, "role": m.role
                })),
                "settings": sets,
                "machine_git_override": machine_git_override,
                "homebrew": brew,
            })
        );
        return Ok(());
    }

    let git_state = if is_repo {
        git::status_line(&home)
    } else {
        ui::dim("not a git repo — git automation dormant").to_string()
    };
    println!("{:<9}{}  {}", ui::bold("home:"), home.display(), ui::dim(&format!("({git_state})")));
    match &machine {
        Some(m) => {
            let role = m.role.as_deref().map(|r| format!(", {r}")).unwrap_or_default();
            println!("{:<9}{}  {}", ui::bold("machine:"), m.name, ui::dim(&format!("({}{role})", m.os)));
        }
        None => println!("{:<9}{}", ui::bold("machine:"), ui::dim("unresolved (name it, or check [[machine]])")),
    }
    let g = |k: &str| sets.get(k).map(String::as_str).unwrap_or("?");
    let override_note = if machine_git_override {
        ui::dim("  (this machine overrides [git] via [machine.git])").to_string()
    } else {
        String::new()
    };
    println!(
        "{:<9}remind={} auto_commit={} auto_push={} auto_pull={} auto_rebase={}{}",
        ui::bold("git:"),
        g("git.remind"),
        g("git.auto_commit"),
        g("git.auto_push"),
        g("git.auto_pull"),
        g("git.auto_rebase"),
        override_note,
    );
    println!(
        "{:<9}mode={}  {}",
        ui::bold("update:"),
        g("update.mode"),
        ui::dim(&format!("(Homebrew install: {})", if brew { "yes" } else { "no" }))
    );
    println!("{}", ui::dim("change settings with: temper configure set <key> <value>  ·  keys: temper configure keys"));
    Ok(())
}

/// Get/set/unset/list the home's scalar settings (`[git]` toggles, `[update].mode`).
fn cmd_configure(action: ConfigureAction, json: bool) -> Result<()> {
    let home = discovery::find_home()?;
    match action {
        ConfigureAction::Set { key, value } => {
            let display = settings::set(&home, &key, &value)?;
            if json {
                println!("{}", serde_json::json!({ "key": key, "value": display }));
            } else {
                println!("{} {} = {}", ui::green("✓"), key, ui::bold(&display));
            }
        }
        ConfigureAction::Get { key } => {
            let v = settings::get(&home, &key)?;
            if json {
                println!("{}", serde_json::json!({ "key": key, "value": v }));
            } else {
                println!("{v}"); // bare — composes in scripts
            }
        }
        ConfigureAction::Unset { key } => {
            settings::unset(&home, &key)?;
            if json {
                println!("{}", serde_json::json!({ "unset": key }));
            } else {
                println!("{} unset {} (back to default)", ui::green("✓"), key);
            }
        }
        ConfigureAction::List => {
            let items = settings::list(&home);
            if json {
                let map: std::collections::BTreeMap<_, _> = items.into_iter().collect();
                println!("{}", serde_json::json!(map));
            } else {
                for (k, v) in items {
                    println!("  {k} = {v}");
                }
            }
        }
        ConfigureAction::Keys => {
            if json {
                let arr: Vec<_> = settings::SETTINGS
                    .iter()
                    .map(|s| serde_json::json!({ "key": s.key, "description": s.desc }))
                    .collect();
                println!("{}", serde_json::json!(arr));
            } else {
                for s in settings::SETTINGS {
                    println!("  {:<16} {}", s.key, ui::dim(s.desc));
                }
            }
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
            candidates
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
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
    let ft = load_fleet(&home)?;
    let cfg = ft.eq_import.ok_or_else(|| {
        anyhow::anyhow!(
            "no [eq_import] in temper.toml — add `repo = \"...\"` (and optional `dest`) to import"
        )
    })?;
    let written = temper_core::eq_import::run(&home, &cfg)?;
    let paths: Vec<String> = written.iter().map(|p| p.display().to_string()).collect();
    if json {
        println!(
            "{}",
            serde_json::json!({ "repo": cfg.repo, "imported": paths })
        );
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
    after_repo_change(
        &home,
        &gc,
        &format!("eq-import: {} profile(s)", paths.len()),
    );
    Ok(())
}

fn cmd_install(
    machine: Option<String>,
    dry_run: bool,
    packages_only: bool,
    yes: bool,
    json: bool,
    verbose: bool,
) -> Result<()> {
    let home = find_home_pulling()?;
    let ft = load_fleet(&home)?;
    let m = machine::resolve(&ft, machine.as_deref())?;

    // A live install with an *explicit* name that isn't this host is a footgun:
    // temper converges the LOCAL machine, so it would apply the named machine's
    // spec to whatever box you're on. Allow it (you may mean it — e.g. imaging a
    // renamed box), but confirm first. Read-only/dry-run paths never gate.
    if !dry_run && machine.is_some() {
        let host = machine::hostname();
        let is_this_host = host
            .as_deref()
            .is_some_and(|h| m.name.eq_ignore_ascii_case(h));
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
    let r = plan::run_install(
        &home,
        &m,
        &vars,
        &ft.brew.trust,
        dry_run,
        packages_only,
        verbose,
    )?;
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
        let verb = if dry_run {
            "would converge"
        } else {
            "converged"
        };
        println!(
            "install-missing {}: {verb} {} declared package(s), config skipped",
            m.name, r.packages
        );
        if r.reboot {
            println!("  ⚠ reboot required (rpm-ostree layered a package)");
        }
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
    remind_if_dirty(&home, &manifest::effective_git(&ft.git, &m.git));
    Ok(())
}

// Steps skipped by a failed `when` gate are announced *live* by the step phase's
// checklist (Principle #6, now naming the step rather than only its probe), so
// there is no after-the-fact replay here. `--json` still carries `skipped`.

fn cmd_update(json: bool, verbose: bool) -> Result<()> {
    let home = find_home_pulling()?;
    let ft = load_fleet(&home)?;
    let m = machine::resolve(&ft, None)?;
    let vars = manifest::effective_vars(&ft.vars, &m);
    let r = plan::run_update(&home, &m, &vars, &ft.brew.trust, verbose)?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "machine": m.name, "packages": r.packages,
                // What the run actually changed, beside what the machine declares.
                "upgraded": r.upgraded,
                "reapplied": r.steps_changed, "total": r.steps_total,
                "skipped": r.skipped
            })
        );
    } else {
        // The package half of the line reports an effect. "upgraded 210 package
        // set" used to recite the *declared* count, so a run that changed nothing
        // and a run that upgraded a dozen packages printed the same number.
        let pkgs = match r.upgraded {
            _ if r.packages == 0 => "no packages declared".to_string(),
            Some(0) | None => "packages already current".to_string(),
            Some(1) => "upgraded 1 package".to_string(),
            Some(n) => format!("upgraded {n} packages"),
        };
        println!(
            "update {}: {pkgs}, re-applied {} of {} always-step(s)",
            m.name, r.steps_changed, r.steps_total
        );
    }
    remind_if_dirty(&home, &manifest::effective_git(&ft.git, &m.git));
    Ok(())
}

fn cmd_drift(machine: Option<String>, json: bool) -> Result<()> {
    let home = find_home_pulling()?;
    let ft = load_fleet(&home)?;
    let m = machine::resolve(&ft, machine.as_deref())?;
    let vars = manifest::effective_vars(&ft.vars, &m);
    let items = plan::run_drift(&home, &m, &vars, &ft.ignore, &ft.brew.trust)?;
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
    remind_if_dirty(&home, &manifest::effective_git(&ft.git, &m.git));
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

    // Measured across every row this view may print, before any of it prints — the
    // findings are all in hand here, exactly as the step phase has its plan in hand.
    // `44` caps the target column so one long `setkey` key can't push status and
    // kind off the screen; the target (column 0) is what gives way when narrow.
    let rows: Vec<Vec<String>> = items
        .iter()
        .filter(|f| !f.ok)
        .map(|f| {
            vec![
                f.target.clone(),
                f.status.clone(),
                format!("[{}]", f.kind),
            ]
        })
        .collect();
    let cols = ui::Columns::measure(&rows, 6, &[44, 0, 0], 0);

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
            // Same column machinery the step phase uses, so the two views line up
            // instead of each guessing a width — this was a hard-coded `{:<32}`,
            // which every `~/.config/…:key` target overran. `parts` keeps the
            // padding measured on plain text while each cell is still coloured.
            let cells = cols.parts(&[&f.target, &f.status, &format!("[{}]", f.kind)]);
            let mut line = format!("    {} ", ui::red("✗"));
            for (i, (cell, pad)) in cells.iter().enumerate() {
                line.push_str(&match i {
                    1 => ui::yellow(cell),
                    2 => ui::dim(cell),
                    _ => cell.clone(),
                });
                line.push_str(&" ".repeat(*pad));
            }
            println!("{line}");
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
        println!(
            "  {} {}",
            ui::cyan("ℹ status-only:"),
            ui::dim(&labels.join(", "))
        );
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

fn cmd_prune(dry_run: bool, yes: bool, json: bool) -> Result<()> {
    let home = find_home_pulling()?;
    let ft = load_fleet(&home)?;
    let m = machine::resolve(&ft, None)?;
    let gc = manifest::effective_git(&ft.git, &m.git);
    // Inner body has several early returns; the closure lets the dirty-spec nudge
    // fire once on every one of them (below), regardless of which path we take.
    let result = (|| -> Result<()> {
        // Compute the plan WITHOUT removing anything, so we can preview + confirm.
        let prune_plan = plan::run_prune(&home, &m, &ft.ignore, &ft.brew.trust)?;

        if json {
            // No tty to confirm on: JSON is a preview unless `--yes` explicitly opts
            // into the (destructive) removal.
            let removed = yes && !dry_run && !prune_plan.is_empty();
            if removed {
                plan::commit_prune(&home, &m, &prune_plan)?;
            }
            let arr: Vec<_> = prune_plan
                .packages
                .iter()
                .map(|(mgr, name)| serde_json::json!({ "manager": mgr.as_str(), "name": name }))
                .collect();
            println!(
                "{}",
                serde_json::json!({
                    "machine": m.name, "extras": arr, "untrust": prune_plan.untrust,
                    "removed": removed
                })
            );
            return Ok(());
        }

        // Resolve mas ids → app names for a legible preview (lazily; only when a
        // mas extra is present), mirroring `drift`.
        let mas_names = if prune_plan
            .packages
            .iter()
            .any(|(m, _)| *m == packages::Manager::Mas)
        {
            providers::mas_names()
        } else {
            std::collections::BTreeMap::new()
        };
        for (mgr, name) in &prune_plan.packages {
            match mgr {
                packages::Manager::Mas => match mas_names.get(name) {
                    Some(app) => println!("  - mas \"{app}\" (id {name})"),
                    None => println!("  - mas {name}"),
                },
                _ => println!("  - {} {}", mgr.as_str(), name),
            }
        }
        for tap in &prune_plan.untrust {
            println!("  - untrust {tap}");
        }
        if prune_plan.is_empty() {
            println!("prune {}: nothing to remove.", m.name);
            return Ok(());
        }
        if dry_run {
            println!(
                "prune {}: {} item(s) (dry-run, nothing removed)",
                m.name,
                prune_plan.len()
            );
            return Ok(());
        }
        // Removal is destructive (dependency-aware uninstall + untrust) — confirm.
        if !yes
            && !prompt_no(&format!(
                "remove {} item(s) listed above? this uninstalls packages and untrusts taps",
                prune_plan.len()
            ))
        {
            println!("aborted — nothing removed.");
            return Ok(());
        }
        plan::commit_prune(&home, &m, &prune_plan)?;
        println!("prune {}: {} item(s) removed", m.name, prune_plan.len());
        Ok(())
    })();
    remind_if_dirty(&home, &gc);
    result
}

fn cmd_backup(machine: Option<String>, json: bool) -> Result<()> {
    let home = find_home_pulling()?;
    let ft = load_fleet(&home)?;
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
        println!(
            "backup {}: dumped package state to {}",
            m.name,
            r.brewfile.display()
        );
        for d in &dconf {
            println!("  dconf snapshot → {d}");
        }
    }
    let gc = manifest::effective_git(&ft.git, &m.git);
    let msg = format!(
        "backup {}: Brewfile + {} dconf snapshot(s)",
        m.name,
        r.dconf.len()
    );
    after_repo_change(&home, &gc, &msg);
    Ok(())
}

fn cmd_adopt(json: bool) -> Result<()> {
    let home = find_home_pulling()?;
    let ft = load_fleet(&home)?;
    let m = machine::resolve(&ft, None)?;
    let extras = plan::run_adopt(&home, &m, &ft.ignore)?;
    if json {
        let arr: Vec<_> = extras
            .iter()
            .map(|(mgr, name)| serde_json::json!({ "manager": mgr.as_str(), "name": name }))
            .collect();
        println!(
            "{}",
            serde_json::json!({ "machine": m.name, "adoptable": arr })
        );
    } else if extras.is_empty() {
        println!(
            "adopt {}: nothing to adopt — machine matches its spec",
            m.name
        );
    } else {
        println!(
            "adopt {}: {} installed package(s) not in the spec:",
            m.name,
            extras.len()
        );
        for (mgr, name) in &extras {
            println!("  {} \"{}\"", mgr.as_str(), name);
        }
        println!(
            "\nAdd the ones you want to a bundle or the machine's loose `packages`, \
             and the rest to `[ignore].<manager>` in temper.toml — or run \
             `temper reconcile` to add/drop them interactively."
        );
    }
    remind_if_dirty(&home, &manifest::effective_git(&ft.git, &m.git));
    Ok(())
}

/// Interactive spec←machine reconcile. Under `--json` it previews the plan and
/// prompts for nothing.
fn cmd_reconcile(machine: Option<String>, json: bool) -> Result<()> {
    let home = find_home_pulling()?;
    let ft = load_fleet(&home)?;
    let m = machine::resolve(&ft, machine.as_deref())?;
    let plan = reconcile::plan(&home, &m, &ft.ignore, &ft.brew.trust)?;

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
                "adds": adds, "drops": plan.drops,
                "trust_adds": plan.trust_adds, "trust_drops": plan.trust_drops
            })
        );
        return Ok(());
    }

    if plan.adds.is_empty()
        && plan.drops.is_empty()
        && plan.trust_adds.is_empty()
        && plan.trust_drops.is_empty()
    {
        println!(
            "reconcile {}: already in sync — nothing to absorb or drop.",
            m.name
        );
        return Ok(());
    }

    let bf_path = home.join(&plan.brewfile_rel);
    let original = std::fs::read_to_string(&bf_path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", bf_path.display()))?;

    // Missing → keep/drop (default KEEP).
    let mut chosen_drops = Vec::new();
    if !plan.drops.is_empty() {
        println!(
            "\n{}",
            ui::bold("Declared in the Brewfile but not installed:")
        );
        for line in &plan.drops {
            if !prompt_yes(&format!("  keep `{}` in the Brewfile?", line.trim())) {
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

    // Tap-trust drops (declared but not trusted) → keep/drop, default KEEP
    // (keeping lets `install`/`update` re-trust it — dropping abandons the tap).
    let mut chosen_trust_drops: Vec<String> = Vec::new();
    if !plan.trust_drops.is_empty() {
        println!(
            "\n{}",
            ui::bold("Declared in [brew].trust but not currently trusted:")
        );
        for tap in &plan.trust_drops {
            if !prompt_yes(&format!("  keep `{tap}` in [brew].trust?")) {
                chosen_trust_drops.push(tap.clone());
            }
        }
    }

    // Tap-trust adds (trusted but not declared) → add / ignore / skip, default
    // SKIP — mirrors the flatpak extra choice (add to spec, or `[ignore].tap`).
    let mut chosen_trust_adds: Vec<String> = Vec::new();
    let mut chosen_tap_ignores: Vec<String> = Vec::new();
    if !plan.trust_adds.is_empty() {
        println!("\n{}", ui::bold("Trusted but not in [brew].trust:"));
        for tap in &plan.trust_adds {
            match prompt_add(tap, true) {
                AddChoice::Add => chosen_trust_adds.push(tap.clone()),
                AddChoice::Ignore => chosen_tap_ignores.push(tap.clone()),
                AddChoice::Skip => {}
            }
        }
    }

    if chosen_drops.is_empty()
        && chosen_adds.is_empty()
        && chosen_ignores.is_empty()
        && chosen_trust_adds.is_empty()
        && chosen_trust_drops.is_empty()
        && chosen_tap_ignores.is_empty()
    {
        println!("\nNothing selected — nothing changed.");
        return Ok(());
    }

    // Preview.
    println!("\n{}", ui::bold("Proposed changes"));
    for t in &chosen_adds {
        println!(
            "  {} {}  {}",
            ui::green("+"),
            t,
            ui::dim(&format!("→ {}", plan.brewfile_rel))
        );
    }
    for d in &chosen_drops {
        println!(
            "  {} {}  {}",
            ui::red("-"),
            d.trim(),
            ui::dim(&format!("→ {}", plan.brewfile_rel))
        );
    }
    for name in &chosen_ignores {
        println!(
            "  {} flatpak {}  {}",
            ui::yellow("~"),
            name,
            ui::dim("→ [ignore].flatpak in temper.toml")
        );
    }
    for tap in &chosen_trust_adds {
        println!(
            "  {} trust {}  {}",
            ui::green("+"),
            tap,
            ui::dim("→ [brew].trust in temper.toml")
        );
    }
    for tap in &chosen_trust_drops {
        println!(
            "  {} trust {}  {}",
            ui::red("-"),
            tap,
            ui::dim("→ [brew].trust in temper.toml")
        );
    }
    for tap in &chosen_tap_ignores {
        println!(
            "  {} trust {}  {}",
            ui::yellow("~"),
            tap,
            ui::dim("→ [ignore].tap in temper.toml")
        );
    }

    if !prompt_no("\napply these changes?") {
        println!("aborted — nothing changed.");
        return Ok(());
    }

    // Write the Brewfile + [ignore] edits THROUGH the journal, so `temper undo`
    // can revert a reconcile (it edits real folder files, so it's journalable).
    let mut jrnl = journal::Journal::begin();
    // Absorb adds/drops, then canonically re-sort so new entries land in their
    // group (taps → brews → casks → mas …) instead of tacked onto the end.
    let new_bf = reconcile::sort_brewfile(&reconcile::brewfile_with_adds(
        &reconcile::brewfile_without(&original, &chosen_drops),
        &chosen_adds,
    ));
    if new_bf != original {
        jrnl.record_write(&bf_path, Some(original.as_bytes()), new_bf.as_bytes())?;
        std::fs::write(&bf_path, &new_bf)
            .map_err(|e| anyhow::anyhow!("writing {}: {e}", bf_path.display()))?;
    }
    // temper.toml edits (comment-preserving): [ignore].flatpak absorbs, plus the
    // tap-trust reconcile — [brew].trust add/drop and [ignore].tap absorb.
    let tt_edits = !chosen_ignores.is_empty()
        || !chosen_trust_adds.is_empty()
        || !chosen_trust_drops.is_empty()
        || !chosen_tap_ignores.is_empty();
    if tt_edits {
        let tt_path = home.join("temper.toml");
        let before_tt = std::fs::read_to_string(&tt_path)?;
        let mut tt = before_tt.clone();
        for name in &chosen_ignores {
            tt = reconcile::append_ignore(&tt, "flatpak", name)?;
        }
        for tap in &chosen_trust_adds {
            tt = reconcile::append_trust(&tt, tap)?;
        }
        for tap in &chosen_trust_drops {
            tt = reconcile::remove_trust(&tt, tap)?;
        }
        for tap in &chosen_tap_ignores {
            tt = reconcile::append_ignore(&tt, "tap", tap)?;
        }
        // Stamp the temper that wrote this file, so a skew is later distinguishable
        // from a genuine parse error (monotonic — never lowers a newer stamp).
        tt = manifest::stamp_version(&tt)?;
        if tt != before_tt {
            jrnl.record_write(&tt_path, Some(before_tt.as_bytes()), tt.as_bytes())?;
            std::fs::write(&tt_path, tt)?;
        }
    }
    jrnl.commit()?;
    let added = chosen_adds.len() + chosen_trust_adds.len();
    let dropped = chosen_drops.len() + chosen_trust_drops.len();
    let ignored = chosen_ignores.len() + chosen_tap_ignores.len();
    println!(
        "{} reconcile {}: {} added, {} dropped, {} ignored.",
        ui::green("✓"),
        m.name,
        added,
        dropped,
        ignored
    );
    let gc = manifest::effective_git(&ft.git, &m.git);
    let msg = format!("reconcile {}: +{} -{} ~{}", m.name, added, dropped, ignored);
    after_repo_change(&home, &gc, &msg);
    Ok(())
}

/// Load dconf snapshots back into live dconf. Confirm-gated (clobbers live
/// desktop state); `--yes` or `--json` skips the prompt.
fn cmd_restore(machine: Option<String>, yes: bool, json: bool) -> Result<()> {
    let home = find_home_pulling()?;
    let ft = load_fleet(&home)?;
    let m = machine::resolve(&ft, machine.as_deref())?;
    let gc = manifest::effective_git(&ft.git, &m.git);
    // Inner body has several early returns; the closure lets the dirty-spec nudge
    // fire once on every one of them (below), regardless of which path we take.
    let result = (|| -> Result<()> {
        if m.dconf.is_empty() {
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "machine": m.name, "restored": [] })
                );
            } else {
                println!(
                    "restore {}: no dconf snapshots declared for this machine.",
                    m.name
                );
            }
            return Ok(());
        }

        if !yes && !json {
            println!(
                "{}",
                ui::bold(&format!(
                    "restore {} — loads snapshots into LIVE dconf:",
                    m.name
                ))
            );
            for snap in &m.dconf {
                println!("  {} {}  {}", ui::cyan("→"), snap.path, ui::dim(&snap.file));
            }
            println!(
                "{}",
                ui::yellow("This overwrites live desktop tweaks under those paths.")
            );
            if !prompt_no("apply?") {
                println!("aborted — nothing changed.");
                return Ok(());
            }
        }

        let loaded = plan::run_restore(&home, &m)?;
        let paths: Vec<String> = loaded.iter().map(|p| p.display().to_string()).collect();
        if json {
            println!(
                "{}",
                serde_json::json!({ "machine": m.name, "restored": paths })
            );
        } else {
            println!(
                "{} restore {}: loaded {} snapshot(s).",
                ui::green("✓"),
                m.name,
                paths.len()
            );
        }
        Ok(())
    })();
    remind_if_dirty(&home, &gc);
    result
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
        print!("  add `{token}`? [y/N/i]  (i = add to [ignore]) ");
        let r = read_reply();
        if r.starts_with('y') {
            AddChoice::Add
        } else if r.starts_with('i') {
            AddChoice::Ignore
        } else {
            AddChoice::Skip
        }
    } else {
        print!("  add `{token}`? [y/N] ");
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
    out.push_str("\n\n=== PATTERNS (how to COMPOSE primitives for common problem shapes) ===\n\n");
    out.push_str(include_str!("../../../PATTERNS.md"));
    out.push_str("\n\n=== README (overview + implementation status) ===\n\n");
    out.push_str(include_str!("../../../README.md"));
    // The design docs describe intent; the SCHEMA + STATUS above are what's real.
    out.push_str("\n\n=== ARCHITECTURE (design intent — trust SCHEMA + STATUS above for what's implemented) ===\n\n");
    out.push_str(include_str!("../../../ARCHITECTURE.md"));
    out.push_str("\n\n=== PRINCIPLES (design intent) ===\n\n");
    out.push_str(include_str!("../../../PRINCIPLES.md"));
    out
}
