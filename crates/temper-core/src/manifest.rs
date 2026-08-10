//! Parse and validate the manifest: `temper.toml` (machines, template vars) and
//! `apps/<name>.toml` bundles (ordered steps + drift-only assertions). See
//! ../../SPEC.md.
//!
//! Step primitives: `copy` (verbatim/template/seed/mode), `block`, `setkey`,
//! `exec`, `profile`. `[[assert]]` covers drift-only checks.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemperToml {
    #[serde(default)]
    pub machine: Vec<Machine>,
    /// Declared template variables, referenced as `{{ var "NAME" }}`.
    #[serde(default)]
    pub vars: std::collections::BTreeMap<String, String>,
    /// Packages installed but not declared that drift/prune must NOT flag as
    /// extras (OS-preinstalled baseline, e.g. Bazzite's default flatpaks).
    #[serde(default)]
    pub ignore: Ignore,
    /// brew-specific settings.
    #[serde(default)]
    pub brew: BrewConfig,
    /// Output glyph set. See `[ui]`.
    #[serde(default)]
    pub ui: UiConfig,
    /// Optional `eq-import` config: where to pull calibrated speaker profiles
    /// from and land them in the folder (RIS's `eq-import`).
    #[serde(default)]
    pub eq_import: Option<EqImport>,
    /// Optional fleet-wide git convenience settings (persist temper's own writes
    /// to a git home). A `[machine.git]` overrides this per machine.
    #[serde(default)]
    pub git: Option<GitConfig>,
    /// What to do when this folder was written by a temper NEWER than the one
    /// running (detected via the `temper_version` stamp below). See `[update]`.
    #[serde(default)]
    pub update: UpdateConfig,
    /// The temper version that last WROTE this file — temper stamps it on every
    /// write (`stamp_version`). On load, a stamp newer than the running temper
    /// drives `[update]`. Managed: hand-editing it is pointless (temper rewrites
    /// it), but it parses so a stamped folder round-trips cleanly.
    #[serde(default)]
    pub temper_version: Option<String>,
}

/// `[eq_import]` — fetch calibrated speaker profiles into the folder (authoring,
/// not machine-converge; see ROADMAP/PRINCIPLES).
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct EqImport {
    /// Upstream git repo of `*.calibrated.conf` profiles (shallow-cloned).
    pub repo: String,
    /// Destination dir in the folder (relative); each `<x>.calibrated.conf`
    /// lands as `<x>.conf`. Defaults to `assets/speaker-eq`.
    #[serde(default = "default_eq_dest")]
    pub dest: String,
}

fn default_eq_dest() -> String {
    "assets/speaker-eq".to_string()
}

/// `[git]` settings — the optional convenience layer for persisting temper's own
/// writes to a git-backed home. Everything here is a no-op on a non-git folder.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct GitConfig {
    /// Hint (to stderr) after a repo-writing verb leaves the folder dirty.
    #[serde(default = "default_true")]
    pub remind: bool,
    /// Commit automatically after a repo-writing verb (with an auto message).
    #[serde(default)]
    pub auto_commit: bool,
    /// Also push after an auto-commit / on `save`.
    #[serde(default)]
    pub auto_push: bool,
    /// `git pull` the folder before a run (warn, never abort, if it can't) so
    /// you work on the latest spec. Fast-forward-only by default (see
    /// `auto_rebase`).
    #[serde(default)]
    pub auto_pull: bool,
    /// When `auto_pull` runs, `git pull --rebase` instead of `--ff-only` — so a
    /// pull still succeeds when local has commits the remote doesn't (they're
    /// replayed on top). No effect unless `auto_pull` is on.
    #[serde(default)]
    pub auto_rebase: bool,
}

/// How the pre-run auto-pull should pull, resolved from `[git]`/`[machine.git]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullMode {
    /// `auto_pull` off — don't pull.
    Off,
    /// `git pull --ff-only` (the safe default).
    FastForward,
    /// `git pull --rebase` (replay local commits on top of the remote).
    Rebase,
}

fn default_true() -> bool {
    true
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            remind: true,
            auto_commit: false,
            auto_push: false,
            auto_pull: false,
            auto_rebase: false,
        }
    }
}

/// `[update]` — what temper does when this folder was written by a temper NEWER
/// than the one running (a version skew, detected via the `temper_version`
/// stamp). The check itself is free and fleet-friendly: a newer machine stamps
/// the folder, an older one notices on the next command.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateConfig {
    /// `off` | `warn` | `prompt` (default) | `auto` — see `UpdateMode`.
    #[serde(default)]
    pub mode: UpdateMode,
}

/// What to do on a newer-temper skew. `prompt` is the default: explain, and on a
/// Homebrew install offer to run the upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateMode {
    /// Ignore the stamp entirely — a skew errors plainly, like any other parse
    /// failure. (The escape hatch if you never want temper touching Homebrew.)
    Off,
    /// Report the skew and print the upgrade command; never touch Homebrew.
    Warn,
    /// Report it and, on a Homebrew install, interactively offer to run
    /// `brew upgrade temper`. The default.
    #[default]
    Prompt,
    /// Run the Homebrew upgrade without asking (unattended machines).
    Auto,
}

/// `[ui]` settings — how temper draws its status markers.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiConfig {
    /// `"unicode"` (default) or `"nerd"`.
    ///
    /// Nerd glyphs are Private Use Area: crisp where a patched font is
    /// installed, an empty box where it isn't. So the default stays the set that
    /// renders anywhere, and `TEMPER_ICONS` overrides this per terminal — font
    /// coverage belongs to the terminal, not to the spec.
    #[serde(default)]
    pub icons: Option<String>,
}

/// `[brew]` settings.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrewConfig {
    /// Third-party taps to `brew trust` before any converge/upgrade (Homebrew
    /// 5.2+ gates untrusted taps, silently skipping their formulae otherwise).
    #[serde(default)]
    pub trust: Vec<String>,
}

