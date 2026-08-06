# temper — Architecture

> **Status: design, sanity-checked.** Drafted from the design conversation,
> then checked against the **entire** ReinstallScripts repo (Mac tree, the
> 1,399-line Linux justfile, and the Linux libs + `install-bazzite.sh`) on
> 2026-07-27. The gaps that pass surfaced are folded in below. Still design, not
> code — but no longer un-vetted.

## What temper is

`temper` converges a machine to a declared spec. You describe what a machine
should be — its packages and its configuration — in a folder of human-readable
files, and `temper` makes the machine match, reports what's out of sync, and can
revert what it changed.

It is the generalization of the `ReinstallScripts` repo: ~3,900 lines of bash
(two aligned platform trees, `just` + `lib/*.sh`) that install and configure
Jakob's fleet of Macs and Linux boxes. That repo works, but its logic is
temper-private glue, duplicated byte-for-byte across platforms, and inseparable
from Jakob's own machines. `temper` extracts the *engine* into one open tool and
leaves the *data* in a private folder anyone can bring.

> **ReinstallScripts is the acceptance spec.** It is proven on Jakob's live
> fleet, so *it* — not this document — is the authority on **what temper must
> do**. Every RIS recipe must have a temper equivalent: a binary verb, or, only
> where a genuine constraint forbids a verb (the bootstrap paradox), a companion
> script that delivers the same result. Where these docs describe behavior that
> disagrees with a working RIS recipe, treat the **doc as the bug** — correct the
> doc (or temper) to match RIS, never dismiss the RIS behavior. "temper does it
> differently on purpose" is only legitimate once the difference is proven at
> least as good on a real machine; until then, RIS wins.

### The core split: engine vs data

- **The engine is the tool.** Public, on the Homebrew tap
  (`jakobhviid/homebrew-tap`, beside `grove`/`amdl`/`dotsync`), identical for
  everyone. It contains **zero** knowledge of any specific machine, person, or
  app.
- **The data is a folder.** Private, per-person. It holds the manifest and the
  real config files. Jakob keeps his in a **git** repo; his wife keeps hers in
  **Nextcloud**; a stranger uses a **USB disk**, Dropbox, or a plain directory.

This is the same move `dotsync` already made to `machine-sync`: take a private
`just`+`yq` shell tool and ship it as an open, manifest-driven Rust CLI where
the config lives in the user's own folder. `temper` does that for provisioning
instead of dotfile-sync, and reuses dotsync's `adopt` verb and mode-enforcement.

### Delivery-agnostic

**`temper` does not *manage* its config folder with git (or any sync client).**
It operates on "a folder that contains a manifest." How that folder arrived —
git clone, a continuously-synced cloud folder, a mounted USB disk, `rsync` — is
not temper's concern. (An `exec` *step* may still shell out to `git`/`curl` for a
specific job, e.g. cloning a tmux plugin manager — that's a step doing work, not
temper managing the folder.)

Folder discovery (built, `discovery.rs`) — first hit wins:

1. **`$TEMPER_DIR`** — explicit override.
2. **Walk up from the cwd** — you're inside the folder (or a subdir of it).
3. **A saved pointer** — `temper setup <dir>` writes `$XDG_CONFIG_HOME/temper/home`.
4. **Auto-scan** — a directory named `steel`, `temper-home`, or `.temper` under
   any of: `~`, a dev parent (`~/Developer`, `~/developer`, `~/dev`, `~/src`,
   `~/code`, `~/projects`, `~/git`, `~/repos`, … — so case/name conventions don't
   matter), or a cloud-sync root (the same set `dotsync` probes: each
   `~/Library/CloudStorage/*` client, iCloud Drive, `~/Nextcloud`, `~/Dropbox`,
   `~/OneDrive`, `~/ProtonDrive`, `~/Google Drive`, `~/Sync`), `/media`,
   `/run/media/$USER`.

So a folder cloned/synced to e.g. `~/steel` or `~/Developer/steel` is found with
**no configuration** — that's why machines "just know where steel is." A fresh
box with none of these errors with a message telling you to `temper setup <dir>`
or set `$TEMPER_DIR`. (Discovery only *locates* the folder — temper never clones
or syncs it; that's git/Nextcloud/rsync's job.)

