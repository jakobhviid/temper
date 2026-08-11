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

**temper writes flatpaks to one installation and removes them from the other.**
The converge runs `flatpak install -y --noninteractive` with **no scope flag**,
whose default is the **system** installation, while `prune`
(`providers.rs::prune_extras`) and `undo` (`journal.rs`) pass `--user`. Where the
two disagree — the ordinary case, since flatpak's own default and every desktop
storefront's (GNOME Software, KDE Discover, Bazaar) is the system installation —
`prune` removes nothing temper installed and `undo` reverts nothing. Both report
success, each truthfully describing an installation the apps are not in.

**The bar is the storefront the desktop already ships: if a user can delete an app
by clicking in it, `temper prune` has to be able to remove it.** temper owns the
installation its converge writes to.

Privilege is not the obstacle it appears to be. flatpak ships
`/usr/share/polkit-1/rules.d/org.freedesktop.Flatpak.rules`, which returns `YES` —
no password at all — for `app-install` / `app-uninstall` / `modify-repo` when
`subject.active && subject.local && subject.isInGroup("wheel")`. That is why a
storefront installs and removes system-wide without ever prompting.

The fix, and what must ride with it:

- Drop `--user` from the prune and undo uninstalls so both act on the scope the
  converge wrote to. The flatpak row's `revertible` column becomes provable.
- Keep the three-valued read; retarget the decline report. The guard that earns
  its place is declared-vs-undeclared — the one prune already applies to every
  other provider, through the preview that names each item and the confirm that
  defaults to no.
- **`flatpak_user_apps`' rationale does not survive the move.** It reasons by
  analogy with `gext`, where system scope means *the image owns it*
  (`/usr/share/gnome-shell/extensions` — removing one means rebuilding the image).
  A system-scope flatpak is nothing of the kind: it is where the storefront puts an
  app the user chose. An image's preinstalled set is an OS baseline like any
  other, which is what `[ignore].flatpak` is for.
- **Warn on an unattended run**, exactly as a run needing sudo already does. The
  polkit rule requires an active local session, so over ssh or from cron it falls
  through to the action's implicit `auth_admin` with nowhere to answer — and
  `sudo::acquire` pre-authenticates sudo, not polkit, so this sits outside the
  one-password-per-run guarantee rather than inside it.
- Two tests pin the current scope and move with it:
  `prune_actually_removes.rs` asserts the uninstall carries `--user`, and
  `prune_promises_what_it_does.rs` fakes flatpak per scope.

**The same defect runs the other way for remotes, and is also live.** temper adds
declared remotes with `remote-add --user` while installing apps into the system
installation, and a system install cannot resolve a user remote — so an app
declared from a vendor remote temper itself added has nowhere to pull from, while
the declaration reads as satisfied, because remotes are observed in **both**
installations. Whichever installation temper settles on, apps and remotes have to
name the same one.

## Scope-model gaps (known, ranked, each one a filled ⚠ or ❌ in the feature matrix)

**The matrix's open cells**, so the summary and the table agree — this file rides
`--llm` precisely so an agent can tell a working cell from a broken one.

- **`revertible` on `brew-trust` and `flatpak-remote`** — neither `brew trust`
  nor `flatpak remote-add` is journaled. The pattern applies (the missing set is
  known before the converge, uninstall is install backwards); it is unwired.
  `install --packages-only` names them at plan time rather than pretending.
- **`revertible` on `flatpak`** — the installation bug above.
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

The Linux half of the `steel` migration (`steel` = the author's own fleet spec,
the folder temper was built for) is transcribed + parse-valid but has
**never run** — the dconf loads, 1Password NMH surgery, PWAs and speaker-eq exec
scripts await a VM. Mac config is drift-verified against a real machine.
ReinstallScripts stays as the fallback until the VM run confirms Linux.

### VM run checklist

This is the compensating control for what the suite cannot reach, so it is worth
saying exactly what "cannot reach" means. `restore-dconf`'s **write** path is
covered only under `--dry-run`, which returns before `dconf load` — so the undo
payload capture and `journal::dconf_load_tree` (whose whole point is that
reverting a subtree needs a **reset then load**, because `dconf load` merges and
replaying the prior dump alone would leave every newly-introduced key behind)
have no automated coverage at all.

On a throwaway VM, in this order:

1. `temper install` from a clean image. Every `copy`, `block`, `sysfile` and
   `exec` lands; note anything that needed a second run.
2. `temper drift` — expect zero out of sync. Anything left is a step that
   claimed success without converging.
3. `temper restore-dconf`, then `temper drift` again. The desktop should match
   the snapshot, and drift should be clean.
4. **`temper undo`** — the step that is otherwise untested. The desktop must
   return to its pre-restore state, *including* keys the restore introduced that
   the prior dump never had. Those are the ones a bare `load` leaves behind.
5. `temper prune` on a machine with something undeclared installed, confirming
   the preview lists exactly what it then removes.
6. The hand-written pieces: 1Password native-messaging host, the PWAs, and the
   speaker-eq `exec` (a `manual` step, so run it explicitly).
