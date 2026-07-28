# ROADMAP.md — temper's deferred features & scope

What we intend to build (parked, not broken), the **workflow/HMI parity gaps**
against the ReinstallScripts recipes temper generalizes, what's deliberately
**not** temper's job, and the migration-verification gap. Each item has why it's
parked, the current mitigation, and enough of a sketch to act on cold.

This is planning — it is **not** embedded in `--llm`. *Current behavior*,
including the limitations that are simply how temper works today
(non-journaled `exec`/`setkey(defaults)`/`setkey(dconf)`, `setkey(toml)` dropping
comments, `profile` being a manual install, `run = "ensure"` on a checkless
`exec` being skipped, os/role-only gating, `mas` failures being fatal), is
documented as **behavior** in `SPEC.md` / `ARCHITECTURE.md` / the README status
table — which *do* ride `--llm`. This file is only the "what's coming" ledger.

See `ARCHITECTURE.md` for the model and `SPEC.md` for the implemented schema.

---

## Deferred features (buildable — just not built yet)

### Presence-gating (`when` / `needs`)
**Status:** designed, not built. ARCHITECTURE describes it; the parser rejects
`when`/`needs` (deny_unknown_fields).
**Now:** steps gate on `os` + `role` only. The "run config only if the app is
present" intent is met today by (a) exec scripts' own `command -v` guards and
(b) drift's `executable_resolves` assert. A `copy` to `~/.config/ghostty/config`
on a box without Ghostty just leaves a dead file — harmless.
**Sketch:** add `when`/`needs` to `Step`; a probe enum
`binary|brew|cask|flatpak|mas|gext|rpm|path|exec`; evaluate before apply; skip
loudly (Principle #6). Default `when` = "my declared package is installed."

### Discovery auto-scan
**Status:** deferred. Only `$TEMPER_DIR` + a cwd walk-up.
**Sketch:** port dotsync's `discovery.rs` (scan common cloud-folder locations, a
saved pointer, first-run prompt).

### Forgiving `mas` converge
**Status:** deferred. Today `mas` rides the aggregate `brew bundle`, which bails
on any failure → a MAS failure (no App Store sign-in, an app not associated with
the Apple ID) fails the whole run. MAS is the flakiest provider, so it *should*
be reported-and-skipped.
**Sketch:** converge mas separately from `brew bundle` (own `mas install` loop)
so its failures are warned, not fatal.

### Declarative system-file primitive (the clean `/etc` path)
**Status:** idea. Root-owned config (the 1Password `/etc` allowlist) is done via
an `exec` script that self-escalates.
**Sketch:** a first-class primitive that writes one root-owned file with
mode/owner, escalating internally for just that write (Ansible's per-task
`become` on a `copy`). Lets `/etc` writes be declarative + drift-checkable
instead of buried in `exec`.

### Journaled system-side `setkey`, comment-preserving toml (maybe)
Low-value polish, only if the need shows up: snapshot the prior `defaults read` /
`dconf read` value so those `setkey` backends become undoable; and swap `toml`
for `toml_edit` so a hand-commented TOML keeps its comments on write. Both are
"current behavior" today (see the note at the top), not bugs.

---

## Workflow & HMI parity gaps (ReinstallScripts)

The features above are self-contained primitives. These are about the **loop** —
change → drift → decide direction → run the named command — that ReinstallScripts
emits *at the moment of detection*, and that temper does not yet reproduce.
Surfaced by comparing temper against the RIS `just` recipes and by LLM + human
review. This is the reviewer-flagged batch: the remediation hand-off, the missing
absorb-direction verb, and the drift display itself.

### Drift → remediation: "what to run next" (the headline gap)
**Status:** not built — the largest workflow shortfall.
RIS accumulates a `fixes=()` array *during* the drift scan — each entry a
`"description|command"` pair — and closes with a Summary that renders each through
`action_line` (a cyan `→` description with the exact, copy-pasteable command
dimmed beneath it). At the package-diff point it pushes a **four-branch fork** in
one shot (`Linux/justfile:541-544`), naming *both directions* out of the same
difference: `install-missing` and `prune` to converge machine→spec, `reconcile`
and `backup` to absorb spec←machine. The GNOME block forks the same way (show
diff / `gnome-restore` / `gnome-backup`).
**Now:** `cmd_drift` prints a flat `✓/✗` list and a single `N ok, M out of sync`
count. `Finding` (`plan.rs`) carries `app/kind/target/ok/status` — **no
remediation field** — and no suggested-command string exists anywhere in the
binary. The only next-step guidance is `cmd_adopt`'s prose telling you to
hand-edit TOML, and that covers the extras direction only. So of RIS's four
branches, exactly one (`install`/`update`) is reachable by *running* something;
the rest need a manual file edit.
**Sketch:** give `Finding` a `remediation: Vec<Action>` (`{ label, command }`);
render a Summary block after the item list (omit when all-ok). Package-drift
findings carry the both-direction fork; config-drift findings point at
`install` / `undo`. Depends on a mutating spec-side verb (`reconcile`, below)
existing for the absorb-direction actions to target — the drift item type must
carry the remediation forward *and* have a verb to point at.