### Humans and LLMs both compose it

The folder is a browsable tree of **real files** — a real Brewfile, a real
`starship.toml`, a real dconf dump — plus one manifest that ties them together.
The bar is "as readable as a Brewfile." The CLI carries the `amdl` house style:
`--json` on every command, an `--llm` guide, a global `-v/--verbose`,
journaled `undo`, human output to stdout / progress + errors to stderr.

A long converge is the one place that house style needs help: hushing the package
managers is right (nobody needs 200 "Using x" lines), but silence for the twenty
minutes it takes to pour `llvm` and `mactex` reads as a hang. So the quiet path
**captures** the manager's output and renders a `grove`-style spinner naming the
package in flight, replaying the whole log if the converge fails and surfacing
warnings even when it succeeds. temper reads Homebrew's `ohai` lines
(`==> Installing …`, `==> Pouring …`) purely as a progress feed — never as a
source of truth about what is installed; that always comes from a probe.

**Capture is not only about noise — it is about voice.** Every child temper shells
out to has its own opinion of the world: `flatpak update` says "Nothing to update."
about *its remotes*, `git pull` says "Already up to date." about *the folder's
upstream*, `brew trust` says "Already trusted tap". Left on the terminal, any of
them reads as temper's verdict on the run, moments before temper installs and
upgrades plenty — and, going to temper's stdout, breaks `--json` outright. So
every converge child goes through one door (`providers::run_child`) and every
phase reports its own effect in temper's words. Three rules fall out, and they
apply to new code as much as old:

1. **A child's output never stands as temper's.** Capture, replay on failure, and
   let warnings through. Stream only where the child's output *is* the operation:
   `prune`'s removals (destructive, confirmed, the user is watching) and the
   self-update's `brew upgrade temper -y`.
2. **Report the effect, never the invocation.** Not "we ran the upgrade" but how
   many packages moved version; not "we called push" but whether the remote moved;
   not "we pulled" but how many commits landed. Measured, never assumed.
3. **Never parse a tool's prose to learn what happened.** git and flatpak
   localize their messages, so string-matching works on the author's machine and
   silently stops working on someone else's. Compare refs, versions, hashes —
   things that mean the same in every locale. (Homebrew's `ohai` lines are the one
   exception, and only as a *progress label*, never as truth.)

The user-facing consequence: **silence means converged.** A run prints a live
region while it works (erased when done), a `✓` line only for something that
actually changed, warnings and errors always, and one summary at the end. `-v`
turns the children back on in full.

---

## Two scopes

Configuration lives at two scopes, and the distinction is load-bearing.

### Machine scope — aggregate / snapshot

Things that must be computed *whole* to be correct, or that represent a
machine-wide state:

- **The effective package set** (brew / flatpak / mas / gext / rpm-ostree).
  Package drift is a dependency-closure computation, not a set subtraction (see
  below), so it can only be evaluated against the *complete* declared set for a
  machine. The declared set = union of composed apps' packages **+ a per-machine
  loose list + minus an ignore/baseline list** (see "Machine model").
- **Whole-desktop dconf** — the entire GNOME shell / Ptyxis state. Backed up
  through a configurable **strip-keys** filter (bookkeeping + per-monitor panel
  keys that would corrupt a backup→restore round-trip). The filter is a manifest
  field, not tool-baked knowledge.

Drift at machine scope is a set/snapshot operation.

### App scope — the composable library

The per-app recipes: a config file to deploy, a key to set, a one-time setup
script to run. The open, composable library. Each machine picks which
app-bundles it wants. Drift at app scope is per-file / per-key / per-assertion.

---

## The taxonomy: three layers, two are code

1. **Primitives — closed set, tool code (Rust).** The atomic operations. Adding
   one is a big deal (new release, new drift/undo logic). This set is the tool's
   entire surface area.
2. **App-bundles — open set, user config (no code).** A named, ordered list of
   primitive steps, each OS/role-gated. Where ghostty and 1Password live. "The
   next ghostty clone" is a new *config file you write*, never a tool release.
3. **Machine registration (`temper.toml`).** Which machines exist (name + OS +
   role) and which bundles + loose packages + ignores each one composes.

### The primitives (closed set)

