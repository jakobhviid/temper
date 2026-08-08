# ROADMAP.md — temper's deferred features & scope

What we intend to build (parked, not broken), the **workflow/HMI parity gaps**
against the ReinstallScripts recipes temper generalizes, what's deliberately
**not** temper's job, and the migration-verification gap. Each item has why it's
parked, the current mitigation, and enough of a sketch to act on cold.

This is planning — it is **not** embedded in `--llm`. *Current behavior*,
including the limitations that are simply how temper works today
(non-journaled `exec`/`setkey(defaults)`/`sysfile`, `profile` being a
manual install, `run = "ensure"` on a checkless `exec` being skipped), is
documented as **behavior** in `SPEC.md` / `ARCHITECTURE.md` / the README status
table — which *do* ride `--llm`. This file is only the "what's coming" ledger.

See `ARCHITECTURE.md` for the model and `SPEC.md` for the implemented schema.

---

## Deferred features (buildable — just not built yet)

*(none open — the deferred batch has shipped: per-machine vars + `{{ brew_prefix }}`,
bundle os/role gating, presence-gating `when`/`needs`, forgiving `mas`, the
`sysfile` primitive, comment-preserving `setkey(toml)`, journaled+undoable
`setkey(dconf)`, discovery auto-scan + `temper setup`, key-level dconf drift +
per-section reconcile, a journaled `restore`, `reconcile --current-state-wins`,
and `temper init`.)*

**Deliberately not journaled** (a decision, not a gap): `setkey(defaults)` —
`defaults read` loses the value's type, so an undo couldn't rewrite it faithfully
— and `sysfile`/`exec`, which mutate root-owned/arbitrary state. dconf *is*
journaled (values round-trip cleanly) — per key for `setkey(dconf)`, and per
subtree for `restore`.

---

## Workflow & HMI parity gaps (ReinstallScripts)

The features above are self-contained primitives. These were about the **loop** —
change → drift → decide direction → run the named command — that ReinstallScripts
emits *at the moment of detection*. Surfaced by comparing temper against the RIS
`just` recipes and by LLM + human review.

**All shipped.** The reviewer-flagged batch is built — `drift` renders grouped,
coloured output with a **"Next steps"** summary that names both directions out of
the drift with exact commands (`plan::remediations`; the RIS four-branch package
fork + a config `install`/`undo` line, also in `--json`); `reconcile` is the
interactive spec←machine verb; `install --packages-only` is install-missing; the
filtered dconf `snapshot` + confirm-gated `restore` pair is built (with dconf
drift and per-section reconcile on top, and a journaled restore); and
`eq-import` pulls calibrated speaker profiles into the folder (folder-authoring,
the labelled Principle-#9 exception).

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

The Linux half of the `steel` migration (`steel` = the author's own fleet spec,
the folder temper was built for) is transcribed + parse-valid but has
**never run** — the dconf loads, 1Password NMH surgery, PWAs, speaker-eq exec
scripts await a VM. See the README "VM run checklist". Mac config is
drift-verified against a real machine. ReinstallScripts stays as the fallback
until the VM run confirms Linux.
