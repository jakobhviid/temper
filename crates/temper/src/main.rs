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
    dconf, discovery, git, journal, machine, manifest, packages, plan, providers, reconcile,
    settings, ui,
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
    /// Scaffold THIS machine into the folder, then seed it from live state.
    ///
    /// Adds a `[[machine]]` block (creating `temper.toml` if the folder has
    /// none), wires up `brewfiles/<name>`, and absorbs the machine's current
    /// package + desktop state into it — `reconcile --current-state-wins
    /// --include-trust` under the hood. Previews and confirms once; journaled.
    ///
    /// This is "put this machine in the folder". To point temper at *which*
    /// folder to use, see `setup`.
    Init {
        /// Machine name. Omit to infer it from this host's hostname.
        name: Option<String>,
        /// "desktop" | "server". Omit to be asked.
        #[arg(long)]
        role: Option<String>,
        /// Skip the confirmations.
        #[arg(long)]
        yes: bool,
    },
    /// Capture the machine's dconf subtree(s) into the folder.
    ///
    /// Each declared `[[machine.dconf]]` is dumped through its `strip` filter to
    /// its file. Spec←machine and wholesale — the mirror of `restore-dconf`, and
    /// the blunt sibling of a per-key `reconcile`. Journaled.
    ///
    /// **dconf only.** Packages and app config are not part of it: those are
    /// `reconcile` and hand-authored recipes respectively.
    #[command(name = "snapshot-dconf", aliases = ["snapshot-gnome", "snapshot"])]
    Snapshot {
        /// Machine name (default: resolved from hostname).
        machine: Option<String>,
    },
    /// List installed packages not in the spec (advisory, non-mutating).
    ///
    /// Report installed extras so you can add them to a bundle, the machine's
    /// loose list, or `[ignore]` — or run `reconcile` to act on them per-item.
    Adopt,
    /// List every path this spec has retired, and whether it is still present.
    ///
    /// The review sweep for tombstones. A `retire` entry never expires on a date
    /// — behaviour that changed with the wall clock would mean two machines on
    /// one commit doing different things — so it stays until someone decides it
    /// has done its job. This is how you decide: oldest-looking first, with the
    /// ones still doing work marked.
    Retired,
    /// Interactively absorb extras / drop missing entries (spec←machine).
    ///
    /// Reconcile the machine's Brewfile with reality: add installed-but-
    /// undeclared extras, drop declared-but-absent entries, or route a flatpak
    /// extra to `[ignore]`. Edits only the machine's own Brewfile; `--json`
    /// previews the plan without prompting.
    Reconcile {
        /// Machine name (default: resolved from hostname).
        machine: Option<String>,
        /// Take the machine's current state for every item, without prompting.
        ///
        /// Adds every extra (taps included), drops every declared-but-absent
        /// entry, and absorbs every changed desktop key. Machine-scope only:
        /// tap-trust lives in temper.toml at FLEET scope, so it is reported but
        /// not touched unless you pass --include-trust. Still previews and
        /// confirms once.
        ///
        /// Converge before you absorb: on a machine that hasn't run `install`
        /// yet, "declared but not installed" and "not wanted" look identical, so
        /// this would strip the spec down to what happens to be present. Check
        /// `temper drift` for missing packages first.
        #[arg(long, alias = "csw")]
        current_state_wins: bool,
        /// Also record taps this machine trusts into `[brew].trust`.
        ///
        /// Fleet-scope — it affects every machine, which is why it is opt-in.
        /// Adds only: a declared tap that isn't trusted here is never removed
        /// (that usually means this machine hasn't converged yet, not that the
        /// fleet is wrong), and is reported instead.
        #[arg(long, requires = "current_state_wins")]
        include_trust: bool,
        /// Skip the final confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Load dconf snapshot(s) back into live dconf (confirm-gated).
    ///
    /// spec→machine, the mirror of `snapshot-dconf`. Clobbers live desktop
    /// tweaks, so it is never part of `update`. Use after a reinstall, or to
    /// reset the desktop to the captured state.
    ///
    /// **dconf only.** It restores nothing else — packages come back with
    /// `install`.
    #[command(name = "restore-dconf", aliases = ["restore-gnome", "restore"])]
    Restore {
        /// Machine name (default: resolved from hostname).
        machine: Option<String>,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
        /// Show which snapshots would be loaded without touching dconf.
        #[arg(long)]
        dry_run: bool,
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
    /// drift after an `init`/`reconcile`/`snapshot`/`eq-import` or a hand edit. The commit
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
        Some(Cmd::Init { name, role, yes }) => cmd_init(name, role, yes, json)?,
        Some(Cmd::Snapshot { machine }) => cmd_snapshot(machine, json)?,
        Some(Cmd::Adopt) => cmd_adopt(json)?,
        Some(Cmd::Retired) => cmd_retired(json)?,
        Some(Cmd::Reconcile {
            machine,
            current_state_wins,
            include_trust,
            yes,
        }) => cmd_reconcile(machine, current_state_wins, include_trust, yes, json)?,
        Some(Cmd::Restore {
            machine,
            yes,
            dry_run,
        }) => cmd_restore(machine, yes, dry_run, json)?,
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
                println!("  {} spec updated ({n} {s})", ui::green(ui::g_ok()));
            }
            git::Pull::Updated(_) => {}
            git::Pull::Warn(w) => eprintln!(
                "{} couldn't pull — working on a possibly-stale spec: {w}",
                ui::yellow(ui::g_warn())
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
    // The glyph set is a manifest choice with a per-terminal override; set it as
    // soon as the manifest is known, before any renderer draws a marker.
    ui::set_icons(ft.ui.icons.as_deref());
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
        ui::yellow(ui::g_warn()),
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
            Err(e) => eprintln!("{} self-update failed: {e:#}", ui::yellow(ui::g_warn())),
        }
    }

    // Didn't (or couldn't) update — leave the user a way forward.
    if already_tried {
        eprintln!(
            "\n{} already updated once this run — temper {} may not be on Homebrew yet.",
            ui::cyan(ui::g_info()),
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
    eprintln!("{} brew update …", ui::cyan(ui::g_arrow()));
    if !Command::new("brew").arg("update").status()?.success() {
        anyhow::bail!("`brew update` failed");
    }
    eprintln!("{} brew upgrade temper …", ui::cyan(ui::g_arrow()));
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
    eprintln!("{} updated — re-running…\n", ui::green(ui::g_ok()));
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
            ui::yellow(ui::g_warn())
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
                    eprintln!("{} committed: {}", ui::green(ui::g_ok()), r.message);
                }
                if r.pushed {
                    eprintln!("{} pushed", ui::green(ui::g_ok()));
                }
                if let Some(w) = r.warning {
                    eprintln!("{} {w}", ui::yellow(ui::g_warn()));
                }
            }
            Err(e) => eprintln!("{} auto-commit failed: {e:#}", ui::yellow(ui::g_warn())),
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
        ui::cyan(ui::g_info()),
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
            println!("{} committed: {}", ui::green(ui::g_ok()), r.message);
        } else {
            println!("nothing to commit — the folder is clean.");
        }
        if r.pushed {
            println!("{} pushed", ui::green(ui::g_ok()));
        }
        if let Some(w) = r.warning {
            eprintln!("{} {w}", ui::yellow(ui::g_warn()));
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
                ui::green(ui::g_ok()),
                home.display()
            ),
            Some(n) => {
                let s = if n == 1 { "commit" } else { "commits" };
                println!(
                    "{} refreshed {} ({n} {s})",
                    ui::green(ui::g_ok()),
                    home.display()
                );
            }
        }
    } else if let Some(w) = warning {
        eprintln!(
            "{} couldn't refresh {}: {w}",
            ui::yellow(ui::g_warn()),
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
    // `set` and `unset` write temper.toml, so they owe the git hook every other
    // folder-writing verb fires. `get`/`list`/`keys` read only.
    let mut wrote: Option<String> = None;
    match action {
        ConfigureAction::Set { key, value } => {
            let display = settings::set(&home, &key, &value)?;
            wrote = Some(format!("configure: set {key}"));
            if json {
                println!("{}", serde_json::json!({ "key": key, "value": display }));
            } else {
                println!("{} {} = {}", ui::green(ui::g_ok()), key, ui::bold(&display));
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
            wrote = Some(format!("configure: unset {key}"));
            if json {
                println!("{}", serde_json::json!({ "unset": key }));
            } else {
                println!("{} unset {} (back to default)", ui::green(ui::g_ok()), key);
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
    if let Some(msg) = wrote {
        // Best-effort: a folder whose fleet config can't be loaded still had its
        // setting written, and a failed hook must not turn that into an error.
        if let Ok(ft) = load_fleet(&home) {
            let gc = match machine::resolve(&ft, None) {
                Ok(m) => manifest::effective_git(&ft.git, &m.git),
                Err(_) => manifest::effective_git(&ft.git, &None),
            };
            after_repo_change(&home, &gc, &msg);
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
        println!("{} temper home set to {}", ui::green(ui::g_ok()), target.display());
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
            println!("{} imported {}", ui::green(ui::g_ok()), p);
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
                eprintln!("{} {warn} (--yes)", ui::yellow(ui::g_warn()));
            } else if json {
                anyhow::bail!("{warn}; pass --yes to confirm");
            } else {
                eprintln!("{} {warn}", ui::yellow(ui::g_warn()));
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
        &manifest::effective_trust(&home, &ft.brew.trust, &m)?,
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
                "unrevertible": r.unrevertible,
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
        if !r.unrevertible.is_empty() {
            println!(
                "  {} {} change(s) `temper undo` cannot revert:",
                ui::yellow(ui::g_warn()),
                r.unrevertible.len()
            );
            for u in &r.unrevertible {
                println!("      {u}");
            }
        }
        if r.reboot {
            println!("  ! reboot required (rpm-ostree layered a package)");
        }
    } else {
        // "applied 11 of 44" read as "applied 11, left 33 alone" — the opposite of
        // what happened: all 44 were applied and 11 of them changed something. Both
        // numbers are worth having, so state each as what it is. A dry run applies
        // nothing at all, so there it *checked*, and the changed count is a forecast.
        let steps = if dry_run {
            format!(
                "checked {} config step(s), {} would change",
                r.steps_total, r.steps_changed
            )
        } else {
            // "ran" is reported apart from "changed": a checkless `exec` ran and
            // temper cannot observe whether it did anything, so folding it into
            // `changed` would claim an effect nobody measured — and would stop a
            // converged machine ever reporting zero.
            let ran = if r.steps_ran > 0 {
                format!(", {} ran (no drift-check)", r.steps_ran)
            } else {
                String::new()
            };
            format!(
                "applied {} config step(s), {} changed{ran}",
                r.steps_total, r.steps_changed
            )
        };
        println!("install {}: {} package(s), {steps}", m.name, r.packages);
        if !r.unrevertible.is_empty() {
            println!(
                "  {} {} change(s) `temper undo` cannot revert:",
                ui::yellow(ui::g_warn()),
                r.unrevertible.len()
            );
            for u in &r.unrevertible {
                println!("      {u}");
            }
        }
        if r.reboot {
            println!("  ! reboot required (rpm-ostree layered a package)");
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
    let r = plan::run_update(&home, &m, &vars, &manifest::effective_trust(&home, &ft.brew.trust, &m)?, verbose)?;
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
        // Same correction as `install`, plus a second one: these are not only
        // `always` steps — an `ensure` step whose target was missing is applied here
        // too, and calling that "re-applied" describes the wrong thing entirely,
        // since it had never been applied before.
        println!(
            "update {}: {pkgs}, re-applied {} step(s), {} changed",
            m.name, r.steps_total, r.steps_changed
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
    let items = plan::run_drift(&home, &m, &vars, &manifest::effective_ignore(&home, &ft.ignore, &m)?, &manifest::effective_trust(&home, &ft.brew.trust, &m)?)?;
    let out_of_sync = items.iter().filter(|f| !f.ok).count();

    if json {
        let arr: Vec<_> = items
            .iter()
            .map(|f| {
                serde_json::json!({
                    "app": f.app, "kind": f.kind, "target": f.target,
                    "ok": f.ok, "status": f.status, "detail": f.detail,
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
/// no-drift-check) called out separately so they read as neither green nor red.
/// `--json` never reaches here, so ANSI is safe (and gated on a real tty by `ui`).
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
            let mut line = format!("    {} ", ui::red(ui::g_bad()));
            for (i, (cell, pad)) in cells.iter().enumerate() {
                line.push_str(&match i {
                    1 => ui::yellow(cell),
                    2 => ui::dim(cell),
                    _ => cell.clone(),
                });
                line.push_str(&" ".repeat(*pad));
            }
            println!("{line}");
            // What actually disagreed, when the check can say — a bare
            // "drifted" is what made a dconf formatting bug take a hand-audit.
            if let Some(d) = &f.detail {
                println!("      {}", ui::dim(d));
            }
        }
        let in_sync = g.len() - drifted.len();
        if in_sync > 0 {
            println!("    {}", ui::dim(&format!("… {in_sync} more in sync")));
        }
    }

    // A `notice` is information the user should actually read ("reboot to apply
    // the staged update"), so it gets its own visible line rather than being
    // compressed into the terse status-only list with everything else.
    let notices: Vec<&plan::Finding> = items
        .iter()
        .filter(|f| f.status.starts_with("notice"))
        .collect();
    let status_only: Vec<&plan::Finding> = items
        .iter()
        .filter(|f| f.status_only() && !f.status.starts_with("notice"))
        .collect();

    if !notices.is_empty() {
        println!();
        for n in &notices {
            println!(
                "  {} {}",
                ui::cyan(ui::g_info()),
                n.status.strip_prefix("notice — ").unwrap_or(&n.status)
            );
        }
    }

    if !clean_apps.is_empty() {
        if drifted_groups > 0 {
            println!();
        }
        println!(
            "  {} {}",
            ui::green(&format!("{} {} app(s) in sync:", ui::g_ok(), clean_apps.len())),
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
            ui::cyan(&format!("{} status-only:", ui::g_info())),
            ui::dim(&labels.join(", "))
        );
    }

    // Footer — always carries the literal "<n> out of sync".
    let out = items.iter().filter(|f| !f.ok).count();
    let so = status_only.len();
    let nt = notices.len();
    let ok = items.len() - out - so - nt;
    // Notices are counted, not just printed: a number that doesn't add up is its
    // own small lie (Principle #6 — no silent caps).
    let notice_tail = if nt > 0 {
        format!(" · {nt} notice")
    } else {
        String::new()
    };
    println!();
    if out == 0 {
        println!(
            "  {} {}",
            ui::green(&format!("{} all in sync", ui::g_ok())),
            ui::dim(&format!(
                "· {ok} checks · 0 out of sync · {so} status-only{notice_tail}"
            )),
        );
    } else {
        println!(
            "  {} · {} · {}",
            ui::green(&format!("{ok} ok")),
            ui::red(&format!("{out} out of sync")),
            ui::dim(&format!("{so} status-only{notice_tail}")),
        );
    }

    // "What to run next" — both directions out of the drift, RIS-style: a cyan
    // arrow + label with the exact command dimmed beneath it.
    let rem = plan::remediations(items);
    if !rem.is_empty() {
        println!("\n{}", ui::bold("Next steps"));
        for r in &rem {
            println!("  {} {}", ui::cyan(ui::g_arrow()), r.label);
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
        let prune_plan = plan::run_prune(&home, &m, &manifest::effective_ignore(&home, &ft.ignore, &m)?, &manifest::effective_trust(&home, &ft.brew.trust, &m)?)?;

        if json {
            // No tty to confirm on: JSON is a preview unless `--yes` explicitly opts
            // into the (destructive) removal.
            let removed = yes && !dry_run && !prune_plan.is_empty();
            if removed {
                let _reboot = plan::commit_prune(&home, &m, &prune_plan)?;
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
                    "extensions": prune_plan.extensions,
                    "rpm_ostree": prune_plan.rpm_ostree,
                    "residue": prune_plan.residue,
                    "residue_edited": prune_plan.residue_edited,
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
        for uuid in &prune_plan.extensions {
            println!("  - uninstall extension {uuid}");
        }
        for pkg in &prune_plan.rpm_ostree {
            println!("  - un-layer rpm {pkg}");
        }
        for path in &prune_plan.residue {
            println!("  - remove {path} (deployed by a step the spec dropped)");
        }
        if !prune_plan.residue_edited.is_empty() && !json {
            println!(
                "  {} {} file(s) the spec no longer declares were EDITED since \
                 temper deployed them — reported, not removed:",
                ui::yellow(ui::g_warn()),
                prune_plan.residue_edited.len()
            );
            for path in &prune_plan.residue_edited {
                println!("      {path}");
            }
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
                "remove {} item(s) listed above? this uninstalls packages, GNOME extensions \
                 and flatpaks, untrusts taps, and un-layers rpms (a reboot applies that last one)",
                prune_plan.len()
            ))
        {
            println!("aborted — nothing removed.");
            return Ok(());
        }
        let reboot = plan::commit_prune(&home, &m, &prune_plan)?;
        println!("prune {}: {} item(s) removed", m.name, prune_plan.len());
        if reboot {
            println!("  ! reboot required (rpm-ostree staged a deployment)");
        }
        Ok(())
    })();
    remind_if_dirty(&home, &gc);
    result
}

/// Scaffold this machine into the folder, then seed it from live state.
///
/// Two steps, each journaled: the scaffold (temper.toml + the `[[machine]]`
/// block + an empty Brewfile), then the `--current-state-wins` seed. They are
/// separate journal runs, so a full rollback is two `temper undo`s.
fn cmd_init(name: Option<String>, role: Option<String>, yes: bool, json: bool) -> Result<()> {
    // Resolve the folder, or create one here. `find_home` needs a temper.toml,
    // so a brand-new folder can't be discovered — that's the case init exists
    // to bootstrap, and the cwd is the only place it could sensibly mean.
    let (home, fresh) = match discovery::find_home() {
        Ok(h) => (h, false),
        Err(_) => {
            // An explicit TEMPER_DIR names the folder even when it has no
            // manifest yet — that's precisely the folder being bootstrapped.
            // Otherwise the cwd is the only place `init` could sensibly mean.
            let cwd = match std::env::var("TEMPER_DIR") {
                Ok(d) => std::path::PathBuf::from(d),
                Err(_) => std::env::current_dir()?,
            };
            if !yes {
                if json {
                    anyhow::bail!(
                        "no temper folder found — run `temper init` from the folder you want \
                         to create, and pass --yes to confirm creating temper.toml there"
                    );
                }
                println!(
                    "{} no temper folder found.\n  {}",
                    ui::yellow(ui::g_warn()),
                    ui::dim(&format!("would create {}/temper.toml", cwd.display()))
                );
                if !prompt_no("create it here?") {
                    println!("aborted — nothing changed.");
                    return Ok(());
                }
            }
            (cwd, true)
        }
    };

    // Name: explicit, else this host's hostname (the same rule every other verb
    // uses to decide which machine it is talking about).
    let name = match name {
        Some(n) => n,
        None => machine::hostname().ok_or_else(|| {
            anyhow::anyhow!("could not infer a machine name from `hostname` — pass one explicitly")
        })?,
    };
    let os = if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    };
    let role = match role {
        Some(r) => Some(r),
        None if yes || json => None,
        None => {
            print!("role for `{name}`? [desktop/server, blank to omit] ");
            let r = read_reply();
            (!r.is_empty()).then_some(r)
        }
    };
    if let Some(r) = &role {
        if r != "desktop" && r != "server" {
            anyhow::bail!("role must be \"desktop\" or \"server\" (got \"{r}\")");
        }
    }

    let tt_path = home.join("temper.toml");
    let before = if fresh {
        String::new()
    } else {
        std::fs::read_to_string(&tt_path)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", tt_path.display()))?
    };
    // Refuses on a machine that already exists — rewriting a hand-authored block
    // would lose intent, and absorbing its state is `reconcile`'s job.
    let brewfile_rel = format!("brewfiles/{name}");
    let after = reconcile::append_machine(&before, &name, os, role.as_deref(), &brewfile_rel)?;
    let after = manifest::stamp_version(&after)?;

    let bf_path = home.join(&brewfile_rel);
    if json {
        println!(
            "{}",
            serde_json::json!({
                "home": home.display().to_string(), "machine": name, "os": os,
                "role": role, "brewfile": brewfile_rel, "created_manifest": fresh
            })
        );
    } else {
        println!(
            "\n{} {}",
            ui::bold("init"),
            ui::dim(&home.display().to_string())
        );
        println!("  {} machine {}  ({os}{})", ui::green("+"), name,
            role.as_ref().map(|r| format!(", {r}")).unwrap_or_default());
        println!("  {} {}", ui::green("+"), brewfile_rel);
        if fresh {
            println!("  {} temper.toml", ui::green("+"));
        }
    }
    if !yes && !json && !prompt_no("\nwrite this?") {
        println!("aborted — nothing changed.");
        return Ok(());
    }

    let mut jrnl = journal::Journal::begin();
    jrnl.record_write(
        &tt_path,
        (!fresh).then_some(before.as_bytes()),
        after.as_bytes(),
    )?;
    std::fs::write(&tt_path, &after)
        .map_err(|e| anyhow::anyhow!("writing {}: {e}", tt_path.display()))?;
    if !bf_path.exists() {
        if let Some(p) = bf_path.parent() {
            std::fs::create_dir_all(p)?;
        }
        jrnl.record_write(&bf_path, None, b"")?;
        std::fs::write(&bf_path, "")
            .map_err(|e| anyhow::anyhow!("writing {}: {e}", bf_path.display()))?;
    }
    jrnl.commit()?;

    if !json {
        println!(
            "\n{} scaffolded — seeding from this machine's current state…",
            ui::green(ui::g_ok())
        );
    }
    // The seed IS a reconcile: same planner, same writes, same journal, same
    // preview. init just answers every prompt with "the machine" and opts into
    // the fleet-scope tap-trust absorb, since establishing the spec is the point.
    let seeded = cmd_reconcile(Some(name.clone()), true, true, yes, json);

    // The scaffold is a folder write in its own right, and `reconcile` returns
    // early when there is nothing to absorb — so without this, `init` on an
    // already-converged machine would leave temper.toml + the Brewfile
    // uncommitted with no auto-commit and no dirty hint. Safe when clean: a
    // clean tree neither commits nor reminds.
    if let Ok(ft) = load_fleet(&home) {
        if let Ok(m) = machine::resolve(&ft, Some(&name)) {
            let gc = manifest::effective_git(&ft.git, &m.git);
            after_repo_change(&home, &gc, &format!("init {name}: scaffold machine"));
        }
    }
    seeded
}

/// Capture the machine's declared dconf subtrees into the folder — the
/// spec←machine mirror of `restore`, and the wholesale sibling of `reconcile`.
fn cmd_snapshot(machine: Option<String>, json: bool) -> Result<()> {
    let home = find_home_pulling()?;
    let ft = load_fleet(&home)?;
    let m = machine::resolve(&ft, machine.as_deref())?;

    if m.dconf.is_empty() {
        if json {
            println!(
                "{}",
                serde_json::json!({ "machine": m.name, "captured": [] })
            );
        } else {
            println!(
                "snapshot-dconf {}: no `[[machine.dconf]]` declared for this machine.",
                m.name
            );
        }
        return Ok(());
    }

    let written = plan::run_snapshot(&home, &m)?;
    let paths: Vec<String> = written.iter().map(|p| p.display().to_string()).collect();
    if json {
        println!(
            "{}",
            serde_json::json!({ "machine": m.name, "captured": paths })
        );
    } else {
        println!(
            "{} snapshot-dconf {}: captured {} subtree(s).",
            ui::green(ui::g_ok()),
            m.name,
            paths.len()
        );
        // Scope, said out loud: "captured 2 subtrees" invited "…so I'm done",
        // and a leftover drift report then read as this verb having failed.
        println!(
            "{}",
            ui::dim("  dconf only — packages and app config are not part of a snapshot.")
        );
        for p in &paths {
            println!("  → {p}");
        }
    }
    let gc = manifest::effective_git(&ft.git, &m.git);
    let msg = format!("snapshot-dconf {}: {} dconf subtree(s)", m.name, paths.len());
    after_repo_change(&home, &gc, &msg);
    Ok(())
}

fn cmd_retired(json: bool) -> Result<()> {
    let home = discovery::find_home()?;
    let ft = load_fleet(&home)?;
    let m = machine::resolve(&ft, None)?;
    let entries = manifest::effective_retire(&home, &m)?;
    let rows: Vec<(String, bool)> = entries
        .into_iter()
        .map(|p| {
            let present = manifest::expand_tilde(&p).exists();
            (p, present)
        })
        .collect();
    if json {
        let arr: Vec<_> = rows
            .iter()
            .map(|(p, present)| serde_json::json!({ "path": p, "present": present }))
            .collect();
        println!(
            "{}",
            serde_json::json!({ "machine": m.name, "retired": arr })
        );
        return Ok(());
    }
    if rows.is_empty() {
        println!("retired {}: nothing declared retired.", m.name);
        return Ok(());
    }
    println!("{}", ui::bold(&format!("retired · {}", m.name)));
    for (p, present) in &rows {
        if *present {
            println!("  {} {p}  still present — `temper prune` removes it", ui::red("✗"));
        } else {
            println!("  {} {p}", ui::dim("· gone"));
        }
    }
    let done = rows.iter().filter(|(_, p)| !*p).count();
    println!(
        "\n  {}",
        ui::dim(&format!(
            "{done} of {} have done their job — an entry nobody can still be \
             migrating from is worth deleting.",
            rows.len()
        ))
    );
    Ok(())
}

fn cmd_adopt(json: bool) -> Result<()> {
    let home = find_home_pulling()?;
    let ft = load_fleet(&home)?;
    let m = machine::resolve(&ft, None)?;
    let extras = plan::run_adopt(&home, &m, &manifest::effective_ignore(&home, &ft.ignore, &m)?)?;
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

/// Interactive spec←machine reconcile, or a non-interactive "the machine's
/// current state wins" absorb (`--current-state-wins`).
///
/// Under `--json` it previews the plan and prompts for nothing — it only applies
/// when the answers need no prompting *and* the confirm is waived
/// (`--current-state-wins --yes`), mirroring `prune`.
fn cmd_reconcile(
    machine: Option<String>,
    csw: bool,
    include_trust: bool,
    yes: bool,
    json: bool,
) -> Result<()> {
    let home = find_home_pulling()?;
    let ft = load_fleet(&home)?;
    let m = machine::resolve(&ft, machine.as_deref())?;
    let plan = reconcile::plan(&home, &m, &manifest::effective_ignore(&home, &ft.ignore, &m)?, &manifest::effective_trust(&home, &ft.brew.trust, &m)?)?;

    // Tap-trust is FLEET-scope (temper.toml, every machine) while everything
    // else here is machine-scope, so `--current-state-wins` leaves it alone
    // unless asked — but never silently: whatever is skipped is reported below.
    //
    // `--include-trust` absorbs only the ADDS. A trusted-but-undeclared tap is
    // real knowledge this machine has. A declared-but-untrusted one is almost
    // always the opposite — a machine that hasn't run `install` yet, most
    // acutely under `init`, where it has trusted nothing at all. Dropping on
    // that basis deletes a tap the rest of the fleet needs, so no single
    // machine's state ever removes one; that stays an interactive decision.
    // Nothing is skipped any more. Both tap-trust directions now land in the
    // machine's own `brew_trust`, which is exactly what `--csw` exists to
    // absorb — the old skip existed because an absorb had nowhere to go but the
    // FLEET list, and one machine must never write that unasked. `--include-trust`
    // keeps its meaning as the explicit opt-in that ALSO records the tap fleet-wide.
    let fleet_trust_writes = if include_trust { plan.trust_adds.len() } else { 0 };
    // Extensions are NOT tap-trust, and `--csw` takes both directions.
    //
    // Tap-trust drops are refused because `[brew].trust` is FLEET scope — one
    // machine's state must not delete a tap the rest of the fleet needs. A
    // machine's own `extensions` list is machine scope, which is precisely what
    // `--csw` exists to absorb, so borrowing the trust rule here would be
    // reasoning from the wrong precedent. It would also manufacture drift no
    // verb clears: refuse the drop and the next `drift` reports the extension
    // missing again, with `install` offered to put back the thing the user just
    // said they had removed.
    //
    // What makes this safe is not refusing the drop, it is refusing to *guess*:
    // `gext_machine_absent` returns nothing unless `gnome-extensions` actually
    // ran and answered, so absence is only ever acted on where it was observed.
    // And because the candidate set is the machine's OWN list, `init` — which
    // seeds via `--csw` on a machine block it just created empty — has nothing
    // to drop by construction.

    if json && !(csw && yes) {
        let adds: Vec<_> = plan
            .adds
            .iter()
            .map(|a| serde_json::json!({ "manager": a.manager.as_str(), "name": a.name, "token": a.token }))
            .collect();
        let dconf_plans: Vec<_> = plan
            .dconf
            .iter()
            .map(|d| {
                let keys: Vec<_> = d
                    .sections
                    .iter()
                    .flat_map(|(s, ds)| {
                        ds.iter().map(move |k| {
                            serde_json::json!({
                                "section": s, "key": k.key, "id": k.id(),
                                "status": k.status(), "change": dconf::describe(k),
                            })
                        })
                    })
                    .collect();
                serde_json::json!({ "name": d.name, "file": d.file_rel, "keys": keys })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "machine": m.name, "brewfile": plan.brewfile_rel,
                "adds": adds, "drops": plan.drops,
                "trust_adds": plan.trust_adds, "trust_drops": plan.trust_drops,
                // Every candidate the plan carries reaches this document, or a
                // `--json` consumer previews a reconcile that then changes
                // something it was never shown (Principle #8's `--json` clause).
                "gext_adds": plan.gext_adds, "gext_drops": plan.gext_drops,
                "package_drops": plan.package_drops,
                "rpm_adds": plan.rpm_adds, "rpm_drops": plan.rpm_drops,
                "remote_adds": plan.remote_adds, "remote_drops": plan.remote_drops,
                "fleet_trust_writes": fleet_trust_writes,
                "dconf": dconf_plans
            })
        );
        return Ok(());
    }

    // Every candidate the plan can carry is tested here. A field left out makes
    // its feature UNREACHABLE whenever it is the only drift present — which is
    // what happened to `gext_adds`: reconcile reported "nothing to absorb" and
    // returned, while `drift` listed the extensions in the same breath.
    if plan.adds.is_empty()
        && plan.drops.is_empty()
        && plan.trust_adds.is_empty()
        && plan.trust_drops.is_empty()
        && plan.gext_adds.is_empty()
        && plan.gext_drops.is_empty()
        && plan.package_drops.is_empty()
        && plan.rpm_adds.is_empty()
        && plan.rpm_drops.is_empty()
        && plan.remote_adds.is_empty()
        && plan.remote_drops.is_empty()
        && plan.dconf.is_empty()
    {
        // `--csw --yes` skips the JSON branch above, so this needs its own
        // guard: one unguarded line makes stdout unparseable (Principle #6b).
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "machine": m.name, "applied": false,
                    "added": 0, "dropped": 0, "ignored": 0, "dconf_keys": 0,
                    "fleet_trust_writes": fleet_trust_writes,
                })
            );
        } else {
            println!(
                "reconcile {}: nothing for reconcile to absorb or drop.",
                m.name
            );
        }
        return Ok(());
    }

    // The package half needs somewhere to write; without a `brewfile` it is
    // skipped (say so — silence would read as "no package drift") and only the
    // tap-trust and desktop halves run.
    let bf_path = plan.brewfile_rel.as_ref().map(|r| home.join(r));
    let original = match &bf_path {
        // A declared Brewfile that doesn't exist yet is the seed case — treat it
        // as empty, same as the planner does.
        Some(p) => std::fs::read_to_string(p).unwrap_or_default(),
        None => {
            if !json {
                println!(
                    "{}",
                    ui::dim(
                        "note: this machine declares no `brewfile`, so package reconcile is \
                         skipped — declare one to absorb packages here."
                    )
                );
            }
            String::new()
        }
    };

    // What gets absorbed. `--current-state-wins` answers every one of these
    // with "the machine", skipping the prompts; the final confirm still stands.
    let mut chosen_drops: Vec<String> = Vec::new();
    let mut chosen_adds: Vec<String> = Vec::new();
    // (ignore list, value) — every manager can be silenced now, not just flatpak.
    let mut chosen_ignores: Vec<(String, String)> = Vec::new();
    let mut chosen_trust_adds: Vec<String> = Vec::new();
    let mut chosen_trust_drops: Vec<String> = Vec::new();
    let mut chosen_tap_ignores: Vec<String> = Vec::new();
    let mut chosen_dconf: Vec<(usize, Vec<dconf::KeyDiff>)> = Vec::new();
    let mut chosen_gext: Vec<String> = Vec::new();
    let mut chosen_gext_drops: Vec<String> = Vec::new();
    let mut chosen_package_drops: Vec<String> = Vec::new();
    let mut chosen_remote_adds: Vec<String> = Vec::new();
    let mut chosen_remote_drops: Vec<String> = Vec::new();
    let mut chosen_rpm_adds: Vec<String> = Vec::new();
    let mut chosen_rpm_drops: Vec<String> = Vec::new();

    if csw {
        // Machine-scope only. `[ignore]` routing is a judgement, not a state, so
        // an extra is ADDED rather than ignored; tap-trust is fleet-scope and
        // needs --include-trust (and is reported below either way).
        chosen_drops = plan.drops.clone();
        chosen_adds = plan.adds.iter().map(|a| a.token.clone()).collect();
        // Machine-scoped, so `--csw` takes both directions: they land in this
        // machine's own `extensions`, never in a shared bundle. See the note
        // above on why the tap-trust "never drop automatically" rule does not
        // transfer here.
        chosen_gext = plan.gext_adds.clone();
        chosen_gext_drops = plan.gext_drops.clone();
        chosen_package_drops = plan.package_drops.clone();
        chosen_remote_adds = plan.remote_adds.clone();
        chosen_remote_drops = plan.remote_drops.clone();
        chosen_rpm_adds = plan.rpm_adds.clone();
        chosen_rpm_drops = plan.rpm_drops.clone();
        chosen_trust_adds = plan.trust_adds.clone();
        chosen_trust_drops = plan.trust_drops.clone();
        for (i, dp) in plan.dconf.iter().enumerate() {
            let all: Vec<dconf::KeyDiff> = dp
                .sections
                .iter()
                .flat_map(|(_, ds)| ds.iter().cloned())
                .collect();
            if !all.is_empty() {
                chosen_dconf.push((i, all));
            }
        }
    } else {
        // Missing → keep/drop (default KEEP).
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
        if !plan.adds.is_empty() {
            println!("\n{}", ui::bold("Installed but not in the spec:"));
            for a in &plan.adds {
                match prompt_add(&a.token, true) {
                    AddChoice::Add => chosen_adds.push(a.token.clone()),
                    AddChoice::Ignore => chosen_ignores
                        .push((a.ignore_key.to_string(), a.ignore_value.clone())),
                    AddChoice::Skip => {}
                }
            }
        }

        // Tap-trust drops (declared but not trusted) → keep/drop, default KEEP
        // (keeping lets `install`/`update` re-trust it — dropping abandons the tap).
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

        if !plan.remote_drops.is_empty() {
            println!(
                "\n{}",
                ui::bold("Declared in this machine's `flatpak_remotes` but not configured:")
            );
            for token in &plan.remote_drops {
                if !prompt_yes(&format!("  keep `{token}` in [[machine]].flatpak_remotes?")) {
                    chosen_remote_drops.push(token.clone());
                }
            }
        }
        if !plan.remote_adds.is_empty() {
            println!("\n{}", ui::bold("Flatpak remotes not in the spec:"));
            for name in &plan.remote_adds {
                match prompt_add(name, true) {
                    // The url is what a declaration needs, and only the machine
                    // has it — so it is read back from the live remote list.
                    AddChoice::Add => {
                        let url = providers::flatpak_remotes_installed()
                            .unwrap_or_default()
                            .into_iter()
                            .find(|(n, _)| n == name)
                            .map(|(_, u)| u)
                            .unwrap_or_default();
                        chosen_remote_adds.push(format!("{name} {url}"));
                    }
                    AddChoice::Ignore => {
                        chosen_ignores.push(("flatpak_remote".to_string(), name.clone()))
                    }
                    AddChoice::Skip => {}
                }
            }
        }
        if !plan.rpm_drops.is_empty() {
            println!(
                "\n{}",
                ui::bold("Declared in this machine's `rpm_ostree` but not layered:")
            );
            for pkg in &plan.rpm_drops {
                if !prompt_yes(&format!("  keep `{pkg}` in [[machine]].rpm_ostree?")) {
                    chosen_rpm_drops.push(pkg.clone());
                }
            }
        }
        if !plan.rpm_adds.is_empty() {
            println!("\n{}", ui::bold("Layered rpms not in the spec:"));
            for pkg in &plan.rpm_adds {
                match prompt_add(pkg, true) {
                    AddChoice::Add => chosen_rpm_adds.push(pkg.clone()),
                    AddChoice::Ignore => {
                        chosen_ignores.push(("rpm_ostree".to_string(), pkg.clone()))
                    }
                    AddChoice::Skip => {}
                }
            }
        }

        // The loose-list twin of the Brewfile drops above: same question, same
        // default, different file.
        if !plan.package_drops.is_empty() {
            println!(
                "\n{}",
                ui::bold("Declared in this machine's `packages` but not installed:")
            );
            for token in &plan.package_drops {
                if !prompt_yes(&format!("  keep `{token}` in [[machine]].packages?")) {
                    chosen_package_drops.push(token.clone());
                }
            }
        }

        // Declared by THIS machine but not installed → keep/drop, default KEEP
        // (the `drops`/`trust_drops` shape). Keeping lets the next converge
        // reinstall it; dropping says "I removed this on purpose" — which had no
        // verb at all before, so an extension absorbed once could never be
        // un-absorbed and every `install` put it back.
        if !plan.gext_drops.is_empty() {
            println!(
                "\n{}",
                ui::bold("Declared for this machine but not installed:")
            );
            for uuid in &plan.gext_drops {
                if !prompt_yes(&format!("  keep `{uuid}` in [[machine]].extensions?")) {
                    chosen_gext_drops.push(uuid.clone());
                }
            }
        }

        // Installed GNOME extensions no bundle or machine declares → add to THIS
        // machine's `extensions` (default SKIP, like every other extra).
        if !plan.gext_adds.is_empty() {
            println!(
                "\n{}",
                ui::bold("Installed GNOME extensions not in the spec:")
            );
            println!(
                "{}",
                ui::dim(
                    "  adding one declares it for THIS machine only — a bundle's list is shared."
                )
            );
            for uuid in &plan.gext_adds {
                match prompt_add(uuid, true) {
                    AddChoice::Add => chosen_gext.push(uuid.clone()),
                    AddChoice::Ignore => chosen_ignores
                        .push(("gnome_extensions".to_string(), uuid.clone())),
                    AddChoice::Skip => {}
                }
            }
        }

    // Desktop keys → per SECTION, because that is the unit dconf itself defines.
        // For a snapshot rooted at a narrow subtree (`…/shell/extensions/`) each
        // section is one extension, so this is a per-extension ask without temper
        // knowing what an extension is. A one-key section (`enabled-extensions`) is
        // a single prompt, not a section prompt plus a key prompt.
        for (i, dp) in plan.dconf.iter().enumerate() {
            println!(
                "\n{} {}",
                ui::bold("Desktop keys that differ —"),
                ui::bold(&dp.name)
            );
            let mut picked: Vec<dconf::KeyDiff> = Vec::new();
            for (section, diffs) in &dp.sections {
                let label = if section.is_empty() { "/" } else { section };
                // `absorb` takes the machine's value; a key the machine no longer
                // sets can only be absorbed by DROPPING it from the snapshot.
                let verb = |d: &dconf::KeyDiff| {
                    if d.live.is_some() {
                        "absorb"
                    } else {
                        "drop from the snapshot"
                    }
                };
                if diffs.len() == 1 {
                    let d = &diffs[0];
                    println!(
                        "\n  {}  {}",
                        ui::bold(&dconf::key_id(section, &d.key)),
                        ui::dim(&dconf::describe(d))
                    );
                    if prompt_no(&format!("  {}?", verb(d))) {
                        picked.push(d.clone());
                    }
                    continue;
                }
                println!(
                    "\n  {} {}",
                    ui::bold(label),
                    ui::dim(&format!("({} keys differ)", diffs.len()))
                );
                for d in diffs {
                    println!("    {:<26} {}", d.key, ui::dim(&dconf::describe(d)));
                }
                match prompt_section() {
                    SectionChoice::All => picked.extend(diffs.iter().cloned()),
                    SectionChoice::PerKey => {
                        for d in diffs {
                            if prompt_no(&format!("    {} `{}`?", verb(d), d.key)) {
                                picked.push(d.clone());
                            }
                        }
                    }
                    SectionChoice::Skip => {}
                }
            }
            if !picked.is_empty() {
                chosen_dconf.push((i, picked));
            }
        }
    }

    // A fleet write is never silent: `--include-trust` changes every machine in
    // the fleet, so say so rather than folding it into the count.
    let report_fleet_trust = || {
        if fleet_trust_writes == 0 || json {
            return;
        }
        println!(
            "\n{} {}",
            ui::yellow(ui::g_warn()),
            ui::bold(&format!(
                "{fleet_trust_writes} tap(s) also recorded in the FLEET [brew].trust — \
                 this changes every machine"
            ))
        );
    };

    if chosen_drops.is_empty()
        && chosen_adds.is_empty()
        && chosen_ignores.is_empty()
        && chosen_trust_adds.is_empty()
        && chosen_trust_drops.is_empty()
        && chosen_tap_ignores.is_empty()
        && chosen_dconf.is_empty()
        && chosen_gext.is_empty()
        && chosen_gext_drops.is_empty()
        && chosen_package_drops.is_empty()
        && chosen_rpm_adds.is_empty()
        && chosen_rpm_drops.is_empty()
        && chosen_remote_adds.is_empty()
        && chosen_remote_drops.is_empty()
    {
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "machine": m.name, "applied": false,
                    "added": 0, "dropped": 0, "ignored": 0, "dconf_keys": 0,
                    "fleet_trust_writes": fleet_trust_writes,
                })
            );
        } else {
            println!("\nNothing selected — nothing changed.");
            report_fleet_trust();
        }
        return Ok(());
    }

    // Preview — human-only; --json reports the same facts as one document.
    if !json {
        // Preview.
        let bf_label = plan.brewfile_rel.clone().unwrap_or_default();
        println!("\n{}", ui::bold("Proposed changes"));
        for t in &chosen_adds {
            println!(
                "  {} {}  {}",
                ui::green("+"),
                t,
                ui::dim(&format!("{} {bf_label}", ui::g_arrow()))
            );
        }
        for d in &chosen_drops {
            println!(
                "  {} {}  {}",
                ui::red("-"),
                d.trim(),
                ui::dim(&format!("{} {bf_label}", ui::g_arrow()))
            );
        }
        for (list, value) in &chosen_ignores {
            println!(
                "  {} {} {}  {}",
                ui::yellow("~"),
                list,
                value,
                ui::dim(&format!(
                    "{} [machine.ignore].{list} in temper.toml",
                    ui::g_arrow()
                ))
            );
        }
        for tap in &chosen_trust_adds {
            println!(
                "  {} trust {}  {}",
                ui::green("+"),
                tap,
                ui::dim(&format!("{} [brew].trust in temper.toml", ui::g_arrow()))
            );
        }
        for tap in &chosen_trust_drops {
            println!(
                "  {} trust {}  {}",
                ui::red("-"),
                tap,
                ui::dim(&format!("{} [brew].trust in temper.toml", ui::g_arrow()))
            );
        }
        for tap in &chosen_tap_ignores {
            println!(
                "  {} trust {}  {}",
                ui::yellow("~"),
                tap,
                ui::dim(&format!("{} [ignore].tap in temper.toml", ui::g_arrow()))
            );
        }
        for uuid in &chosen_gext {
            println!(
                "  {} extension {}  {}",
                ui::green("+"),
                uuid,
                ui::dim(&format!("{} [[machine]].extensions in temper.toml", ui::g_arrow()))
            );
        }
        for t in &chosen_remote_adds {
            println!(
                "  {} remote {}  {}",
                ui::green("+"),
                t,
                ui::dim(&format!("{} [[machine]].flatpak_remotes", ui::g_arrow()))
            );
        }
        for t in &chosen_remote_drops {
            println!(
                "  {} remote {}  {}",
                ui::red("-"),
                t,
                ui::dim(&format!("{} [[machine]].flatpak_remotes", ui::g_arrow()))
            );
        }
        for pkg in &chosen_rpm_adds {
            println!(
                "  {} rpm-ostree {}  {}",
                ui::green("+"),
                pkg,
                ui::dim(&format!("{} [[machine]].rpm_ostree in temper.toml", ui::g_arrow()))
            );
        }
        for pkg in &chosen_rpm_drops {
            println!(
                "  {} rpm-ostree {}  {}",
                ui::red("-"),
                pkg,
                ui::dim(&format!("{} [[machine]].rpm_ostree in temper.toml", ui::g_arrow()))
            );
        }
        for token in &chosen_package_drops {
            println!(
                "  {} {}  {}",
                ui::red("-"),
                token,
                ui::dim(&format!("{} [[machine]].packages in temper.toml", ui::g_arrow()))
            );
        }
        for uuid in &chosen_gext_drops {
            println!(
                "  {} extension {}  {}",
                ui::red("-"),
                uuid,
                ui::dim(&format!("{} [[machine]].extensions in temper.toml", ui::g_arrow()))
            );
        }
        // Grouped by section, because a flat list buries the thing you most need
        // to see: `--csw` removing thirty keys under one extension looked
        // identical to thirty unrelated lines. Small sections still list every
        // key; larger ones collapse to counts so a big removal is legible.
        for (i, picked) in &chosen_dconf {
            let dp = &plan.dconf[*i];
            for (section, keys) in dconf::group_by_section(picked) {
                let label = if section.is_empty() { "/" } else { &section };
                let removed = keys.iter().filter(|d| d.live.is_none()).count();
                let set = keys.len() - removed;
                if keys.len() <= 3 {
                    for d in &keys {
                        let (mark, verb) = match d.live {
                            Some(_) => (ui::green("+"), "set"),
                            None => (ui::red("-"), "remove"),
                        };
                        println!(
                            "  {} {} {}  {}",
                            mark,
                            verb,
                            dconf::key_id(&d.section, &d.key),
                            ui::dim(&format!("{} {}", ui::g_arrow(), dp.file_rel))
                        );
                    }
                    continue;
                }
                let mut parts = Vec::new();
                if removed > 0 {
                    parts.push(ui::red(&format!("{removed} removed")));
                }
                if set > 0 {
                    parts.push(ui::green(&format!("{set} set")));
                }
                println!(
                    "  {} {}  {}  {}",
                    ui::yellow("~"),
                    ui::bold(label),
                    parts.join(", "),
                    ui::dim(&format!("{} {}", ui::g_arrow(), dp.file_rel))
                );
            }
        }

    }

    report_fleet_trust();

    // The per-item prompts WERE the review step, so `--current-state-wins` still
    // confirms once — otherwise a bulk write lands in the spec unreviewed.
    // `--yes` waives that one confirm.
    if !yes && !prompt_no("\napply these changes?") {
        println!("aborted — nothing changed.");
        return Ok(());
    }

    // Write the Brewfile + [ignore] edits THROUGH the journal, so `temper undo`
    // can revert a reconcile (it edits real folder files, so it's journalable).
    let mut jrnl = journal::Journal::begin();
    // Absorb adds/drops, then canonically re-sort so new entries land in their
    // group (taps → brews → casks → mas …) instead of tacked onto the end.
    if let Some(bf_path) = &bf_path {
        let new_bf = reconcile::sort_brewfile(&reconcile::brewfile_with_adds(
            &reconcile::brewfile_without(&original, &chosen_drops),
            &chosen_adds,
        ));
        if new_bf != original {
            jrnl.record_write(bf_path, Some(original.as_bytes()), new_bf.as_bytes())?;
            std::fs::write(bf_path, &new_bf)
                .map_err(|e| anyhow::anyhow!("writing {}: {e}", bf_path.display()))?;
        }
    }
    // Snapshot files: absorb each accepted key (set to the live value, or drop
    // it). Journaled like every other folder write, so `undo` reverts it.
    for (i, picked) in &chosen_dconf {
        let dp = &plan.dconf[*i];
        let p = home.join(&dp.file_rel);
        let before = std::fs::read_to_string(&p)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", p.display()))?;
        let after = dconf::absorbed(&before, picked);
        if after != before {
            jrnl.record_write(&p, Some(before.as_bytes()), after.as_bytes())?;
            std::fs::write(&p, &after)
                .map_err(|e| anyhow::anyhow!("writing {}: {e}", p.display()))?;
        }
    }
    // temper.toml edits (comment-preserving): [ignore].flatpak absorbs, plus the
    // tap-trust reconcile — [brew].trust add/drop and [ignore].tap absorb.
    let tt_edits = !chosen_ignores.is_empty()
        || !chosen_trust_adds.is_empty()
        || !chosen_trust_drops.is_empty()
        || !chosen_tap_ignores.is_empty()
        || !chosen_gext.is_empty()
        || !chosen_gext_drops.is_empty()
        || !chosen_package_drops.is_empty()
        || !chosen_rpm_adds.is_empty()
        || !chosen_rpm_drops.is_empty()
        || !chosen_remote_adds.is_empty()
        || !chosen_remote_drops.is_empty();
    if tt_edits {
        let tt_path = home.join("temper.toml");
        let before_tt = std::fs::read_to_string(&tt_path)?;
        let mut tt = before_tt.clone();
        for (list, value) in &chosen_ignores {
            tt = reconcile::append_machine_ignore(&tt, &m.name, list, value)?;
        }
        for tap in &chosen_trust_adds {
            // Machine scope by default. `--include-trust` is the explicit,
            // reported opt-in that ALSO records it for the whole fleet.
            tt = reconcile::append_machine_trust(&tt, &m.name, tap)?;
            if include_trust {
                tt = reconcile::append_trust(&tt, tap)?;
            }
        }
        for tap in &chosen_trust_drops {
            tt = reconcile::remove_machine_trust(&tt, &m.name, tap)?;
        }
        for tap in &chosen_tap_ignores {
            tt = reconcile::append_machine_ignore(&tt, &m.name, "tap", tap)?;
        }
        for uuid in &chosen_gext {
            tt = reconcile::append_machine_extension(&tt, &m.name, uuid)?;
        }
        for uuid in &chosen_gext_drops {
            tt = reconcile::remove_machine_extension(&tt, &m.name, uuid)?;
        }
        for token in &chosen_package_drops {
            tt = reconcile::remove_machine_package(&tt, &m.name, token)?;
        }
        for pkg in &chosen_rpm_adds {
            tt = reconcile::append_machine_rpm(&tt, &m.name, pkg)?;
        }
        for t in &chosen_remote_adds {
            tt = reconcile::append_machine_remote(&tt, &m.name, t)?;
        }
        for t in &chosen_remote_drops {
            tt = reconcile::remove_machine_remote(&tt, &m.name, t)?;
        }
        for pkg in &chosen_rpm_drops {
            tt = reconcile::remove_machine_rpm(&tt, &m.name, pkg)?;
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
    let keys: usize = chosen_dconf.iter().map(|(_, p)| p.len()).sum();
    let added =
        chosen_adds.len() + chosen_trust_adds.len() + chosen_gext.len() + chosen_rpm_adds.len();
    let dropped = chosen_drops.len()
        + chosen_trust_drops.len()
        + chosen_gext_drops.len()
        + chosen_package_drops.len()
        + chosen_rpm_drops.len()
        + chosen_remote_drops.len();
    let ignored = chosen_ignores.len() + chosen_tap_ignores.len();
    if json {
        // Only reachable via --current-state-wins --yes (nothing prompted).
        println!(
            "{}",
            serde_json::json!({
                "machine": m.name, "applied": true,
                "added": added, "dropped": dropped, "ignored": ignored,
                "dconf_keys": keys,
                "fleet_trust_writes": fleet_trust_writes,
            })
        );
        let gc = manifest::effective_git(&ft.git, &m.git);
        let msg = format!(
            "reconcile {}: +{} -{} ~{} dconf:{}",
            m.name, added, dropped, ignored, keys
        );
        after_repo_change(&home, &gc, &msg);
        return Ok(());
    }
    let keys_note = if keys > 0 {
        format!(", {keys} desktop key(s) captured")
    } else {
        String::new()
    };
    println!(
        "{} reconcile {}: {} added, {} dropped, {} ignored{}.",
        ui::green(ui::g_ok()),
        m.name,
        added,
        dropped,
        ignored,
        keys_note
    );
    let gc = manifest::effective_git(&ft.git, &m.git);
    let msg = format!(
        "reconcile {}: +{} -{} ~{} dconf:{}",
        m.name, added, dropped, ignored, keys
    );
    after_repo_change(&home, &gc, &msg);
    Ok(())
}

/// Load dconf snapshots back into live dconf. Confirm-gated (clobbers live
/// desktop state); `--yes` or `--json` skips the prompt, `--dry-run` touches
/// nothing. Journaled, so `temper undo` reverts it.
fn cmd_restore(machine: Option<String>, yes: bool, dry_run: bool, json: bool) -> Result<()> {
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

        if !yes && !json && !dry_run {
            println!(
                "{}",
                ui::bold(&format!(
                    "restore {} — loads snapshots into LIVE dconf:",
                    m.name
                ))
            );
            for snap in &m.dconf {
                println!("  {} {}  {}", ui::cyan(ui::g_arrow()), snap.path, ui::dim(&snap.file));
            }
            println!(
                "{}",
                ui::yellow("This overwrites live desktop tweaks under those paths.")
            );
            println!("{}", ui::dim("`temper undo` reverts it."));
            if !prompt_no("apply?") {
                println!("aborted — nothing changed.");
                return Ok(());
            }
        }

        let loaded = plan::run_restore(&home, &m, dry_run)?;
        let paths: Vec<String> = loaded.iter().map(|p| p.display().to_string()).collect();
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "machine": m.name, "dry_run": dry_run, "restored": paths
                })
            );
        } else if dry_run {
            println!("restore {} (dry run) — would load:", m.name);
            for (snap, p) in m.dconf.iter().zip(&paths) {
                println!("  {} {}  {}", ui::cyan(ui::g_arrow()), snap.path, ui::dim(p));
            }
        } else {
            println!(
                "{} restore {}: loaded {} snapshot(s).",
                ui::green(ui::g_ok()),
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

enum SectionChoice {
    All,
    PerKey,
    Skip,
}

/// `[y/N/k]` — default skip, `k` drills into the section's individual keys.
/// Sections are the unit dconf itself defines, so this is the natural grain:
/// one prompt per extension / per settings group, with per-key as the escape.
fn prompt_section() -> SectionChoice {
    print!("  absorb this whole section? [y/N/k]  (k = choose per key) ");
    let r = read_reply();
    if r.starts_with('y') {
        SectionChoice::All
    } else if r.starts_with('k') {
        SectionChoice::PerKey
    } else {
        SectionChoice::Skip
    }
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
    // `undo` writes folder files — it reverts `temper.toml`, a Brewfile, a dconf
    // snapshot — so it owes the same git hook every other folder-writing verb
    // fires. Without it a git-backed home was left silently dirty by the one
    // command whose whole job is putting things back, which is worst exactly
    // when it matters: the run being reverted may already have been committed
    // and pushed, so the repair sat uncommitted while the damage was upstream.
    // Best-effort and quiet on failure: a folder that can't be resolved must not
    // turn a successful revert into an error.
    if !dry_run && reverted > 0 {
        if let Ok(home) = discovery::find_home() {
            if let Ok(ft) = load_fleet(&home) {
                let gc = match machine::resolve(&ft, None) {
                    Ok(m) => manifest::effective_git(&ft.git, &m.git),
                    Err(_) => manifest::effective_git(&ft.git, &None),
                };
                after_repo_change(&home, &gc, &format!("undo: revert {reverted} change(s)"));
            }
        }
    }
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
    // Last of the operate-and-author set: only relevant when a folder was
    // written by an older major, but when it IS relevant nothing else in the
    // guide explains why the folder no longer parses.
    out.push_str("\n\n=== MIGRATION (moving a folder across a MAJOR version) ===\n\n");
    out.push_str(include_str!("../../../MIGRATION-GUIDE.md"));
    // The design docs describe intent; the SCHEMA + STATUS above are what's real.
    out.push_str("\n\n=== ARCHITECTURE (design intent — trust SCHEMA + STATUS above for what's implemented) ===\n\n");
    out.push_str(include_str!("../../../ARCHITECTURE.md"));
    out.push_str("\n\n=== PRINCIPLES (design intent) ===\n\n");
    out.push_str(include_str!("../../../PRINCIPLES.md"));
    // What is deliberately NOT built, and the known gaps in what is. An agent
    // authoring a folder needs this as much as the schema: without it, a feature
    // that scores ⚠ in the matrix reads as one that works.
    out.push_str("\n\n=== ROADMAP (deferred features + known gaps — what is NOT built) ===\n\n");
    out.push_str(include_str!("../../../ROADMAP.md"));
    out
}