/// Per-manager ignore lists (by the same short name drift matches on).
#[derive(Debug, Default, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Ignore {
    #[serde(default)]
    pub brew: Vec<String>,
    #[serde(default)]
    pub cask: Vec<String>,
    #[serde(default)]
    pub flatpak: Vec<String>,
    #[serde(default)]
    pub mas: Vec<String>,
    #[serde(default)]
    pub vscode: Vec<String>,
    #[serde(default)]
    pub tap: Vec<String>,
    /// GNOME extension UUIDs installed in the user scope that should not be
    /// reported as extras — a deliberate hand-install you don't want tracked.
    /// (System/image-baked extensions are never extras, so they need no entry.)
    #[serde(default, alias = "gext")]
    pub gnome_extensions: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Machine {
    pub name: String,
    /// "mac" | "linux".
    pub os: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub apps: Vec<String>,
    /// Per-machine loose packages that belong to no app-bundle.
    #[serde(default)]
    pub packages: Vec<String>,
    /// Per-machine GNOME extensions, unioned with the composed bundles'.
    ///
    /// The machine-scoped counterpart of a bundle's `extensions`, mirroring how
    /// `packages` gives a machine somewhere of its own. Without it an extension
    /// could only be declared in a *shared* bundle, so `reconcile` had nowhere
    /// to write one and an undeclared extension's only answers were "ignore it"
    /// or "uninstall it" — never "yes, on this machine".
    #[serde(default)]
    #[serde(alias = "extensions")]
    pub gnome_extensions: Vec<String>,
    /// A Brewfile (relative to the temper-home) whose lines are added to this
    /// machine's package set — the clean way to migrate an existing Brewfile.
    #[serde(default)]
    pub brewfile: Option<String>,
    /// Per-machine template vars, merged OVER the global `[vars]` (so a Linux
    /// box can override a Mac-valued `BREW_PREFIX`). Referenced as
    /// `{{ var "NAME" }}` like the globals.
    #[serde(default)]
    pub vars: std::collections::BTreeMap<String, String>,
    /// Whole-desktop dconf snapshots this machine owns: `snapshot` captures each
    /// (filtered by `strip`) to its `file`; `restore` loads them back. The
    /// machine-scope GNOME/Ptyxis state RIS captured with gnome-backup/restore.
    /// Taps THIS machine trusts, on top of the fleet `[brew].trust`.
    ///
    /// The machine-scope counterpart of a fleet list. Without it, `reconcile`
    /// had nowhere to record a tap this box trusts and compensated by editing
    /// the FLEET list from one machine — which changes every other machine
    /// silently, and is the one thing scope forbids (Principle #12). Absorbing
    /// and dropping both land here.
    #[serde(default)]
    pub brew_trust: Vec<String>,
    /// Extras THIS machine should not be told about, on top of the fleet
    /// `[ignore]`. Same reason: silencing something is a per-machine judgement
    /// far more often than a fleet one, and the fleet list could not express
    /// "on this box".
    #[serde(default)]
    pub ignore: Ignore,
    #[serde(default)]
    pub dconf: Vec<DconfSnapshot>,
    /// Per-machine git-convenience override (wholesale replaces the fleet
    /// `[git]` for this machine — e.g. auto-push only from your main box).
    #[serde(default)]
    pub git: Option<GitConfig>,
}

/// One whole-subtree dconf snapshot (e.g. `/org/gnome/shell/`).
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct DconfSnapshot {
    /// The dconf path prefix to dump/load (must end in `/`, e.g.
    /// `/org/gnome/shell/`).
    pub path: String,
    /// Snapshot file, relative to the temper-home (`snapshot` writes, `restore`
    /// reads).
    pub file: String,
    /// Substrings of a dumped `section/key` line to drop on capture — the
    /// strip-keys filter (bookkeeping + per-monitor panel keys that would
    /// corrupt a capture→restore round-trip). Applied to **both** sides of a
    /// drift comparison, so a stripped key never reads as drift.
    #[serde(default)]
    pub strip: Vec<String>,
    /// Optional human name for drift/reconcile output ("extensions: 3 keys
    /// drifted") in place of the raw dconf path.
    #[serde(default)]
    pub label: Option<String>,
}

impl DconfSnapshot {
    /// What to call this snapshot in output: its `label`, else its `path`.
    pub fn name(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.path)
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    /// Optional bundle-level gate: skip this bundle's machine-scope lists
    /// (`extensions`/`rpm`) unless the machine's os matches ("mac" | "linux").
    /// Makes "GNOME extensions are Linux-desktop only" enforced, not convention.
    #[serde(default)]
    pub os: Option<String>,
    /// Optional bundle-level gate: skip `extensions`/`rpm` unless the machine's
    /// role matches ("desktop" | "server") — so a server that mistakenly composes
    /// a desktop bundle never layers its extensions/rpms.
    #[serde(default)]
    pub role: Option<String>,
    /// Packages this bundle needs (Brewfile line grammar), aggregated into the
    /// machine's effective set. `_mac`/`_linux` variants are OS-scoped.
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub packages_mac: Vec<String>,
    #[serde(default)]
    pub packages_linux: Vec<String>,
    /// GNOME extension UUIDs to install via `gext` (Linux desktop).
    #[serde(default)]
    #[serde(alias = "extensions")]
    pub gnome_extensions: Vec<String>,
    /// rpm-ostree layered packages (Linux; can't be image-baked).
    #[serde(default)]
    #[serde(alias = "rpm")]
    pub rpm_ostree: Vec<String>,
    #[serde(default)]
    pub step: Vec<Step>,
    /// Drift-only assertions (no converge action).
    #[serde(default)]
    pub assert: Vec<Assert>,
}

