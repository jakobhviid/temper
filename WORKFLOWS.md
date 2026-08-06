# WORKFLOWS.md — how you operate temper

The day-to-day loops, not the schema. `SPEC.md` is *what you write*; this is
*what you run* and when. Compiled into `--llm` so an agent can operate a temper
folder, not just author one.

> **Status: confirmed against Jakob's habits** (cadence, absorb-direction, fleet
> model, restore) and grounded in ReinstallScripts. Each loop maps a RIS recipe
> onto temper's verbs.

The mental model: a machine has a **declared spec** (the folder) and a **live
state**. Every loop either converges the machine toward the spec
(**machine→spec**) or absorbs the machine's reality into the spec
(**spec←machine**). `drift` shows the gap and names both directions.

**How Jakob works it (the short version):** `drift` is the hub — run it, read the
**Next steps**, run the command it names. Absorbing an ad-hoc change goes through
`reconcile` (per-item), not a wholesale `backup`. The fleet is **authored in one
place** (this folder, in git) and **run per-machine** (locally, or over ssh — see
§Fleet). `restore` is used both when bringing a desktop back up *and* mid-life to
snap GNOME/Ptyxis back to the known-good snapshot.

Throughout, every verb runs against **this machine** (resolved by hostname); a
machine name is **optional** — pass one only to target or preview another (a
live `install <name>` then asks to confirm). The examples below omit it, matching
what `drift`'s Next steps print.

---

## 1. Bring up a machine (fresh install / reinstall)

**When:** a new box, or a reinstalled one, that already has brew + temper
(bootstrap is out of scope — a separate script).

```sh
temper setup ~/steel   # once: record where the folder is (optional)
temper install       # full converge: packages + all config + one-time setup
temper restore       # Linux desktop only: load the dconf snapshot back
```

`install` adds missing packages (brew/flatpak/mas/gext/rpm), applies every
config step, and runs one-time setup. `manual` steps (e.g. `speaker-eq`) are
skipped — run them by hand. On a fresh desktop, `restore` reloads GNOME/Ptyxis
state from the snapshot (it's a separate, confirm-gated verb because it clobbers
live tweaks). *(RIS: `bootstrap.sh` → `just install` → `just gnome-restore`.)*