| Primitive | Scope | What it does |
|---|---|---|
| `copy` | app | deploy a file/dir → target(s). Modes: `verbatim`, `template` (variable + apply-time-probe substitution), `seed` (create-once, then hands-off, excluded from drift). Fields: `to`, `mode` (file perms), `template`. |
| `block` | app | ensure a marker-delimited block / line is present in a user-owned file, idempotently (the grove-`setup` pattern: SSH `Include`, zshrc `source` line). |
| `setkey` | both | set one or more keys in a structured store, preserving siblings. **Backends:** `dconf`, macOS `defaults`, `ini`/`.desktop`, `json`, `toml`. Supports **list-append** (json/toml/dconf array-union, e.g. dconf `custom-keybindings`) and opt-in apply-time value templating (`template = true`, all backends). The `json` backend is **JSONC/comment-preserving** (reads `//` + trailing commas, writes without reformatting). Generalizes the old standalone `dconf`. |
| `brew` | machine | converge the aggregate Brewfile (`brew bundle`); internalizes tap-trust (and drift-checks it both ways vs `brew trust --json`); knows the `vscode` sub-type |
| `flatpak` | machine | converge the flatpak set (with ignore-list); `flatpak override` env/perms is a `setkey`-style op on the override store |
| `mas` | machine | converge Mac App Store apps in a separate, **forgiving** `mas install` loop — skips apps already installed (per `mas list`), mutes Spotlight-reindex noise (`MAS_NO_AUTO_INDEX`), and a MAS failure is warned + skipped, never fatal (see below) |
| `gext` | machine | converge GNOME extensions (install from EGO + `gext update`); distinct from *enabling* them (a dconf key) |
| `rpm-ostree` | machine | layer an rpm that can't be image-baked (proton-vpn); emits a **reboot-required** signal temper reports but never automates |
| `profile` | app/machine | install a macOS `.mobileconfig` — **weaker contract** (apply is a GUI `open`; drift is status-only `manual`; not silently undoable). Idempotent across runs via a content stamp — re-opened only when the source `.mobileconfig` changed |
| `sysfile` | app | write one **root-owned** system file (`/etc/…`) with mode/owner/group, escalating internally (`sudo install`) for just that write. Drift compares content + mode + owner; not journaled (system-side) |
| `exec` | app | run a user-supplied script — the escape hatch (see "exec's contract") |

Every primitive is **planned and drift-checked**. The **file-writing** ones
(`copy`, `block`, `setkey` json/toml/ini) are **journaled** for `undo` (the
`plan.rs`/`apply.rs` shape from `dotsync`, the journal from `amdl`), and so is
**`setkey(dconf)`** — dconf values round-trip cleanly, so undo snapshots the
prior value and restores it (or resets a previously-unset key). **`setkey
(defaults)`**, `sysfile`, and `exec` stay **not journaled**: `defaults read`
loses the value's type (an undo couldn't rewrite it faithfully), and `sysfile`/
`exec` mutate root-owned/arbitrary state — so `undo` can't revert them. All the
system-side backends degrade to `unavailable` in drift when their tool is absent
rather than aborting.

### Dynamic (apply-time) values

`template` (`copy`) and `setkey` (opt-in `template = true`) values may be
**resolved from live state at apply time**, not just from declared vars:
`{{ which "ghostty" }}` (absolute path — GNOME's PATH excludes the brew prefix,
so keybinding commands must be resolved on the box), plus `{{ env "…" }}`,
`{{ var "…" }}`, and `{{ brew_prefix }}`. `setkey` is literal by default (static
drift = trivial equality); `template = true` opts a value in and renders its
string leaves on every backend. Drift on a dynamic value compares
**semantically** (does the current value equal the re-resolved probe?), never
byte-for-byte — a byte compare would report permanent false drift.

### Composition modes live in providers, not the schema

There is **no generic `merge` mode**. dconf, ghostty, and Brewfile each need a
different algorithm, so a universal "merge" would be a lie. `copy` is
`verbatim`/`template`/`seed`; structured key-*setting* (preserve siblings) is
`setkey`, parameterized by backend; whole-file/whole-subtree *merges* (dconf
snapshot load order, the extensions-sync union-add) are **provider-internal**.

---