/// One primitive step. Exactly one primitive is set.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Step {
    // --- copy ---
    #[serde(default)]
    pub copy: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    /// `copy`: substitute `{{ … }}` before deploy.
    #[serde(default)]
    pub template: bool,
    /// `copy`: create-once if absent, then hands-off; excluded from drift.
    #[serde(default)]
    pub seed: bool,
    /// `copy`: octal file mode enforced on the target (e.g. "0600").
    #[serde(default)]
    pub mode: Option<String>,

    // --- block: ensure a marker-delimited region is present in a user file ---
    #[serde(default)]
    pub block: Option<String>,
    #[serde(default, rename = "in")]
    pub in_file: Option<String>,
    #[serde(default)]
    pub marker: Option<String>,

    // --- setkey: set a key in a structured file, preserving siblings ---
    #[serde(default)]
    pub setkey: Option<SetKey>,

    // --- profile: install a macOS .mobileconfig (manual approval) ---
    #[serde(default)]
    pub profile: Option<String>,

    // --- sysfile: write one ROOT-owned file (the clean /etc path) ---
    /// Source file in the folder, deployed to `to` as a root-owned system file
    /// with `mode`/`owner`/`group`, escalating internally (`sudo install`) for
    /// just that write. Drift-checkable; not journaled (system-side).
    #[serde(default)]
    pub sysfile: Option<String>,
    /// `sysfile` owner (e.g. "root"). Enforced on apply, drift-checked.
    #[serde(default)]
    pub owner: Option<String>,
    /// `sysfile` group (e.g. "root").
    #[serde(default)]
    pub group: Option<String>,

    // --- exec: run a user script (the escape hatch) ---
    #[serde(default)]
    pub exec: Option<String>,
    /// Companion drift-hook: exit 0 = in sync. Also gates whether `exec` re-runs.
    #[serde(default)]
    pub check: Option<String>,
    /// "This script escalates internally." temper still runs it **as the user**
    /// (the chezmoi model — escalate per-command inside the script), so this does
    /// not change how it runs; it declares that root will be needed, which lets
    /// temper fold the step into the single up-front password ask instead of the
    /// script stopping mid-run to prompt. `sysfile` steps are included without a
    /// declaration, since temper escalates for those itself.
    #[serde(default)]
    pub sudo: bool,
    /// Env var names that must be present and are passed through to the script.
    #[serde(default)]
    pub secrets: Vec<String>,

    /// Lifecycle: "always" (re-apply every update) | "install" (once) |
    /// "ensure" (install-if-missing on update; never overwrites a present
    /// target) | "manual" (only when explicitly invoked). Default by primitive:
    /// copy/setkey/block → always; exec/seed → install.
    #[serde(default)]
    pub run: Option<String>,

    /// Skip this step unless the machine's OS matches ("mac" | "linux").
    #[serde(default)]
    pub os: Option<String>,
    /// Skip this step unless the machine's role matches ("desktop" | "server").
    #[serde(default)]
    pub role: Option<String>,

    /// Presence gate — **skip** this step (loudly) unless the probe passes
    /// ("run my config only if the app is actually here"). Reality, not intent.
    #[serde(default)]
    pub when: Option<Probe>,
    /// Hard presence requirement — **error** unless the probe passes. For a step
    /// that is meaningless (not merely skippable) without its dependency.
    #[serde(default)]
    pub needs: Option<Probe>,
}

/// A presence probe: exactly one field set. Checks *reality* (what's actually on
/// the machine), so image-baked / hand-installed / opted-out apps all behave
/// under one rule.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    /// A command that must resolve on PATH.
    #[serde(default)]
    pub binary: Option<String>,
    /// A filesystem path that must exist (`~` expands).
    #[serde(default)]
    pub path: Option<String>,
    /// A brew formula that must be installed.
    #[serde(default)]
    pub brew: Option<String>,
    /// A brew cask that must be installed.
    #[serde(default)]
    pub cask: Option<String>,
    /// A flatpak app id that must be installed.
    #[serde(default)]
    pub flatpak: Option<String>,
    /// A Mac App Store id that must be installed.
    #[serde(default)]
    pub mas: Option<String>,
    /// A GNOME extension uuid that must be present.
    #[serde(default)]
    pub gext: Option<String>,
    /// An rpm that must be installed (`rpm -q`).
    #[serde(default)]
    pub rpm: Option<String>,
    /// A script (relative to the temper-home) that must exit 0.
    #[serde(default)]
    pub exec: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct SetKey {
    /// json | toml | ini | defaults (macOS) | dconf (Linux).
    pub backend: String,
    /// Target file for file backends.
    #[serde(default)]
    pub file: Option<String>,
    /// Dotted key path.
    pub key: String,
    /// Scalar (or array) value to set.
    pub value: toml::Value,
    /// List-union append into an array-valued key.
    #[serde(default)]
    pub append: bool,
    /// Render `{{ … }}` in the string leaves of `value` at apply time
    /// (`which`/`env`/`var`/`brew_prefix`), like `copy`'s `template`. Default
    /// false — a value is literal unless opted in. Works on every backend.
    #[serde(default)]
    pub template: bool,
}

