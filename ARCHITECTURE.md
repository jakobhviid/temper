# fleet — Architecture

> **Status: design, sanity-checked.** Drafted from the design conversation,
> then checked against the **entire** ReinstallScripts repo (Mac tree, the
> 1,399-line Linux justfile, and the Linux libs + `install-bazzite.sh`) on
> 2026-07-27. The gaps that pass surfaced are folded in below. Still design, not
> code — but no longer un-vetted.

## What fleet is

`fleet` converges a machine to a declared spec. You describe what a machine
should be — its packages and its configuration — in a folder of human-readable
files, and `fleet` makes the machine match, reports what's out of sync, and can
revert what it changed.

It is the generalization of the `ReinstallScripts` repo: ~3,900 lines of bash
(two aligned platform trees, `just` + `lib/*.sh`) that install and configure
Jakob's fleet of Macs and Linux boxes. That repo works, but its logic is
fleet-private glue, duplicated byte-for-byte across platforms, and inseparable
from Jakob's own machines. `fleet` extracts the *engine* into one open tool and
leaves the *data* in a private folder anyone can bring.

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
the config lives in the user's own folder. `fleet` does that for provisioning
instead of dotfile-sync, and reuses dotsync's `adopt` verb and mode-enforcement.

### Delivery-agnostic

**`fleet` does not *manage* its config folder with git (or any sync client).**
It operates on "a folder that contains a manifest." How that folder arrived —
git clone, a continuously-synced cloud folder, a mounted USB disk, `rsync` — is
not fleet's concern. (An `exec` *step* may still shell out to `git`/`curl` for a
specific job, e.g. cloning a tmux plugin manager — that's a step doing work, not
fleet managing the folder.)

Folder discovery reuses `dotsync`'s model (`discovery.rs`): `$FLEET_DIR` → a
saved pointer in per-machine config → auto-scan common locations → prompt. First
run on a fresh machine offers setup automatically.

### Humans and LLMs both compose it

The folder is a browsable tree of **real files** — a real Brewfile, a real
`starship.toml`, a real dconf dump — plus one manifest that ties them together.
The bar is "as readable as a Brewfile." The CLI carries the `amdl` house style:
`--json` on every command, an `--llm` guide, `indicatif` progress bars,
journaled `undo`, human output to stdout / progress + errors to stderr.

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
3. **Machine registration (`fleet.toml`).** Which machines exist (name + OS +
   role) and which bundles + loose packages + ignores each one composes.

### The primitives (closed set)

| Primitive | Scope | What it does |
|---|---|---|
| `copy` | app | deploy a file/dir → target(s). Modes: `verbatim`, `template` (variable + apply-time-probe substitution), `seed` (create-once, then hands-off, excluded from drift). Fields: `to`, `mode` (file perms), `template`. |
| `block` | app | ensure a marker-delimited block / line is present in a user-owned file, idempotently (the grove-`setup` pattern: SSH `Include`, zshrc `source` line). |
| `setkey` | both | set one or more keys in a structured store, preserving siblings. **Backends:** `dconf`, macOS `defaults`, `ini`/`.desktop`, `json`, `toml`. Supports **list-append** (array-union, e.g. `custom-keybindings`) and **dynamic values** (see below). This is the generalization of the old standalone `dconf`. |
| `brew` | machine | converge the aggregate Brewfile (`brew bundle`); internalizes tap-trust; knows the `vscode` sub-type |
| `flatpak` | machine | converge the flatpak set (with ignore-list); `flatpak override` env/perms is a `setkey`-style op on the override store |
| `mas` | machine | converge Mac App Store apps — **forgiving** (see below) |
| `gext` | machine | converge GNOME extensions (install from EGO + `gext update`); distinct from *enabling* them (a dconf key) |
| `rpm-ostree` | machine | layer an rpm that can't be image-baked (proton-vpn); emits a **reboot-required** signal fleet reports but never automates |
| `profile` | app/machine | install a macOS `.mobileconfig` — **weaker contract** (apply is a GUI `open`; drift is a plist key-subset compare; not silently undoable) |
| `exec` | app | run a user-supplied script — the escape hatch (see "exec's contract") |

Every non-`exec` primitive implements **plan → apply → drift → undo**
(the `plan.rs`/`apply.rs` shape from `dotsync`, the journal from `amdl`).
"Platform-specific" describes *where* a primitive runs, never *whether* it
behaves — a Linux-only `setkey(dconf)` is still dry-runnable, drift-checked, and
journaled.

### Dynamic (apply-time) values

`template` and `setkey` values may be **resolved from live state at apply time**,
not just from declared vars: `{{ which "ghostty" }}` (absolute path — GNOME's
PATH excludes the brew prefix, so keybinding commands must be resolved on the
box), `{{ sink match "…" }}` (the speaker-eq target sink). Drift on a dynamic
value compares **semantically** (does the current value equal the re-resolved
probe?), never byte-for-byte — a byte compare would report permanent false drift.

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

App-bundles declare packages as **pure data** tokens. `fleet` collects the
**union** of a machine's composed apps' packages **+ its loose list, minus its
ignore list**, synthesizes **one** effective Brewfile per manager, and makes
**one** converge call. Composition happens on paper; each manager runs once.

