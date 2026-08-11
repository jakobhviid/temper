# ROADMAP.md — temper's deferred features & scope

What we intend to build (parked, not broken), the **workflow/HMI parity gaps**
against the ReinstallScripts recipes temper generalizes, what's deliberately
**not** temper's job, one **restructuring** weighed and declined, and the
migration-verification gap. Each item has why it's
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
`interface.rs` scores flatpak `revertible` as **No** with the reason. `prune`
stays **Yes** — it removes the user-scope apps and *reports* the system-scope
ones it declined, which is a real path with an honest answer. The
remote provider is settled by contrast — remotes are **observed in both**
installations (so a declared remote the image provides is not permanently
missing, and no duplicate user copy is added) and **written to the user** one,
which is the only one `remote-delete` may act on.

**The rest of the matrix's open cells**, so the summary and the table agree —
this file rides `--llm` precisely so an agent can tell a working cell from a
broken one, and it previously said "none open" over a table with fifteen marks
in it.

- **`revertible` on `brew-trust` and `flatpak-remote`** — neither `brew trust`
  nor `flatpak remote-add` is journaled. The pattern applies (the missing set is
  known before the converge, uninstall is install backwards); it is unwired.
  `install --packages-only` names them at plan time rather than pretending.
- **`revertible` on `flatpak`** — the installation question above.
- **`dconf`**: `install` is ⚠ because `restore` is excluded from
  `install`/`update` (reloading a snapshot would clobber live tweaks — a
  property of the *recording model*, not of the store); `prune` is ❌ because a
  key has no extras direction that is safe to enact wholesale; `ignore` is ⚠ —
  `strip` silences noise but there is no per-key ignore; `residue` is ❌ because
  a retired subtree leaves its keys behind and nothing enumerates them.
- **`ignore` on `deployed-files`** — deliberate: an edited file is *reported*
  rather than removed, which covers the case that matters.
- **`profile`** is the weakest row and is honestly scored: no machine scope, a
  GUI-gated apply, no prune, no reconcile, no ignore, not revertible, no
  residue story. It now carries a `ProviderSpec` saying so in seven written
  reasons, so those answers are held to the registry like everyone else's rather
  than being prose nothing checks. Its `observe` column is genuinely ✅ —
  `system_profiler` reads the installed set across both scopes without MDM or
  root — which is why drift on a profile is real even though nothing else about
  it is.

**The provider trait is half built.** `interface.rs` records each provider's
eleven answers as data and cross-checks them against the finding registry, so a
claimed capability with nothing behind it fails a test. What remains is
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
`gid_of` (absent on macOS — it falls back to `dscl`), and the `defaults` numeric
comparison (`48` vs `48.0` drifted forever).

## Possible restructuring — provider dispatch behind a trait

**Parked deliberately, with a recommendation against the full version.** Recorded
here because the question recurs the moment anyone reads `providers.rs`, and the
answer took measuring to reach.

The contract is already explicit as **data**: `interface::PROVIDERS` gives every
provider eleven columns, each `Yes` / `No(reason)` / `NA(reason)`, cross-checked
by tests against `plan::KIND_ANSWERS` so a provider cannot claim `prune` unless
its kinds name `temper prune`. The restructuring would move that from data into
types — `trait Provider { fn observe(&self) -> Option<Vec<Item>>; fn extras(…);
fn converge(…); fn prune(…); … }` — and have callers iterate providers instead of
naming them.

**What it would buy.** A new provider becomes a compile error until every method
exists, which is the strongest form of "walk the matrix before you call it done".
And one answer shape, enforced rather than conventional — today they diverge:

```rust
brew_extras(&[Pkg], &Ignore) -> Result<Vec<(Manager, String)>>
gext_extras(&[String], &Ignore) -> Vec<String>   // "couldn't ask" becomes an empty Vec
rpm_ostree_extras(&[String], &Ignore) -> Vec<String>
```

**Why the full version is the wrong trade.** The providers do not differ because
the code is untidy. They differ because the *domain* does: brew manages three
namespaces at once, rpm-ostree converges into a deployment that needs a reboot,
flatpak has two installations whose asymmetry cannot be resolved from one side,
mas is one platform with an auth state that is an ordinary condition rather than
a failure. A trait asserts these are one kind of thing with different
implementations, and the evidence says they are partly different kinds of thing.

The concrete cost is not aesthetic. `Col::NA("reason")` makes each difference
state *why*, and a test fails on an empty reason; a trait method returning
`Ok(())` for the inapplicable case deletes that sentence and leaves the asymmetry
silent. Per-tool facts get pressed flat the same way — `brew bundle cleanup`
exits **non-zero when it finds orphans**, and reading that as failure once zeroed
brew extras on every machine.

The gain is also smaller than a grep suggests. Of the ~48 per-manager match arms,
**23 are in `packages.rs`** and are total mappings (`as_str`, `journal_provider`,
`ignore_list`) — already exhaustive, already a compile error on a new variant,
and better left as matches. The real dispatch fan-out is closer to twenty sites.

**The narrow version, which is recommended.** Unify where the shape is genuinely
shared, not where we wish it were. Exactly one question is common to every
provider — *"what is present here, and could I even ask?"* — and it is where the
dangerous failure lives, because "couldn't ask" collapsing into "none" is what
lets a write path read empty as *delete everything*.

1. **One `Option<…>` shape for observation and extras**, enforced rather than
   conventional.
2. **Make `Col::Yes` provable.** It is currently checked against another table.
   Call each provider's observation function for every provider claiming
   `observe: Yes`, so a cell cannot claim a capability no code path delivers —
   the shape of defect that let a removed GNOME extension come back on every
   converge for two releases.

Everything past observation — converge, prune, reconcile — stays as functions,
because that is where the providers legitimately stop resembling each other.

**The deciding evidence.** None of the defects found in the scope-model cycle
would have been caught by a trait: an extension's dconf subtree derived from its
uuid, a capture writing a bundle-scope file from one machine, an escape hatch
skipping the rule its probe enforced, a verb reporting on rows it had not
checked. Every one came from a specific provider's specific behaviour — the
details a uniform signature is designed to hide.

---

## Verification gap (a state, not a feature)

The Linux half of the `steel` migration (`steel` = the author's own fleet spec,
the folder temper was built for) is transcribed + parse-valid but has
**never run** — the dconf loads, 1Password NMH surgery, PWAs, speaker-eq exec
scripts await a VM. See the README "VM run checklist". Mac config is
drift-verified against a real machine. ReinstallScripts stays as the fallback
until the VM run confirms Linux.