`install`/`update` are **quiet by default** — the underlying tools are hushed
(`brew bundle`/`brew upgrade` `--quiet`, mas's Spotlight-reindex noise muted, and
already-installed App Store apps skipped) so you see installs, changes, warnings,
and errors, not a wall of "already OK". Quiet is not silent: the package phase
shows a **spinner naming the package being installed right now**
(`⠹ Installing llvm`, and `⠹ Installing Xcode 3/49` for the one-at-a-time App
Store apps), so a multi-GB download reads as progress instead of a hang. The
tool's own output is captured and **replayed in full if the converge fails**;
Homebrew's warnings (with their bodies, which carry the remedy) print even on
success. Your own **`exec` scripts are hushed the same way**: their output is
captured and stays hidden on success (so an idempotent script's "nothing to do"
chatter can't masquerade as temper's own verdict), and is surfaced only if the
script fails. Pass **`-v`/`--verbose`** (a global flag, like `--json`) to stream
every tool's — and every `exec`'s — full output instead of the spinner when
debugging. (An idempotent `exec` that re-runs each `update` should carry a `check`
hook so it's skipped, not just hushed, when already in sync.)

**One password per run.** Some casks install through a system installer
(`mactex`, `zoom`, `dotnet-sdk`, …) and need root. Homebrew asks per cask, and
because sudo's timestamp expires (5 min by default) during the multi-GB downloads
in between, a big converge used to prompt over and over, hours apart. temper now
checks up front whether this run will touch any such package, names them, and
asks **once** before anything downloads — then holds the timestamp open for the
rest of the run. Nothing to ask for means no prompt at all: a converged machine,
a spec with no pkg-based casks, and every read-only verb stay password-free, and
so does `--dry-run`. Set `TEMPER_NO_SUDO_KEEPALIVE=1` to opt out (you'll get
Homebrew's per-cask prompts back). App Store prompts are Apple's own and can't be
cached this way — `mas` may still ask per app.

**How temper finds the folder (why it "just knows where steel is").** temper
resolves its home in this order, first hit wins: `$TEMPER_DIR` → walk up from the
cwd → a saved pointer (`temper setup <dir>`) → **auto-scan** a folder named
`steel`/`temper-home`/`.temper` under `~`, a dev parent
(`~/Developer`, `~/developer`, `~/dev`, `~/src`, `~/code`, `~/projects`, `~/git`,
`~/repos`, …), or a cloud-sync root — the same set `dotsync` probes: each
`~/Library/CloudStorage/*` client, iCloud Drive, `~/Nextcloud`, `~/Dropbox`,
`~/OneDrive`, `~/ProtonDrive`, `~/Google Drive`, `~/Sync`, plus `/media` and
`/run/media/$USER`. So on a
fresh box you have three no-fuss options: clone/sync your folder to one of those
locations (e.g. `~/Developer/steel`) and it's found automatically; or run
`temper setup` — with no argument it lists the discovered libraries and lets you
pick one (or paste a path), saving the pointer; `temper setup <dir>` sets one
directly. Or export `TEMPER_DIR`. If several libraries are found and none is
pinned, temper refuses to guess and tells you to run `temper setup`. temper only
*locates* the folder — getting it onto the box (git clone, a synced cloud folder,
a USB copy) is not temper's job.

## 2. Stay current (routine maintenance)

**When:** periodically — the safe, boring upgrade.

```sh
temper update
```

Upgrades the declared package set (`brew upgrade` + `flatpak update`), re-applies
`always` config steps (so hand-drift is corrected), and backfills `ensure` steps
that are missing. It does **not** add newly-declared apps wholesale (that's an
`install`), and it never runs `restore` (reloading a snapshot would clobber live
tweaks). *(RIS: `just update`, which deliberately excludes gnome-restore.)*

The summary reports what the run **changed**, not what the machine declares —
`upgraded 6 packages` (measured as the drop in the outdated count across the
upgrade, so a failed upgrade doesn't claim credit) or `packages already current`.
The package managers' own output is captured: a converged machine is near-silent
except for a spinner, warnings and errors always print, and a tool's private
verdict ("Nothing to update.", about *its* remotes) is never shown as temper's.
`-v` streams everything the tools print.

> **Keep temper itself current across the fleet before using a new manifest
> field.** Because unknown fields are rejected (`deny_unknown_fields`), a machine
> on an older temper doesn't *skip* a field it doesn't know — it fails to parse
> the whole manifest, breaking that machine's entire converge, not just the step.
> So a new field (a new `setkey` backend, `template`, `append`, …) sets a version
> *floor*: bump temper everywhere first, and note the floor in the commit/recipe.
>
> temper helps you catch this: it stamps `temper_version` into `temper.toml` on
> every write, so a box running an older temper can tell a *version skew* (the
> folder came from a newer temper) from a genuine typo. On a skew it says so —
> and, on a Homebrew install (macOS or Linuxbrew), offers to run
> `brew upgrade temper` and re-run your command, instead of dumping a cryptic
> `unknown field` error. Tune this with
> `temper configure set update.mode <off|warn|prompt|auto>` (default `prompt`).
> `auto` upgrades unattended — **only temper**, never your other packages.
> (`temper upgrade` is an alias for `temper update`.)

## 3. The drift loop (the core habit)

**When:** anytime you want to know "is this machine what I said it should be?" —
read-only, so run it freely.

```sh
temper drift
```

The report groups findings by app, surfaces out-of-sync items in red, collapses
in-sync apps, and ends with **Next steps** — the exact command for each way out
of the drift. You pick a direction and run what it prints:

- **Config drifted** (a file/key/assert): `temper install` re-applies
  it, or `temper undo` reverts the last run.
- **Packages missing:** `temper install --packages-only` (add them, no
  config churn) — the additive "install-missing" flow.
- **Packages installed but not declared (extras):** decide the direction —
  - converge machine→spec: `temper prune` (uninstall them, asks first);
  - absorb spec←machine: `temper reconcile` (interactively add/drop),
    or `temper backup` (overwrite the machine Brewfile wholesale).
- **Tap-trust drifted** (`[brew].trust`): a declared tap that isn't trusted
  (brew silently skips its formulae) → `temper install`/`update` re-trusts it;
  a tap trusted on the machine but not declared → `temper reconcile` absorbs it
  into `[brew].trust` (or `[ignore].tap`), or `temper prune` `brew untrust`s it
  (the machine→spec mirror). `[ignore].tap` suppresses the extra either way.

This four-branch package fork is the heart of it (RIS emitted it at the moment of
detection; temper prints it under Next steps, and in `--json` as `remediation`).

## 4. Add something to the spec

**When:** you want a new app/package on a machine going forward.

1. Edit the folder: add the package to a bundle, the machine's loose `packages`,
   or its `brewfile`; add config steps to the app bundle. (The `[[step]]`
   primitives and their fields are in `SPEC.md` / `temper --llm` — e.g. `setkey`
   can resolve `{{ … }}` in a value at apply time with `template = true`, and its
   `json` backend edits JSONC comment-preservingly. `PATTERNS.md` shows how to
   *compose* the primitives for common problem shapes.)
2. Apply it:

```sh
temper install                 # add packages + apply the new config
# or, packages only, no config re-run:
temper install --packages-only
```

## 5. Absorb ad-hoc changes back into the spec

**When:** you installed/changed something directly on the machine and want the
spec to reflect it (spec←machine). **The default habit is `reconcile`** — it's
per-item and surgical, so nothing lands in the spec without you saying so.

```sh
temper reconcile   # the go-to: interactively add extras / drop missing entries /
                   #   route a flatpak extra to [ignore] / reconcile tap-trust
temper adopt       # optional first look: just list the extras, mutate nothing
temper backup      # rarely: wholesale dump of live state → Brewfile
```

`reconcile` prompts per item (missing entries default to keep, extras default to
skip; flatpak extras also offer "ignore") and edits only the machine's **own**
Brewfile, never a shared bundle — so it's safe to run often. Absorbed entries are
written back in canonical order (taps → brews → casks → mas, alphabetical within
each group) so the Brewfile stays sorted instead of growing an unsorted tail.

`reconcile` also reconciles **tap-trust** (`[brew].trust`, fleet-level in
temper.toml) the same way: a tap trusted on the machine but not declared can be
absorbed into `[brew].trust` (or routed to `[ignore].tap`), and a declared tap
that isn't currently trusted can be dropped (default keep — `install`/`update`
would re-trust it). `adopt` is the read-only preview; `backup` is the blunt "just
capture everything" fallback when you'd rather diff-then-trim than answer prompts.

## 6. Capture / restore desktop (GNOME + Ptyxis) state

**When:** you tuned the desktop and want it in the spec, or you're resetting a
machine to the snapshot — both on a fresh bring-up **and** mid-life when live
GNOME/Ptyxis state has drifted and you want it back to known-good.

```sh
temper backup      # also writes each [[machine.dconf]] snapshot (filtered)
temper restore     # load the snapshot(s) back into live dconf (confirm-gated)
```

`backup`'s dconf dump runs through the `strip` filter (bookkeeping + per-monitor
keys that would corrupt a round-trip). `restore` is confirm-gated and never part
of `update` (so a routine `update` never clobbers live tweaks) — it's a
deliberate, on-demand reset. *(RIS: `gnome-backup`/`gnome-restore`,
`ptyxis-backup`/`-restore`.)*

## 7. Undo a run

**When:** a run did something you didn't want.

```sh
temper undo --list           # revertible runs, newest first
temper undo                  # revert the most recent
temper undo <run-id>         # revert a specific one
temper undo --dry-run        # show what would revert, touch nothing
```

Reverts file writes (`copy`/`block`/`setkey` json/toml/ini) and `setkey(dconf)`
values. `setkey(defaults)`, `sysfile`, and `exec` aren't journaled — undo skips
them. Every revert is guarded: if the target changed since, it's skipped, not
clobbered.

## 8. Pull calibrated speaker profiles

**When:** the upstream speaker-EQ repo has new profiles (needs `[eq_import]`).

```sh
temper eq-import             # clone the repo, land <x>.calibrated.conf → <x>.conf
```

Then apply via the `speaker-eq` step (which is `manual` — run it explicitly).
This is folder-authoring: it writes *into* the folder, then you review + commit.
*(RIS: `just eq-import`.)*

---

## Save spec changes to git (so the folder doesn't drift)

`reconcile`, `backup`, and `eq-import` — and any hand edit — change the
temper-home *folder*, not a machine. If that folder is a git repo, temper helps
you persist those changes so it doesn't silently drift:

- Whenever the folder is left dirty, **any** command hints — the spec-writing
  verbs above *and* the read/apply ones (`drift`, `install`, `update`, `prune`,
  `adopt`, `restore`), so a stray hand edit surfaces whatever you run next:
  `ⓘ steel has uncommitted spec changes — temper save …`.
- **`temper save`** = `pull → add -A → commit → push`, with an
  auto-generated message (`reconcile chronos-redux: +2 -1 ~0`) unless you pass
  `-m "…"`. `--no-push` to hold. Works after hand edits too (message from the
  changed paths).
- **`temper refresh`** (alias `pull`) = `git pull` in the home — the pull-side
  counterpart to `save`. Run it from **anywhere**: temper resolves the folder, so
  you never have to find it or `cd` in just to grab a fleet change. Explicit, so
  it pulls even when `auto_pull` is off; `--rebase` (or `[git].auto_rebase`)
  rebases instead of fast-forward. A non-git home just says so.
- Prefer hands-off? Turn on the `[git]` toggles so temper auto-commits (and
  optionally pushes, and pulls before a run):
  **`temper configure set git.auto_commit true`** (then `git.auto_push`,
  `git.auto_pull`, and `git.auto_rebase` — the last makes `auto_pull` use
  `--rebase` instead of `--ff-only`, so a pull still lands when the box has
  un-pushed local commits). `temper configure list` shows the current values;
  `temper configure unset git.auto_commit` reverts a toggle to its default.
- On a **non-git** folder (Nextcloud / USB / plain dir) all of this is a silent
  no-op — syncing that folder is git/Nextcloud's job, not temper's (Principle #9).

`auto_pull` (and `save`'s pre-push pull) keep you from committing onto a stale
spec; if it can't pull (offline, diverged) it warns and continues — never blocks.
With `auto_rebase` a diverged local is replayed on top instead of warned past.

**Per-run override.** `auto_pull` is the persistent default; two global flags
override it for a single run on **any** verb: **`--pull`** forces a pull even
when `auto_pull` is off (handy after a known fleet change), and **`--no-pull`**
skips it even when on (handy offline). **`temper status`** shows the home's
state + settings in one view: path, git state, resolved machine, `[git]` toggles,
and the `[update]` self-update mode.

The whole git surface, then, is symmetric: **pull** = `auto_pull` (default) ·
`--pull`/`--no-pull` (per-run) · `temper refresh` (explicit); **push** =
`auto_commit`/`auto_push` (default) · `temper save` (explicit); **status** =
`temper status`.

## Fleet: author once, run per-machine

temper acts on the **machine it runs on**. The machine *argument* selects which
**spec** to read; execution is always local. So the fleet model is:

- **Author centrally.** Edit this one folder; it travels to each machine by
  git/Nextcloud/USB. On each box, `temper setup <dir>` records where it lives.
- **Run per-machine, no argument.** On a machine, `temper install` / `update` /
  `drift` with **no machine name** resolves *this* machine by hostname. Drive
  remotes over ssh — `ssh atlas 'cd ~/steel && temper drift'` — so atlas resolves
  and converges *itself*.
- **The machine argument selects a spec, not a remote target.** `temper drift
  atlas` on another box checks *this* box's live state against *atlas's* spec
  (useful while authoring). A live `temper install atlas` from a different host
  **asks you to confirm** — because it would apply atlas's spec to the box you're
  on, not to a remote atlas (`--yes` skips the prompt; `--json` refuses without
  it; a differing OS is still hard-refused). To converge atlas for real, ssh in
  and run `temper install` there — with no name it resolves *itself* by hostname,
  no prompt. Installing by an explicit name is for the deliberate case (e.g.
  imaging a box whose hostname isn't set yet).

## Presence-gating in practice (why config "just works" per machine)

Steps can carry `when = { <probe> }` (skip unless present) or `needs =
{ <probe> }` (error unless present). So composing a desktop bundle on a server,
or an app whose binary is image-baked vs brew-installed, behaves correctly under
one rule — `install`/`update` print `⚠ skipped: <probe> absent`, and `drift`
reports the gated-out step status-only. You rarely think about it; it's why a
machine's app list can be generous without config landing where it shouldn't.

## Two directions, one table

| You want… | Direction | Command |
|---|---|---|
| Machine to match the spec (packages) | machine→spec | `install --packages-only` / `prune` |
| Machine to match the spec (config) | machine→spec | `install` |
| Spec to match the machine (surgical) | spec←machine | `reconcile` |
| Spec to match the machine (wholesale) | spec←machine | `backup` |
| Desktop reset to the snapshot | spec→machine | `restore` |
| Undo the last change | — | `undo` |

## `--json` everywhere

Every verb takes `--json` (machine output on stdout, progress/errors on stderr),
so these loops script cleanly. `drift --json` includes a `remediation` array (the
Next-steps commands); `reconcile --json` previews the plan without prompting.