/// A drift-only assertion. Exactly one check field is set.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Assert {
    /// Path that must NOT exist.
    #[serde(default)]
    pub absent: Option<String>,
    /// A file that must contain a given line.
    #[serde(default)]
    pub contains_line: Option<ContainsLine>,
    /// A path that must have a given octal mode.
    #[serde(default)]
    pub mode: Option<ModeCheck>,
    /// A command that must resolve on PATH.
    #[serde(default)]
    pub executable_resolves: Option<String>,
    /// The current user must NOT belong to this group.
    #[serde(default)]
    pub not_member: Option<GroupCheck>,
    /// The current user's login shell must equal this path.
    #[serde(default)]
    pub shell: Option<String>,
    /// A deployed json file must be semantically equal to a reference.
    #[serde(default)]
    pub json_semantic: Option<JsonSemantic>,
    /// `"drift"` (default) or `"notice"`.
    ///
    /// Some conditions an assertion watches are a *state*, not a defect: a
    /// staged ostree deployment means an update is waiting for a reboot —
    /// nothing is wrong, and calling it drift makes a converged machine look
    /// broken and can never be cleared by any verb. A `notice` is reported for
    /// visibility, kept out of the out-of-sync count, and never given a
    /// remediation.
    #[serde(default)]
    pub severity: Option<String>,
    /// Human text shown instead of the generic check result — say what to do
    /// ("a system update is staged; reboot to apply") rather than restating the
    /// predicate ("should not exist").
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub os: Option<String>,
    /// Skip this assertion unless the machine's role matches.
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ContainsLine {
    pub file: String,
    pub line: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ModeCheck {
    pub path: String,
    pub mode: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct GroupCheck {
    pub group: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct JsonSemantic {
    /// The deployed file to check (on the machine).
    pub file: String,
    /// The reference file (relative to the temper-home folder).
    pub against: String,
}

/// This build's temper version. Both workspace crates set `version.workspace`,
/// so this equals the running CLI's `--version`. Stamped into `temper.toml` on
/// every write, and compared against a file's stamp on load.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Numeric `(major, minor, patch)` of a version string, ignoring any
/// `-pre`/`+build` suffix. Unparseable parts → 0, so a version we can't read
/// never spuriously compares as "newer".
fn version_triple(s: &str) -> (u64, u64, u64) {
    let core = s.trim().split(['-', '+']).next().unwrap_or("");
    let mut it = core.split('.').map(|p| p.trim().parse::<u64>().unwrap_or(0));
    (
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
    )
}

/// Is `candidate` a strictly newer temper than `running`? (Both `major.minor.patch`.)
pub fn version_is_newer(candidate: &str, running: &str) -> bool {
    version_triple(candidate) > version_triple(running)
}

/// The `temper_version` stamp from a raw temper.toml, read leniently — a file
/// whose *strict* parse just failed still yields its stamp, which is exactly the
/// case that needs it. Any trouble → None.
pub fn peek_version_stamp(src: &str) -> Option<String> {
    src.parse::<toml::Value>()
        .ok()?
        .get("temper_version")?
        .as_str()
        .map(str::to_string)
}

/// `[update].mode`, read leniently from a raw temper.toml (needed on the
/// parse-failure path, where the strict load is unavailable). Unset/unreadable →
/// the default (`prompt`).
pub fn peek_update_mode(src: &str) -> UpdateMode {
    src.parse::<toml::Value>()
        .ok()
        .and_then(|v| {
            v.get("update")
                .and_then(|u| u.get("mode"))
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .and_then(|s| match s.as_str() {
            "off" => Some(UpdateMode::Off),
            "warn" => Some(UpdateMode::Warn),
            "prompt" => Some(UpdateMode::Prompt),
            "auto" => Some(UpdateMode::Auto),
            _ => None,
        })
        .unwrap_or_default()
}

/// Stamp `temper_version = VERSION` at the top level of a temper.toml, preserving
/// comments/formatting (toml_edit). Updates an existing stamp in place, else
/// prepends it as a leading root key (valid before any `[table]`). **Monotonic**:
/// a stamp already recording a NEWER temper is left untouched — stamping *down*
/// would erase the very signal an older temper needs to spot the skew. Idempotent
/// otherwise, so a git home sees no diff until temper itself is upgraded.
pub fn stamp_version(src: &str) -> Result<String> {
    if let Some(existing) = peek_version_stamp(src) {
        if version_is_newer(&existing, VERSION) {
            return Ok(src.to_string()); // never stamp a newer folder down
        }
    }
    let mut doc: toml_edit::DocumentMut = src
        .parse()
        .context("parsing temper.toml for version stamp")?;
    if doc.as_table().contains_key("temper_version") {
        doc["temper_version"] = toml_edit::value(VERSION);
        Ok(doc.to_string())
    } else {
        Ok(format!("temper_version = \"{VERSION}\"\n{src}"))
    }
}

/// The load-time error raised when a folder was written by a temper NEWER than
/// the one running: the strict parse choked on a field/value this build doesn't
/// know, AND the `temper_version` stamp confirms it's a version skew (not a
/// typo). Carries what the CLI needs to explain the upgrade — and, per
/// `[update].mode`, offer to run it. See `crates/temper/src/main.rs`.
#[derive(Debug)]
pub struct NewerVersion {
    /// The `temper_version` stamped in the file (the temper that wrote it).
    pub required: String,
    /// The running temper (`VERSION`).
    pub running: String,
    /// Resolved `[update].mode`.
    pub mode: UpdateMode,
    /// The underlying strict-parse error, for the detail line.
    pub parse_error: String,
}

impl std::fmt::Display for NewerVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "this temper-home was written by temper {} — you're running {}. \
             Upgrade temper to read it (`brew upgrade temper`). (parser: {})",
            self.required, self.running, self.parse_error
        )
    }
}

impl std::error::Error for NewerVersion {}

/// Reject duplicate machine names — otherwise the second silently shadows.
fn reject_duplicate_machines(ft: &TemperToml) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for m in &ft.machine {
        if !seen.insert(m.name.to_lowercase()) {
            anyhow::bail!("duplicate machine name '{}' in temper.toml", m.name);
        }
    }
    Ok(())
}

pub fn load_fleet(home: &Path) -> Result<TemperToml> {
    let p = home.join("temper.toml");
    let s = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    match toml::from_str::<TemperToml>(&s) {
        Ok(ft) => {
            reject_duplicate_machines(&ft)?;
            Ok(ft)
        }
        // Couldn't parse. Version skew (a newer temper's field) or a genuine
        // mistake? The stamp decides: a stamp newer than us (with the check on)
        // becomes a `NewerVersion` the CLI can offer to fix; anything else is the
        // plain parse error the author needs to see.
        Err(e) => {
            let mode = peek_update_mode(&s);
            if mode != UpdateMode::Off {
                if let Some(stamp) = peek_version_stamp(&s) {
                    if version_is_newer(&stamp, VERSION) {
                        return Err(NewerVersion {
                            required: stamp,
                            running: VERSION.to_string(),
                            mode,
                            parse_error: e.to_string(),
                        }
                        .into());
                    }
                }
            }
            Err(e).with_context(|| format!("parsing {}", p.display()))
        }
    }
}

/// Load a bundle, with the same version-skew story `load_fleet` has.
///
/// Every struct here carries `deny_unknown_fields`, so a folder written by a
/// newer temper — one whose bundles use a field this binary has never heard of —
/// fails to parse. `temper.toml` has always turned that into "upgrade temper";
/// bundles turned it into a raw TOML error with no hint, which is the wrong half
/// to leave bare: new gates and new step primitives land in `apps/*.toml`, and a
/// fleet on staggered updates would hit it on every machine at once.
///
/// The stamp lives in `temper.toml` (a bundle carries none), so that is where
/// the answer is read from — the folder is stamped as a whole.
pub fn load_bundle(home: &Path, name: &str) -> Result<Bundle> {
    let p = home.join("apps").join(format!("{name}.toml"));
    let s =
        std::fs::read_to_string(&p).with_context(|| format!("reading bundle {}", p.display()))?;
    match toml::from_str::<Bundle>(&s) {
        Ok(b) => Ok(b),
        Err(e) => {
            if let Ok(fleet) = std::fs::read_to_string(home.join("temper.toml")) {
                let mode = peek_update_mode(&fleet);
                if mode != UpdateMode::Off {
                    if let Some(stamp) = peek_version_stamp(&fleet) {
                        if version_is_newer(&stamp, VERSION) {
                            return Err(NewerVersion {
                                required: stamp,
                                running: VERSION.to_string(),
                                mode,
                                parse_error: format!("{}: {e}", p.display()),
                            }
                            .into());
                        }
                    }
                }
            }
            Err(e).with_context(|| format!("parsing {}", p.display()))
        }
    }
}

#[cfg(test)]
mod rename_alias_tests {
    use super::*;

    /// A folder written against the old names keeps parsing.
    ///
    /// Names got specific (Principle #13) — `extensions` collided with the VS
    /// Code extensions temper also manages, and `rpm` claimed a slot a future
    /// `apt`/`dnf` deserves. Every struct here is `deny_unknown_fields`, so
    /// without the aliases the rename would be a hard parse error on every
    /// existing folder rather than a rename.
    #[test]
    fn the_old_field_names_still_parse() {
        let b: Bundle = toml::from_str(
            "extensions = [\"a@x\"]\nrpm = [\"vpn\"]\n",
        )
        .expect("old bundle names must still parse");
        assert_eq!(b.gnome_extensions, vec!["a@x".to_string()]);
        assert_eq!(b.rpm_ostree, vec!["vpn".to_string()]);

        let t: TemperToml = toml::from_str(
            "[[machine]]\nname = \"a\"\nos = \"linux\"\nextensions = [\"b@x\"]\n\n[ignore]\ngext = [\"c@x\"]\n",
        )
        .expect("old machine/ignore names must still parse");
        assert_eq!(t.machine[0].gnome_extensions, vec!["b@x".to_string()]);
        assert_eq!(t.ignore.gnome_extensions, vec!["c@x".to_string()]);
    }

    /// …and the new names parse too, obviously — but assert it, because an alias
    /// typo would leave only the OLD name working and nothing would notice.
    #[test]
    fn the_new_field_names_parse() {
        let b: Bundle = toml::from_str(
            "gnome_extensions = [\"a@x\"]\nrpm_ostree = [\"vpn\"]\n",
        )
        .expect("new bundle names must parse");
        assert_eq!(b.gnome_extensions, vec!["a@x".to_string()]);
        assert_eq!(b.rpm_ostree, vec!["vpn".to_string()]);
    }
}

#[cfg(test)]
mod bundle_skew_tests {
    use super::*;

    /// A bundle written by a NEWER temper must raise the upgrade path, not a
    /// bare TOML error. `temper.toml` has always done this; bundles are where
    /// new gates and step primitives actually land, so leaving them bare turned
    /// a staggered fleet update into an outage with no guidance.
    #[test]
    fn a_newer_folders_bundle_asks_you_to_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join("apps")).unwrap();
        std::fs::write(
            home.join("temper.toml"),
            "temper_version = \"9999.0.0\"\n",
        )
        .unwrap();
        // A field this binary has never heard of — `deny_unknown_fields` rejects it.
        std::fs::write(
            home.join("apps").join("x.toml"),
            "a_field_from_the_future = true\n",
        )
        .unwrap();
        let err = load_bundle(home, "x").unwrap_err();
        assert!(
            err.downcast_ref::<NewerVersion>().is_some(),
            "expected the upgrade path, got: {err}"
        );
    }

    /// …and a genuine authoring mistake, on a folder that is NOT newer, still
    /// surfaces as the parse error the author has to see.
    #[test]
    fn a_plain_mistake_is_still_a_plain_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join("apps")).unwrap();
        std::fs::write(home.join("temper.toml"), "temper_version = \"0.0.1\"\n").unwrap();
        std::fs::write(home.join("apps").join("x.toml"), "typo_here = true\n").unwrap();
        let err = load_bundle(home, "x").unwrap_err();
        assert!(err.downcast_ref::<NewerVersion>().is_none());
    }
}

