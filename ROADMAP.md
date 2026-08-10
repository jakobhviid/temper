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

**Which flatpak installation temper owns.** flatpak has two installations, and
temper currently uses different ones in different directions: `install` runs
`flatpak install` with no scope flag, whose default is the **system**
installation, while `prune` and `undo` pass `--user`. On a host whose apps are
all system-scope — any image-based one; the box this was found on has 83 apps
and **zero** in the user installation — that means prune removes nothing temper
installed and undo reverts nothing, silently.

Neither direction is obviously the right one to change:

- Passing `--user` to `install` does **not** work as-is: a user-scope operation
  cannot see a system remote (`flatpak remote-info --user flathub …` → *"Remote
  not found in the user installation"*), so an image's system `flathub` would
  stop resolving. It would need temper to add a user copy of every declared
  remote, duplicating what the machine already has and re-downloading apps that
  are already present system-wide.
- Letting `prune`/`undo` act on the system installation makes them able to
  remove image-baked apps, needs polkit, and can hang over ssh. `[ignore]` and
  the confirm are the existing guards, and they may well be enough — but that is
  a fleet-behaviour decision, not a cleanup.

Until it is decided, the honest state is recorded rather than papered over:
`interface.rs` scores flatpak `prune` and `revertible` as **No** with the reason,
and `prune` already reports the system-scope apps it declined to touch. The
remote provider is settled by contrast — remotes are **observed in both**
installations (so a declared remote the image provides is not permanently
missing, and no duplicate user copy is added) and **written to the user** one,
which is the only one `remote-delete` may act on.

*(Otherwise the matrix is clean apart from the ignore column on deployed files,
which is deliberate: an edited file is reported rather than removed, which covers
the case that matters.)*

**The provider trait is half built.** `interface.rs` records each provider's
eleven answers as data and cross-checks them against the finding registry, so a
claimed capability with nothing behind it now fails a test. What remains is
dispatch: the providers still have bespoke function signatures, so `install`,
`prune` and the reconcile pair are wired per provider rather than driven from the
table. Harmonising them is what makes adding `apt` or `npm` routine.

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
per-section reconcile, a journaled `restore-dconf`, `reconcile --current-state-wins`,
and `temper init`.)*

**Deliberately not journaled** (a decision, not a gap): `setkey(defaults)` —
`defaults read` loses the value's type, so an undo couldn't rewrite it faithfully
— and `sysfile`/`exec`, which mutate root-owned/arbitrary state. dconf *is*
journaled (values round-trip cleanly) — per key for `setkey(dconf)`, and per
subtree for `restore-dconf`.

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
filtered `snapshot-dconf` + confirm-gated `restore-dconf` pair is built (with dconf
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

## macOS claims that need a Mac to settle

Everything on this list was raised by review, is **not** settled, and cannot be
settled from Linux. None is a guess dressed as a finding — each names the check
that would decide it. Run these on a Mac before trusting the corresponding cell
in the feature matrix.

- **`mas uninstall` arity.** `interface.rs` records `mas uninstall (<id>…|--all)`,
  and `undo` passes the whole set in one invocation. mas 1.8's usage line reads
  `mas uninstall [--dry-run] <app-id>` — singular. If that holds, a multi-app
  revert fails argument parsing and, because `undo`'s package path has no
  per-item fallback, reports the *whole* set as un-uninstallable.
  → `mas uninstall --help`.
- **`brew bundle cleanup --mas` / `--vscode`.** Homebrew documents those type
  flags for `install`/`list`/`dump`; `prune` passes them to `cleanup`, relying on
  "naming one type turns the others off". If `cleanup` does not accept `--mas`,
  a mas-only spec cleans nothing while reporting success.
  → `brew bundle cleanup --help`, then a dry run against a scratch Brewfile.
- **`sudo mas uninstall` and `secure_path`.** The revert shells out through
  `sudo`, so it depends on `mas` being on **root's** PATH.
- **`MAS_NO_AUTO_INDEX`.** temper sets it to mute the Spotlight reindex and
  ARCHITECTURE states it as fact; it is not in mas's documented environment.
- **`profile_apply` counts a cancelled dialog as a change.** It writes the
  content stamp and returns `Changed` as soon as `open` returns — which is when
  the window appears, not when the user approves. Declining a profile therefore
  reports an applied change and an unrevertible one.
- **`sudo temper …` splits the state root.** The journal, ledger and profile
  stamps land under `/var/root/Library/Application Support/temper`, so a later
  unprivileged `undo` sees nothing to undo. Nothing detects the split. (Not
  mac-specific in principle, but that is where the path differs most.)

Fixed blind, and portably, rather than left for the hardware: `sudo install -D`
(GNU-only, so every `sysfile` step failed on macOS — and the error propagated
before `journal.commit()`, discarding the run's undo record), `getent` in
`gid_of` (absent on macOS; now falls back to `dscl`), and the `defaults` numeric
comparison (`48` vs `48.0` drifted forever).

## Verification gap (a state, not a feature)

The Linux half of the `steel` migration (`steel` = the author's own fleet spec,
the folder temper was built for) is transcribed + parse-valid but has
**never run** — the dconf loads, 1Password NMH surgery, PWAs, speaker-eq exec
scripts await a VM. See the README "VM run checklist". Mac config is
drift-verified against a real machine. ReinstallScripts stays as the fallback
until the VM run confirms Linux.
