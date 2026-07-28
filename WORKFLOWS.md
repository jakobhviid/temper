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
temper use ~/steel   # once: record where the folder is (optional)
temper install       # full converge: packages + all config + one-time setup
temper restore       # Linux desktop only: load the dconf snapshot back
```

`install` adds missing packages (brew/flatpak/mas/gext/rpm), applies every
config step, and runs one-time setup. `manual` steps (e.g. `speaker-eq`) are
skipped — run them by hand. On a fresh desktop, `restore` reloads GNOME/Ptyxis
state from the snapshot (it's a separate, confirm-gated verb because it clobbers
live tweaks). *(RIS: `bootstrap.sh` → `just install` → `just gnome-restore`.)*

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

This four-branch package fork is the heart of it (RIS emitted it at the moment of
detection; temper prints it under Next steps, and in `--json` as `remediation`).

## 4. Add something to the spec

**When:** you want a new app/package on a machine going forward.

1. Edit the folder: add the package to a bundle, the machine's loose `packages`,
   or its `brewfile`; add config steps to the app bundle.
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
                   #   route a flatpak extra to [ignore]
temper adopt       # optional first look: just list the extras, mutate nothing
temper backup      # rarely: wholesale dump of live state → Brewfile
```

`reconcile` prompts per item (missing entries default to keep, extras default to
skip; flatpak extras also offer "ignore") and edits only the machine's **own**
Brewfile, never a shared bundle — so it's safe to run often. `adopt` is the
read-only preview; `backup` is the blunt "just capture everything" fallback when
you'd rather diff-then-trim than answer prompts.

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

## Fleet: author once, run per-machine

temper acts on the **machine it runs on**. The machine *argument* selects which
**spec** to read; execution is always local. So the fleet model is:

- **Author centrally.** Edit this one folder; it travels to each machine by
  git/Nextcloud/USB. On each box, `temper use <dir>` records where it lives.
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