This is required for *correctness*, not speed: `brew bundle cleanup` is
dependency-aware — a formula kept only as another package's transitive dependency
is **not** listed as an extra. Split brew per-app and cleanup can't distinguish a
real orphan from a shared dependency; drift gets **wrong**. (RIS documents this
in `brew_cleanup_extras`: "*a kept entry's deps are NOT listed — which is why
this can't be replaced by naive set subtraction*.")

**MAS is the forgiving provider.** It needs an App Store sign-in and can only
install apps already associated with the Apple ID, so converge failures are
**reported, not fatal** (`mas: skipped Xcode (not signed in)`). brew/flatpak may
be strict; MAS must not fail the whole run. (Note: on macOS, mas lines ride
`brew bundle` today; the aggregate is still one call.)

### The gate: presence probes config

Config runs in a second phase; each app's steps are **gated on a presence
probe** — "is this actually here? → run my config." The gate checks *reality,
not intent*: on Linux, **Ghostty is baked into the image and in no Brewfile**, so
a gate of `binary: ghostty` fires correctly however it was installed. Probe
vocabulary (declarative): `binary` / `brew` / `cask` / `flatpak` / `mas` / `gext`
/ `rpm` / `path` / `exec`. Default = "the package(s) I declared are installed."

**Skips must be loud** (Principle #6): `skipped 1password config: probe failed`.

### The cask-artifact exception (a named Principle-#2 violation)

App-scope config sometimes patches `.desktop` files that brew records as **cask
artifacts** (1Password, VS Code). The *next* machine-scope `brew bundle` then
refuses to upgrade the modified artifact, so it must be reset to pristine first,
then re-patched. This is a genuine app→machine effect-dependency that no clean
two-phase model absorbs. fleet handles it explicitly: a cask can be **annotated
"config patches my artifacts → reset-before-converge,"** and the brew provider
honors it. Documented as an exception rather than pretended away.

---

## Drift is a subsystem, not a diff

Drift is more than "package-set + file-byte + key." It also evaluates
declarative **assertions** and **exec drift-hooks**, and reports **status-only**
items:

- **`assert`** — checks that aren't a converge action: `absent` (must-not-exist —
  `~/.zshrc.local`, retired PWAs), `mode`/`owner` (root:root 0755 on
  `/etc/1password/…`), `contains-line` (`~/.zshrc` sources `.image`),
  `not-member` (user not in `onepassword` group), `executable-resolves` (a
  keybinding command is on PATH), `json-semantic` (order-independent
  missing-vs-extra — the brave policy), `shell` (default login shell).
- **exec drift-hook** — an `exec` step may supply a companion **check** script
  that reports in/out-of-sync (exit code + message). Without it, anything pushed
  to `exec` loses drift coverage.
- **status-only** — items fleet drift-*reports* but has no verb to repair (image
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

- **`fleet install [machine]`** — full converge: add missing packages, apply
  everything, run one-time setup + profiles + dconf reload.
- **`fleet update`** — upgrade packages + re-apply `always` + honor `ensure`
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
- **Secrets / env** — a step may declare env vars / a `secrets/` source to pass
  through (the `ACOUSTID_KEY` amdl case). The private folder makes a `secrets/`
  dir viable; this is the mechanism that consumes it.
- **Drift hook** — the optional companion check script above.

---

## Engine operations

All `--json`-capable, all with an `--llm` guide, mutating ones journaled for
`undo`:

- **`install [machine]`** / **`update`** — the two lifecycle flows.
- **`drift [machine]`** — read-only: package set + every managed file + keys +
  assertions + exec-hooks. Reports per-app *present & drifted* vs *absent &
  N/A*, and *status-only* items.
- **`prune`** — remove installed-but-not-declared (dependency-aware, honoring the
  ignore/baseline list). Journaled.
- **`backup [machine]`** — whole-dump live state into the folder (effective
  package set, dconf snapshot through the strip filter).
- **`adopt`** — *interactive* spec←machine capture (dotsync's verb): for each
  drifted extra, offer **add to a bundle / add to the machine loose list / add to
  ignore**. This is what RIS mis-named `reconcile`. (There is no separate
  machine←spec `reconcile` verb — that *is* `install`/`update`.)
- **`undo`** — revert the last mutating run (amdl's content-addressed journal:
  minimal-inverse entries, after-hash-guarded reverts that skip-and-report
  rather than clobber).

---

## What stays out of fleet

- **Bootstrap** — brew + fleet onto a bare machine runs *before* the tap exists
  (the paradox). Stays a small shell script (`install-bazzite.sh`'s bootstrap
  tier + the `curl | sh` fallback). Phase-1 image work (cosign key, `policy.json`
  JSON-merge, signed `rpm-ostree rebase`, reboot) stays there too.
- **Image-side system layer** — the OS image bakes browsers, CLI baseline, brave
  policy, etc. fleet configures a machine; it does not build the image.
- **Folder-authoring tools** — `eq-import` (clone an upstream repo *into* the
  folder) is authoring, not machine-converge; like `grove`, it's a separate
  concern, not a fleet verb.

---

## Folder layout (proposal)

```
<fleet-home>/                  git for Jakob · Nextcloud for his wife · USB for a stranger
  fleet.toml                   machines (name/os/role) + apps + loose packages + ignores
  apps/
    ghostty.toml               copy config (all OS) + setkey dconf shortcut (linux)
    1password.toml             setkey keybinding + exec(sudo) NMH setup (linux)
    base.toml                  the CLI baseline as a bundle
  assets/                      the real files the recipes reference
    ghostty.config  starship.toml  …
  machines/
    chronos/   loose Brewfile, dconf snapshot (+ strip-keys applied on backup)
    kira/      his wife's machine — in HER folder
  secrets/                     consumed by exec/keyset steps that declare it
```

> Organization is app-first for recipes (a file per app), tiered *assets*
> referenced by path, machine-scope loose packages + dconf snapshots under
> `machines/`.

See `SPEC.md` for the concrete manifest schema and `PRINCIPLES.md` for the
guardrails.
