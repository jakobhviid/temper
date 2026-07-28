# ROADMAP.md — temper's deferred features & scope

What we intend to build (parked, not broken), the **workflow/HMI parity gaps**
against the ReinstallScripts recipes temper generalizes, what's deliberately
**not** temper's job, and the migration-verification gap. Each item has why it's
parked, the current mitigation, and enough of a sketch to act on cold.

This is planning — it is **not** embedded in `--llm`. *Current behavior*,
including the limitations that are simply how temper works today
(non-journaled `exec`/`setkey(defaults)`/`setkey(dconf)`, `setkey(toml)` dropping
comments, `profile` being a manual install, `run = "ensure"` on a checkless
`exec` being skipped), is
documented as **behavior** in `SPEC.md` / `ARCHITECTURE.md` / the README status
table — which *do* ride `--llm`. This file is only the "what's coming" ledger.

See `ARCHITECTURE.md` for the model and `SPEC.md` for the implemented schema.

---

## Deferred features (buildable — just not built yet)

### Discovery auto-scan
**Status:** deferred. Only `$TEMPER_DIR` + a cwd walk-up.
**Sketch:** port dotsync's `discovery.rs` (scan common cloud-folder locations, a
saved pointer, first-run prompt).

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

The features above are self-contained primitives. These were about the **loop** —
change → drift → decide direction → run the named command — that ReinstallScripts
emits *at the moment of detection*. Surfaced by comparing temper against the RIS
`just` recipes and by LLM + human review.

**Shipped:** the reviewer-flagged batch is now built — `drift` renders grouped,
coloured output with a **"Next steps"** summary that names both directions out of
the drift with exact commands (`plan::remediations`; the RIS four-branch package
fork + a config `install`/`undo` line, also in `--json`); `reconcile` is the
interactive spec←machine verb; `install --packages-only` is install-missing.
What remains below is the dconf snapshot/restore pair and `eq-import`.

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