## Package managers = converge + probe

Every package manager is two operations:

- **converge** — aggregate install + whole-set drift: `brew bundle`,
  `flatpak install`-set, `mas install`-set, `gext install`-set, `rpm-ostree`.
- **probe** — "is *this one* present?": `brew list` / `flatpak info` /
  `mas list` / `gext` / `rpm -q` / `command -v` / path-exists.

### Compile-time compose, run-time aggregate (the brew rule)

App-bundles declare packages as **pure data** tokens. `temper` collects the
**union** of a machine's composed apps' packages **+ its loose list, minus its
ignore list**, synthesizes **one** effective Brewfile per manager, and makes
**one** converge call. Composition happens on paper; each manager runs once.

This is required for *correctness*, not speed: `brew bundle cleanup` is
dependency-aware — a formula kept only as another package's transitive dependency
is **not** listed as an extra. Split brew per-app and cleanup can't distinguish a
real orphan from a shared dependency; drift gets **wrong**. (RIS documents this
in `brew_cleanup_extras`: "*a kept entry's deps are NOT listed — which is why
this can't be replaced by naive set subtraction*.")

**MAS.** `mas` is converged **separately** from the aggregate `brew bundle`, in
its own `mas install` loop, because it is the flakiest provider (no App Store
sign-in, an app not tied to the Apple ID). A MAS failure is **warned (to stderr)
and skipped**, never fatal — so it can't abort the rest of a converge
(Principle #6). Sign in to the App Store first for the installs to succeed.

### The gate: presence probes config

> **Status: BUILT.** `when`/`needs` presence gating is implemented (`probe.rs`);
> steps also still gate on `os`/`role`.

Config runs in a second phase; each app's steps are **gated on a presence
probe** — "is this actually here? → run my config." The gate checks *reality,
not intent*: on Linux, **Ghostty is baked into the image and in no Brewfile**, so
a gate of `when = { binary = "ghostty" }` fires correctly however it was
installed. Probe vocabulary (declarative, exactly one per probe): `binary` /
`brew` / `cask` / `flatpak` / `mas` / `gext` / `rpm` / `path` / `exec`. `when`
skips the step when the probe fails; `needs` errors (a hard requirement).

**Skips are loud** (Principle #6): install/update print
`⚠ ghostty · copy ~/.config/ghostty/config — skipped: binary \`ghostty\` absent`
as the phase reaches the step, and `drift` reports the gated-out step status-only
(never as red drift). (The implicit "my declared package is installed" default is
not inferred — declare the probe explicitly.)

### The cask-artifact exception (a named Principle-#2 violation)

App-scope config sometimes patches `.desktop` files that brew records as **cask
artifacts** (1Password, VS Code). The *next* machine-scope `brew bundle` then
refuses to upgrade the modified artifact, so it must be reset to pristine first,
then re-patched. This is a genuine app→machine effect-dependency that no clean
two-phase model absorbs. temper handles it explicitly: a cask can be **annotated
"config patches my artifacts → reset-before-converge,"** and the brew provider
honors it. Documented as an exception rather than pretended away.

---

## Drift is a subsystem, not a diff

Drift is more than "package-set + file-byte + key." It also evaluates
declarative **assertions** and **exec drift-hooks**, and reports **status-only**
items:

- **`assert`** — checks that aren't a converge action: `absent` (must-not-exist —
  `~/.zshrc.local`, retired PWAs), `mode` (root:root 0755 on
  `/etc/1password/…`), `contains-line` (`~/.zshrc` sources `.image`),
  `not-member` (user not in `onepassword` group), `executable-resolves` (a
  keybinding command is on PATH), `json-semantic` (order-independent
  missing-vs-extra — the brave policy), `shell` (default login shell).
- **exec drift-hook** — an `exec` step may supply a companion **check** script
  that reports in/out-of-sync (exit code + message). Without it, anything pushed
  to `exec` loses drift coverage.
- **status-only** — items temper drift-*reports* but has no verb to repair (image
  origin, the image-baked brave policy). Read-only.

This is where a third of `just drift`'s real value lived; the model now has a
home for it.

---

## Lifecycle

Steps declare which flows they participate in; the default derives from the
primitive, so the modifier is written only for exceptions.

| Value | Runs during | Notes |
|---|---|---|
| `always` | install + update | default for `copy`/`template`/`setkey`; re-applied and drift-tracked |
| `install` | install only | default for `seed`, `profile`, one-time `exec`; update skips (reloading whole-desktop dconf clobbers live tweaks) |
| `ensure` | install + update, **install-if-missing only** | the corrected "update installs a little": backfill `grove`/`amdl`/`pwtune` and the zsh tool set if absent, without upgrade-churn |
| `manual` | never automated | only when explicitly invoked (`gnome-restore`, `speaker-eq`, EQ import) |

Enforcement steps that today re-run every `update` (git identity via
`git config`, default shell via `chsh`) are `exec` with `run = always` + a drift
hook, so `update` keeps re-asserting them.

The two automated flows:

- **`temper install [machine]`** — full converge: add missing packages, apply
  everything, run one-time setup + profiles + dconf reload.
- **`temper update`** — upgrade packages + re-apply `always` + honor `ensure`
  (install-if-missing for the allowlist). Does **not** add newly-declared apps
  wholesale — adding an app is an `install`. *(Corrected from the first draft's
  "update never installs," which the repo disproved.)*

---

## exec's contract

`exec` is the pressure valve, but its execution *context* is now defined, not
assumed:

- **Privilege** — a step may declare it needs `sudo` (the `/etc/1password/…`
  edits, `rpm-ostree`). `plan` shows it; `undo`/journal semantics for privileged
  system mutations are best-effort and labeled as such.
- **One password per run** — root is needed in several unrelated places (a
  pkg-based cask's installer, a `sysfile` write, `rpm-ostree`), each of which
  would otherwise prompt on its own schedule, minutes apart, because sudo's
  timestamp expires during the downloads between them. A mutating run therefore
  determines up front whether it needs root at all (`providers::casks_needing_root`
  — a batched cask-artifact query over just the packages this run would touch),
  asks **once** with a reason if so, and keeps the timestamp warm for the duration
  (`sudo::keep_alive`, a `sudo -n -v` refresh that can never itself prompt).
  Nothing needed → no prompt; `--dry-run` and every read-only verb never ask.
- **Secrets / env** — a step may declare env vars / a `secrets/` source to pass
  through (the `ACOUSTID_KEY` amdl case). The private folder makes a `secrets/`
  dir viable; this is the mechanism that consumes it.
- **Drift hook** — the optional companion check script above.

---

## Engine operations

All `--json`-capable, all with an `--llm` guide, mutating ones journaled for
`undo`:

- **`install [machine]`** / **`update`** — the two lifecycle flows. A live
  `install` refuses to run when the machine's `os` ≠ the host os (drift and
  `--dry-run` work from any host; only a converge is host-guarded). `manual`
  steps are skipped by both flows.
- **`drift [machine]`** — read-only: package set + tap-trust (`[brew].trust` vs
  `brew trust --json`, both directions) + every managed file + keys + assertions
  + exec-hooks. Findings are `ok` / `drifted` / `missing` / `untrusted` /
  `trusted-extra` / **`unavailable`** (a backend whose tool is absent here, e.g.
  dconf on a Mac — degraded, not a failure); `manual` steps and image-baked items
  are status-only, never counted as drift.
- **`prune`** — remove installed-but-not-declared (dependency-aware, honoring the
  ignore/baseline list), and `brew untrust` any tap trusted on the machine but
  not in `[brew].trust` (the machine→spec mirror of `reconcile`'s trust absorb);
  previews and confirms first (`--yes` skips; under `--json` it previews unless
  `--yes`).
- **`backup [machine]`** — dump live package state into the folder
  (`brew bundle dump` → the machine's own `brewfile`, the file it reads; else
  `machines/<name>/Brewfile`), plus each declared
  `[[machine.dconf]]` snapshot dumped through its strip-keys filter to its file.
- **`restore [machine]`** — load the machine's dconf snapshot(s) back into live
  dconf (confirm-gated, `--yes` to skip). Clobbers live desktop tweaks, so it is
  a standalone verb, **never** part of `update` (RIS excludes gnome-restore from
  its update for the same reason).
- **`adopt`** — report installed extras not in the spec (advisory / non-mutating)
  so you can add each to a bundle, the machine loose list, or `[ignore]`. The
  read-only sibling of `reconcile`.
- **`reconcile [machine]`** — the interactive spec←machine capture (RIS's
  `reconcile`): per-item, absorb installed-but-undeclared extras INTO the
  machine's own Brewfile, drop declared-but-absent entries FROM it, or route a
  flatpak extra to `[ignore]` (comment-preserving, via toml_edit). It also
  reconciles **tap-trust**: absorb a trusted-but-undeclared tap into
  `[brew].trust` (or `[ignore].tap`), or drop a declared-but-untrusted tap from
  it — the same both-direction diff, written to `temper.toml` via toml_edit.
  Missing entries default to *keep*, extras default to *skip*; a unified preview
  + one confirm precede any write. Edits only the machine's **own** `brewfile`
  (and the fleet `temper.toml` for `[brew].trust`/`[ignore]`), never a shared
  bundle. `--json` previews the plan without prompting. Converging the other way,
  machine←spec, *is* `install`/`update`.
- **`undo [run]`** — revert a run — the one named by its id, else the newest;
  **`undo --list`** enumerates revertible runs (read-only). amdl's
  content-addressed journal: after-hash-guarded reverts skip-and-report (a file
  changed since, or a missing object) rather than clobber or abort mid-run.

---

## Delivered outside the temper *binary* (still replicated — not refused)

RIS-parity is the goal, so nothing RIS does is dropped. A few RIS jobs are
delivered by something other than a temper *verb* — because of a genuine
constraint, not a scope preference. RIS itself delivers these outside its `just`
recipes too (a `bootstrap.sh`, an image tier), so this is parity, not a gap.

- **Bootstrap** — getting brew + temper onto a bare machine runs *before* the tap
  (and thus temper) exists — the paradox. It stays a small companion shell script
  (`install-bazzite.sh`'s bootstrap tier + the `curl | sh` fallback), exactly as
  RIS bootstraps with `bootstrap.sh`. Phase-1 image work (cosign key,
  `policy.json` JSON-merge, signed `rpm-ostree rebase`, reboot) rides there too.
- **Image-side system layer** — the OS image bakes browsers, CLI baseline, brave
  policy, etc. Building the image is a different *artifact* (the Stacks repo), the
  same split RIS draws with `install-bazzite.sh`. temper *configures* a machine on
  top of that image; drift still reports image-baked items status-only.
- **`eq-import` — folder-authoring, and built.** The `eq-import` verb shallow-
  clones the configured `[eq_import].repo` and lands each `<x>.calibrated.conf`
  as `<dest>/<x>.conf`. It writes *into* the config folder (authoring) rather
  than converging a machine — the one clearly-labelled Principle-#9 exception —
  so it lives as its own verb, not a converge step. Was a working RIS recipe;
  now replicated, not carved out.

---

## Folder layout — building your own "steel"

temper *requires* only `temper.toml` at the root; everything else is convention.
The recommended shape (app-first recipes, real files under `assets/`):

```
<your-steel>/            a git repo, a synced cloud folder, or a USB copy
  temper.toml            machines (name/os/role) + apps + loose pkgs + [vars] + [ignore] + [brew] + [eq_import]
  apps/                  one file per app — the composable, code-free recipes
    shell.toml           copy/block/setkey/exec steps, os/role/when-gated
    ghostty.toml
    1password.toml       e.g. setkey keybinding + exec(sudo) NMH setup + a sysfile /etc write
  assets/                the real files the recipes deploy
    starship.toml  ghostty.config  gnome/shell.<machine>.dconf  …
  brewfiles/             optional per-machine Brewfiles (a machine's `brewfile = "brewfiles/<name>"`)
    <machine>
  machines/              `temper backup` fallback dump dir (when a machine has no `brewfile`)
  secrets/               git-ignored; consumed by exec/setkey steps that declare them
```

Get the folder onto a box however you like, then let temper find it (§discovery:
drop it at a scanned location like `~/steel` or `~/Developer/steel`, or run
`temper setup <dir>`). See `SPEC.md` for the schema of each file, `WORKFLOWS.md`
for the day-to-day loops, and `PRINCIPLES.md` for the guardrails.