/// A machine's effective tap-trust: the fleet list plus its own, de-duplicated.
///
/// A union, not an override: the fleet list is a group decision this machine is
/// a member of, so it cannot opt out of one here — that is a spec edit. What it
/// can do is add.
pub fn effective_trust(fleet: &[String], machine: &Machine) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in fleet.iter().chain(machine.brew_trust.iter()) {
        if !out.iter().any(|x| x == t) {
            out.push(t.clone());
        }
    }
    out
}

/// A machine's effective ignore lists: the fleet lists plus its own, per
/// manager. Union, for the same reason as `effective_trust`.
pub fn effective_ignore(fleet: &Ignore, machine: &Machine) -> Ignore {
    let join = |a: &[String], b: &[String]| {
        let mut out: Vec<String> = a.to_vec();
        for x in b {
            if !out.iter().any(|y| y == x) {
                out.push(x.clone());
            }
        }
        out
    };
    let m = &machine.ignore;
    Ignore {
        brew: join(&fleet.brew, &m.brew),
        cask: join(&fleet.cask, &m.cask),
        flatpak: join(&fleet.flatpak, &m.flatpak),
        mas: join(&fleet.mas, &m.mas),
        vscode: join(&fleet.vscode, &m.vscode),
        tap: join(&fleet.tap, &m.tap),
        gnome_extensions: join(&fleet.gnome_extensions, &m.gnome_extensions),
    }
}