### `reconcile` — interactive spec←machine capture (both directions, with drops)
**Status:** designed as `adopt`'s target shape; not built. (ARCHITECTURE named
"interactive spec←machine capture — dotsync's verb" as adopt's target but
formerly dismissed the *name* `reconcile` as machine←spec = install/update — the
wrong way round. RIS `reconcile` is **spec←machine**, the opposite of install;
that line is now corrected.)
**Now:** `adopt` covers the **extras half only**, reports without mutating, and
**never drops**. Absorbing a change into the spec is a hand TOML edit — except
packages, where `backup` does a *full overwrite* rather than a per-item choice.
RIS `reconcile` (Linux `justfile:723-852`) is per-item and surgical: missing
entries prompt `[Y/n]` (default keep; `n` drops the line from the Brewfile),
extras prompt `[y/N]` (default skip; `y` adds), and **flatpak** extras get a
three-way `[y/N/i]` where `i` appends the appid to `bazzite-flatpak-ignore.txt`
(silenced in future drift without being tracked). It ends with a unified
`diff -u` preview + a separate `+`-prefixed ignore-file preview + one final
`[y/N]` gate before any write. Mac (`justfile:334-435`) is the same minus
flatpak/ignore, scoped to `brew|cask|tap`.
**Sketch:** a `reconcile [machine]` verb: compute the same missing/extras diff
`drift` already computes, prompt per item (keep/drop for missing; add/skip/ignore
for extras, `ignore` flatpak-only), preview the resulting TOML/Brewfile edit,
confirm once, write. This is the absorb-direction verb the remediation summary
above points at.

### `install-missing` — additive, packages-only converge
**Status:** deferred. RIS `install-missing` (`Linux/justfile:905-917`) is a thin,
**additive** wrapper over `brew bundle install` (+ tap-trust): add
declared-but-absent packages, never remove, never run config or one-time setup.
**Now:** `install` takes only a machine + `--dry-run`, so the full converge
(packages + all config + one-time setup + dconf reload) is the *only* path to add
a single missing package. There is no "just the additions, just the packages"
step.
**Sketch:** an `install --packages-only` flag (or a dedicated verb) that runs only
the aggregate package converge in add-only mode — no prune, no config phase. It's
a subset of what `install` already does.

### Filtered dconf snapshot + targeted restore
**Status:** snapshot designed, not built; the restore pair is absent entirely.
`backup` does `brew bundle dump` only. RIS additionally has
`gnome-backup`/`ptyxis-backup` (a *filtered* `dconf dump` → `assets/…`, the
strip-keys `awk` that drops bookkeeping + per-monitor panel keys that would
corrupt a round-trip) and `gnome-restore`/`ptyxis-restore` (load shared then
per-machine snapshot into live dconf, `confirm`-gated).
**Now:** no dconf capture and no targeted restore — the only way to reload desktop
state is a full `install`, which re-runs everything. RIS's `update` deliberately
**excludes** restore *because* reloading a snapshot clobbers live tweaks — so a
targeted, opt-in restore verb (never in `update`) is exactly the point.
**Sketch:** extend `backup` to also write the filtered dconf snapshot (the
strip-keys filter is already an ARCHITECTURE manifest field, not tool-baked); add
a `manual`-lifecycle `restore [machine]` that loads shared-then-per-machine with a
confirm gate. Kept out of `update` by design.

### `eq-import` — pull calibrated profiles into the folder
**Status:** deferred, **to replicate** (it's a working RIS recipe, so it's in).
RIS `eq-import` (`Linux/justfile:1091-1110`) clones-or-`git pull --ff-only`s the
public `pipewire-speaker-profiles` repo and copies each `*.calibrated.conf` into
`assets/speaker-eq/<base>.conf`, ready for the `speaker-eq` step to apply.
**Now:** no equivalent — the profiles must be fetched and copied by hand.
**Wrinkle (its shape, not whether):** unlike every other verb it writes *into* the
config folder (authoring) rather than converging a machine, brushing Principle #9.
That decides whether it's a clearly-labelled authoring verb or a companion helper
— it does **not** make it out of scope (that framing was wrong and is corrected in
ARCHITECTURE/PRINCIPLES).
**Sketch:** an `import` (or `eq-import`) verb/helper that `--ff-only`-fetches the
upstream repo, lands the `*.calibrated.conf` files under `assets/speaker-eq/`,
reports each file, and points at the `speaker-eq` step to apply them.

---

## Delivered outside the binary (not scope-refused — RIS parity still holds)

Every RIS recipe gets a temper equivalent. Exactly two RIS jobs are delivered by
something other than a temper *verb*, for a real constraint — and RIS delivers
them outside its `just` recipes the same way, so this is parity, not a dropped
feature:

- **Bootstrap** — getting brew + temper onto a bare machine runs before the tap
  (and temper) exists — the paradox. Stays a small companion getting-started
  script (like grove/amdl/dotsync's `install.sh` fallback; RIS uses
  `bootstrap.sh`). Deferred.
- **Building the host image** — rebase, cosign, baked system layer. This is a
  different *artifact* and is **being spun out to its own repo** (Stacks); it was
  never temper's job. temper *configures* a machine on top of the image, and drift
  reports image-baked items status-only. (RIS draws the same line with
  `install-bazzite.sh`.)

---

## Verification gap (a state, not a feature)

The Linux half of the `steel` migration is transcribed + parse-valid but has
**never run** — the dconf loads, 1Password NMH surgery, PWAs, speaker-eq exec
scripts await a VM. See the README "VM run checklist". Mac config is
drift-verified against a real machine. ReinstallScripts stays as the fallback
until the VM run confirms Linux.
