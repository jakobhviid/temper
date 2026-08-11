# ROADMAP.md — temper's deferred features & scope

**Bugs** — behaviour that is wrong rather than unbuilt — then the **scope-model
gaps** (each a filled ⚠ or ❌ in the feature matrix), what's deliberately **not**
temper's job, one **restructuring** weighed and declined, the macOS claims only a
Mac can settle, and the migration-verification gap. Each item has why it's
parked, the current mitigation, and enough of a sketch to act on cold.

**Nothing shipped stays in this file.** A roadmap that inventories finished work
reads as work outstanding, and every reader has to diff it against reality to
find the live items. Git keeps the record, with dates and diffs. When something
lands, delete its entry rather than annotating it.

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

## Bugs

**A declared flatpak remote is added where the converge cannot use it.** temper
adds remotes with `remote-add --user` (`providers::remotes_converge`) while
installing apps with `install --system`, and a system-scope install cannot
resolve a user-scope remote. So an app declared from a vendor remote temper itself
added has nowhere to pull from, while the declaration reads as satisfied —
`remotes_missing` observes **both** installations, so the remote is found and the
install still fails.

Fixing it is not the one-line mirror of the app-scope fix, which is why it is
here rather than done:

- **Adding is easy; removing is the decision.** `remotes_extras` is correctly
  gated on the spec declaring at least one remote, and reads only the user
  installation because `remotes_delete` passes `--user --force`. Point both at the
  system installation and, on a spec declaring one vendor remote, `flathub`
  becomes an undeclared extra — so `prune` would offer to force-delete the remote
  every installed app updates from. `--force` is there precisely because
  `remote-delete` otherwise refuses a remote with apps installed from it.
- Three honest options, and it is a fleet-behaviour call: keep removal user-scope
  and add system-scope remotes only; drop `--force` so flatpak's own refusal is
  the guard; or gate system-scope removal behind an explicit opt-in, the way
  `--include-trust` gates fleet-scope taps.
- Whichever wins, **an app and the remote it comes from must name the same
  installation** — that is the invariant the app-scope pairing test now holds for
  install-vs-uninstall, and remotes are outside it.

**Root-owned removal is outside the one-password-per-run guarantee.** A
system-scope `flatpak uninstall` authorizes through **polkit**, and
`sudo::acquire` pre-authenticates **sudo**. flatpak's shipped rule
(`/usr/share/polkit-1/rules.d/org.freedesktop.Flatpak.rules`) returns `YES` with
no password for an active local session in `wheel` — which is why a storefront
never prompts — but over ssh or from cron `subject.local` is false, it falls
through to the action's implicit `auth_admin`, and there is nowhere to answer.
`prune` should say so up front, naming what needs it, exactly as a run needing
sudo already does; today it finds out when the removal fails.

## Scope-model gaps (known, ranked, each one a filled ⚠ or ❌ in the feature matrix)

**The matrix's open cells**, so the summary and the table agree — this file rides
`--llm` precisely so an agent can tell a working cell from a broken one.

- **`revertible` on `brew-trust` and `flatpak-remote`** — neither `brew trust`
  nor `flatpak remote-add` is journaled. The pattern applies (the missing set is
  known before the converge, uninstall is install backwards); it is unwired.
  `install --packages-only` names them at plan time rather than pretending.
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

**Deliberately not journaled** (a decision, not a gap): `setkey(defaults)` —
`defaults read` loses the value's type, so an undo couldn't rewrite it faithfully
— and `sysfile`/`exec`, which mutate root-owned/arbitrary state. dconf *is*
journaled (values round-trip cleanly) — per key for `setkey(dconf)`, and per
subtree for `restore-dconf`.

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
flatpak has two installations every direction must name the same one of,
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

`restore-dconf` and its revert are covered by `restore_dconf_is_revertible.rs`,
against a fake `dconf` that keeps a store on disk and whose `load` **appends** —
because the real one merges, and a stub that overwrote would pass whether or not
temper resets first. The tests assert the round trip (a key the snapshot
introduced is gone after `undo`, a key that predated it is not), the ordering
(`reset` precedes `load`), and that `--dry-run` touches nothing. Removing the
reset from `journal::dconf_load_tree` fails two of the three.

What that does **not** prove is the fake's fidelity: that `dconf reset -f` really
empties a subtree, and that `dconf load` merges exactly the way the stub models.
Those are assumptions about a tool, not about temper, and only a real desktop
settles them:

1. `temper restore-dconf`, then `temper drift` — the desktop should match the
   snapshot, and drift should be clean.
2. **`temper undo`** — the desktop must return to its pre-restore state,
   *including* keys the restore introduced that the prior dump never had.

Cheap to do on a machine that already has a snapshot, and worth doing once.
