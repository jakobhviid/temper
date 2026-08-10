# ROADMAP.md — temper's deferred features & scope

What we intend to build (parked, not broken), the **workflow/HMI parity gaps**
against the ReinstallScripts recipes temper generalizes, what's deliberately
**not** temper's job, and the migration-verification gap. Each item has why it's
parked, the current mitigation, and enough of a sketch to act on cold.

This file **is** embedded in `--llm`, because what is *not* built is as
load-bearing for an agent authoring a folder as what is: without it, a feature
scoring ⚠ in the ARCHITECTURE matrix reads as one that works. *Current behavior*,
including the limitations that are simply how temper works today
(non-journaled `exec`/`setkey(defaults)`/`sysfile`, `profile` being a
manual install, `run = "ensure"` on a checkless `exec` being skipped), is
documented as **behavior** in `SPEC.md` / `ARCHITECTURE.md` / the README status
table — which *do* ride `--llm`. This file is only the "what's coming" ledger.

See `ARCHITECTURE.md` for the model and `SPEC.md` for the implemented schema.

---

## Scope-model gaps (known, ranked, each one a filled ⚠ or ❌ in the feature matrix)

The feature matrix in `ARCHITECTURE.md` shows where each feature stands against
the eleven-column interface. These are the open cells, worst first. Every one is
a *verified* gap, not a suspicion.

1. **Fleet `[brew].trust` and `[ignore]` have no `os`/`role` gate.** The
   machine-scope counterparts now exist (`[[machine]].brew_trust`,
   `[machine.ignore]`), so a per-machine declaration has a home — but a *group*
   declaration still cannot say which machines it describes, so one that is only
   meaningful on some is permanently red on the rest. The natural fix is to let a
   **bundle** carry them: a bundle is already the group construct and is already
   os/role-gated, which beats bolting a third gate axis onto the fleet tables.
2. **Package installs are not journaled, and nothing says so before you confirm.**
   `undo` covers file and key writes; a `brew`/`flatpak`/`mas`/`gext`/`rpm-ostree`
   converge is not revertible, and on a Mac `setkey(defaults)` is not either. A
   run whose only changes were unjournaled reverts nothing while reporting
   success. AGENTS.md question 7 asks for this at plan time; no code answers it
   yet.
3. **`flatpak` remotes are unmanaged.** There is no `flatpak remote-add` and no
   remote enumeration, so a declared app from a vendor remote or `flathub-beta`
   cannot be installed and the converge degrades to a warning. Remotes are the
   flatpak analogue of `[brew].trust` — the same fleet-vs-machine scope question
   that earned trust a machine-scope list.
4. **`[ignore]` is writable for two of its seven lists.** drift honours all seven;
   only `flatpak` and `tap` can be written by a verb, while the drift status for a
   GNOME extension extra tells the user to use `[ignore].gext`.
5. **No deployment ledger, so the file primitives score zero on residue.** Remove
   a `copy` step and its file stays on every machine forever, with no extras
   direction to report it. See "Retirement" in `ARCHITECTURE.md` for the shape.

**Sequencing note.** Build the settings-backend seam only after a *second real
consumer* exists. Flatpak overrides (`~/.local/share/flatpak/overrides/<app>`) is
the best candidate — sectioned key=value, one file per app, no cascade, no flag
syntax, no reload problem — and is valuable to a Linux fleet on its own. KDE is
the worst candidate to shape a seam around, because every hard case lives there.

## Deferred features (buildable — just not built yet)

*(none open — the deferred batch has shipped: per-machine vars + `{{ brew_prefix }}`,
bundle os/role gating, presence-gating `when`/`needs`, forgiving `mas`, the
`sysfile` primitive, comment-preserving `setkey(toml)`, journaled+undoable
`setkey(dconf)`, discovery auto-scan + `temper setup`, key-level dconf drift +
per-section reconcile, a journaled `restore-gnome`, `reconcile --current-state-wins`,
and `temper init`.)*

**Deliberately not journaled** (a decision, not a gap): `setkey(defaults)` —
`defaults read` loses the value's type, so an undo couldn't rewrite it faithfully
— and `sysfile`/`exec`, which mutate root-owned/arbitrary state. dconf *is*
journaled (values round-trip cleanly) — per key for `setkey(dconf)`, and per
subtree for `restore-gnome`.

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
filtered `snapshot-gnome` + confirm-gated `restore-gnome` pair is built (with dconf
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