/// A machine's effective git settings: a `[machine.git]` wholly overrides the
/// fleet `[git]`; otherwise the fleet setting; otherwise defaults (remind on).
pub fn effective_git(fleet: &Option<GitConfig>, machine: &Option<GitConfig>) -> GitConfig {
    machine
        .clone()
        .or_else(|| fleet.clone())
        .unwrap_or_default()
}

/// Cheap pre-load peek at `auto_pull` — read before the full, validated load so
/// a pull can happen *before* any spec file is read. A `[machine.git]` for this
/// machine wholly overrides the fleet `[git]` (same precedence as
/// `effective_git`): the pull decision must match what the resolved config would
/// say, or a machine that sets `auto_pull` only under `[machine.git]` would
/// never pull. Any parse trouble → false (pull is opt-in; never let a peek error
/// block a run).
pub fn peek_pull_mode(home: &Path) -> PullMode {
    std::fs::read_to_string(home.join("temper.toml"))
        .ok()
        .and_then(|s| s.parse::<toml::Value>().ok())
        .map(|v| pull_mode_from(&v, crate::machine::hostname().as_deref()))
        .unwrap_or(PullMode::Off)
}

/// Peek at `[git].auto_rebase` (machine-override-aware) — the pull strategy
/// `save` uses for its pre-push sync, read cheaply before the validated load so
/// even a save that fixes a malformed spec still resolves it.
pub fn peek_auto_rebase(home: &Path) -> bool {
    std::fs::read_to_string(home.join("temper.toml"))
        .ok()
        .and_then(|s| s.parse::<toml::Value>().ok())
        .map(|v| git_flag_from(&v, crate::machine::hostname().as_deref(), "auto_rebase"))
        .unwrap_or(false)
}

/// A boolean `[git]`/`[machine.git]` flag from the raw doc for the resolved
/// machine. A `[machine.git]` replaces the fleet `[git]` outright (so we do NOT
/// fall back to fleet when it's present) — matching `effective_git`.
fn git_flag_from(v: &toml::Value, host: Option<&str>, key: &str) -> bool {
    let git = peek_current_machine(v, host)
        .and_then(|m| m.get("git"))
        .or_else(|| v.get("git"));
    git.and_then(|g| g.get(key))
        .and_then(|b| b.as_bool())
        .unwrap_or(false)
}

/// Pure pull-mode decision over the raw doc + this host's name.
fn pull_mode_from(v: &toml::Value, host: Option<&str>) -> PullMode {
    if !git_flag_from(v, host, "auto_pull") {
        PullMode::Off
    } else if git_flag_from(v, host, "auto_rebase") {
        PullMode::Rebase
    } else {
        PullMode::FastForward
    }
}

/// The `[[machine]]` table for this host, resolved the way `machine::resolve`
/// would with no explicit name: by hostname, else the sole machine. Peek-only —
/// works on the raw `toml::Value` before the validated load.
fn peek_current_machine<'a>(v: &'a toml::Value, host: Option<&str>) -> Option<&'a toml::Value> {
    let machines = v.get("machine")?.as_array()?;
    if let Some(h) = host {
        if let Some(m) = machines
            .iter()
            .find(|m| m.get("name").and_then(|n| n.as_str()) == Some(h))
        {
            return Some(m);
        }
    }
    (machines.len() == 1).then(|| &machines[0])
}

/// Whether an os/role-gated item should be **skipped** for this machine. A
/// declared `os` that differs from the machine's os skips; a declared `role`
/// that differs from the machine's declared role skips. An unset side never
/// gates (lenient — the machine may not declare a role). Shared by step,
/// assert, and bundle gating so they can't drift apart.
pub fn gated(os: &Option<String>, role: &Option<String>, machine: &Machine) -> bool {
    let os_skip = matches!(os, Some(o) if o != &machine.os);
    // A declared role gates, and an UNDECLARED machine role fails the gate
    // closed. Being lenient here meant a machine that simply omitted `role`
    // composed every `role = "desktop"` bundle and layered its GNOME extensions
    // and its rpms — precisely what this gate exists to stop, and the opposite
    // of what the comment on those fields promised. A bundle that names a role
    // is describing a group; a machine that names none is not in it.
    let role_skip = match (role, &machine.role) {
        (Some(r), Some(mr)) => r != mr,
        (Some(_), None) => true,
        (None, _) => false,
    };
    os_skip || role_skip
}

/// A machine's effective template vars: the global `[vars]` overlaid by the
/// machine's own `vars` (per-machine wins). Lets a Linux box override a
/// Mac-valued `BREW_PREFIX` without a second global table.
pub fn effective_vars(
    global: &std::collections::BTreeMap<String, String>,
    machine: &Machine,
) -> std::collections::BTreeMap<String, String> {
    let mut merged = global.clone();
    for (k, v) in &machine.vars {
        merged.insert(k.clone(), v.clone());
    }
    merged
}

/// Expand a leading `~/` against `$HOME`. Everything else is returned as-is.
pub fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn machine_with_vars(vars: &[(&str, &str)]) -> Machine {
        Machine {
            name: "kira".into(),
            os: "linux".into(),
            role: None,
            apps: vec![],
            packages: vec![],
            gnome_extensions: Vec::new(),
            brewfile: None,
            vars: vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            brew_trust: Vec::new(),
            ignore: Default::default(),
            dconf: vec![],
            git: None,
        }
    }

    #[test]
    fn per_machine_vars_override_global() {
        let mut global = BTreeMap::new();
        global.insert("BREW_PREFIX".to_string(), "/opt/homebrew".to_string());
        global.insert("SHARED".to_string(), "keep".to_string());
        let m = machine_with_vars(&[("BREW_PREFIX", "/home/linuxbrew/.linuxbrew")]);
        let eff = effective_vars(&global, &m);
        assert_eq!(eff["BREW_PREFIX"], "/home/linuxbrew/.linuxbrew"); // machine wins
        assert_eq!(eff["SHARED"], "keep"); // global survives
    }

    #[test]
    fn no_machine_vars_leaves_global_untouched() {
        let mut global = BTreeMap::new();
        global.insert("A".to_string(), "1".to_string());
        let m = machine_with_vars(&[]);
        assert_eq!(effective_vars(&global, &m), global);
    }

    fn doc(s: &str) -> toml::Value {
        s.parse().unwrap()
    }

    #[test]
    fn pull_mode_reads_fleet_git() {
        let v = doc("[git]\nauto_pull = true\n[[machine]]\nname=\"kira\"\nos=\"linux\"\n");
        assert_eq!(pull_mode_from(&v, Some("kira")), PullMode::FastForward);
        assert_eq!(
            pull_mode_from(&doc("[git]\nauto_pull = false\n"), Some("kira")),
            PullMode::Off
        );
        assert_eq!(
            pull_mode_from(
                &doc("[[machine]]\nname=\"kira\"\nos=\"linux\"\n"),
                Some("kira")
            ),
            PullMode::Off
        );
    }

    #[test]
    fn pull_mode_rebase_only_when_pull_on() {
        // auto_rebase alone (pull off) stays Off — rebase has no effect.
        assert_eq!(
            pull_mode_from(&doc("[git]\nauto_rebase = true\n"), Some("k")),
            PullMode::Off
        );
        // pull on + rebase on → Rebase.
        assert_eq!(
            pull_mode_from(
                &doc("[git]\nauto_pull = true\nauto_rebase = true\n"),
                Some("k")
            ),
            PullMode::Rebase
        );
    }

    #[test]
    fn machine_git_wholly_overrides_fleet_for_pull_mode() {
        // Machine sets it on though the fleet is off (or silent) → pulls.
        let on = "[git]\nauto_pull = false\n[[machine]]\nname=\"kira\"\nos=\"linux\"\n\
                  [machine.git]\nauto_pull = true\n";
        assert_eq!(
            pull_mode_from(&doc(on), Some("kira")),
            PullMode::FastForward
        );
        // …and a matching machine whose `[machine.git]` omits auto_pull is OFF
        // even when the fleet turns it on — the override replaces wholesale.
        // (Two machines so the sole-machine fallback doesn't mask the match.)
        let off = "[git]\nauto_pull = true\n\
                   [[machine]]\nname=\"kira\"\nos=\"linux\"\n[machine.git]\nremind = false\n\
                   [[machine]]\nname=\"other\"\nos=\"linux\"\n";
        assert_eq!(pull_mode_from(&doc(off), Some("kira")), PullMode::Off);
        // A *different* host with no override of its own falls to the fleet.
        assert_eq!(
            pull_mode_from(&doc(off), Some("other")),
            PullMode::FastForward
        );
    }

    #[test]
    fn pull_mode_sole_machine_fallback_when_host_unknown() {
        let v = doc("[[machine]]\nname=\"only\"\nos=\"linux\"\n[machine.git]\nauto_pull = true\n");
        assert_eq!(pull_mode_from(&v, None), PullMode::FastForward); // sole machine wins
    }

    fn machine(os: &str, role: Option<&str>) -> Machine {
        Machine {
            name: "m".into(),
            os: os.into(),
            role: role.map(String::from),
            apps: vec![],
            packages: vec![],
            gnome_extensions: Vec::new(),
            brewfile: None,
            vars: Default::default(),
            brew_trust: Vec::new(),
            ignore: Default::default(),
            dconf: vec![],
            git: None,
        }
    }

    #[test]
    fn gate_semantics() {
        let server = machine("linux", Some("server"));
        let desktop = machine("linux", Some("desktop"));
        let mac = machine("mac", Some("desktop"));
        let os_l = Some("linux".to_string());
        let role_d = Some("desktop".to_string());

        // A desktop-Linux bundle is skipped on a server and on a Mac...
        assert!(gated(&os_l, &role_d, &server)); // role mismatch
        assert!(gated(&os_l, &role_d, &mac)); // os mismatch
                                              // ...and applies on a Linux desktop.
        assert!(!gated(&os_l, &role_d, &desktop));
        // A bundle that gates on nothing applies everywhere.
        assert!(!gated(&None, &None, &server));
        assert!(!gated(&None, &None, &machine("linux", None)));

        // A role gate FAILS CLOSED against a machine that declares no role.
        //
        // This was lenient, and the leniency defeated the gate's stated purpose:
        // a server that simply omitted `role` composed every `role = "desktop"`
        // bundle and layered its GNOME extensions and rpms. A bundle naming a
        // role is describing a group; a machine naming none is not in it, and
        // guessing that it might be is how the gate silently did nothing.
        assert!(gated(&None, &role_d, &machine("linux", None)));
        // The os half is unaffected — it never had a "machine declares no os"
        // case to be lenient about, because os is required.
        assert!(!gated(&os_l, &None, &machine("linux", None)));
    }

    #[test]
    fn unknown_fields_are_rejected() {
        // The SPEC's headline guarantee: a typo names itself, never silently
        // drops. Covers the root table, [ignore], and an assert sub-check.
        assert!(toml::from_str::<TemperToml>("[ignore]\nflatpak = []\n").is_ok());
        assert!(toml::from_str::<TemperToml>("bogus_table = 1\n").is_err());
        assert!(toml::from_str::<Ignore>("flatpaks = []\n").is_err()); // typo'd sub-key
        assert!(toml::from_str::<Ignore>("flatpak = []\n").is_ok());
        assert!(toml::from_str::<ContainsLine>("file = \"x\"\nlyne = \"y\"\n").is_err());
        assert!(toml::from_str::<ContainsLine>("file = \"x\"\nline = \"y\"\n").is_ok());
    }

    #[test]
    fn version_ordering_is_numeric_and_lenient() {
        assert!(version_is_newer("1.41.0", "1.40.0"));
        assert!(version_is_newer("2.0.0", "1.99.99"));
        assert!(version_is_newer("1.40.10", "1.40.9")); // numeric, not lexical
        assert!(!version_is_newer("1.40.0", "1.40.0")); // equal is not newer
        assert!(!version_is_newer("1.39.0", "1.40.0"));
        // pre-release / build suffixes are ignored (core parts compared)
        assert!(!version_is_newer("1.40.0-rc1", "1.40.0"));
        // garbage parses to 0.0.0 → never spuriously newer
        assert!(!version_is_newer("not-a-version", "1.0.0"));
    }

    #[test]
    fn peek_stamp_and_mode_are_lenient() {
        // Both peeks must survive a file that FAILS the strict parse (unknown
        // field) — that's the whole point.
        let broken = "temper_version = \"9.9.9\"\n[update]\nmode = \"warn\"\nfuture_field = 1\n";
        assert_eq!(peek_version_stamp(broken).as_deref(), Some("9.9.9"));
        assert_eq!(peek_update_mode(broken), UpdateMode::Warn);
        // Absent → None / default(prompt).
        assert_eq!(peek_version_stamp("[vars]\nA=\"b\"\n"), None);
        assert_eq!(peek_update_mode("[vars]\nA=\"b\"\n"), UpdateMode::Prompt);
        // Truly malformed TOML → None / default (never panics).
        assert_eq!(peek_version_stamp("= = ="), None);
        assert_eq!(peek_update_mode("= = ="), UpdateMode::Prompt);
    }

    #[test]
    fn update_mode_parses_and_rejects_unknown() {
        let ok: TemperToml = toml::from_str("[update]\nmode = \"auto\"\n").unwrap();
        assert_eq!(ok.update.mode, UpdateMode::Auto);
        // default when the table/field is absent
        let def: TemperToml = toml::from_str("[vars]\nA=\"b\"\n").unwrap();
        assert_eq!(def.update.mode, UpdateMode::Prompt);
        // an unknown mode value is a parse error (names itself)
        assert!(toml::from_str::<TemperToml>("[update]\nmode = \"sometimes\"\n").is_err());
        // an unknown [update] sub-field errors (deny_unknown_fields)
        assert!(toml::from_str::<TemperToml>("[update]\nnope = true\n").is_err());
        // a stamped folder round-trips (the stamp is a known field now)
        assert!(toml::from_str::<TemperToml>("temper_version = \"1.2.3\"\n").is_ok());
    }

    #[test]
    fn stamp_is_prepended_updated_and_monotonic() {
        // Absent → prepended as a leading root key (survives a re-parse).
        let src = "[vars]\nEDITOR = \"hx\"\n";
        let stamped = stamp_version(src).unwrap();
        assert!(stamped.starts_with(&format!("temper_version = \"{VERSION}\"\n")));
        assert_eq!(peek_version_stamp(&stamped).as_deref(), Some(VERSION));
        assert!(stamped.contains("EDITOR = \"hx\"")); // original content preserved

        // Present-and-equal → byte-for-byte no-op (no git churn).
        assert_eq!(stamp_version(&stamped).unwrap(), stamped);

        // Present-and-NEWER → left untouched (never stamp down).
        let newer = "temper_version = \"999.0.0\"\n[vars]\nA = \"b\"\n";
        assert_eq!(stamp_version(newer).unwrap(), newer);
    }

    #[test]
    fn load_fleet_distinguishes_skew_from_typo() {
        use std::io::Write;
        let dir = tempfile::TempDir::new().unwrap();
        let write = |body: &str| {
            let mut f = std::fs::File::create(dir.path().join("temper.toml")).unwrap();
            f.write_all(body.as_bytes()).unwrap();
        };

        // Unknown field + a NEWER stamp → NewerVersion (a version issue).
        write("temper_version = \"999.0.0\"\nfuture_field = true\n");
        let err = load_fleet(dir.path()).unwrap_err();
        let nv = err.downcast_ref::<NewerVersion>().expect("should be NewerVersion");
        assert_eq!(nv.required, "999.0.0");
        assert_eq!(nv.mode, UpdateMode::Prompt);

        // Same unknown field but mode = off → plain parse error, not NewerVersion.
        write("temper_version = \"999.0.0\"\nfuture_field = true\n[update]\nmode = \"off\"\n");
        let err = load_fleet(dir.path()).unwrap_err();
        assert!(err.downcast_ref::<NewerVersion>().is_none());

        // Unknown field with NO newer stamp → genuine TOML issue → plain error.
        write("future_field = true\n");
        let err = load_fleet(dir.path()).unwrap_err();
        assert!(err.downcast_ref::<NewerVersion>().is_none());

        // A clean, current-stamped folder loads fine.
        write(&format!(
            "temper_version = \"{VERSION}\"\n[[machine]]\nname = \"m\"\nos = \"linux\"\n"
        ));
        assert!(load_fleet(dir.path()).is_ok());
    }
}
