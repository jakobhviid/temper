# ROADMAP.md — temper's deferred features & scope

**Bugs** — behaviour that is wrong rather than unbuilt — then the **scope-model
gaps** (each a filled ⚠ or ❌ in the feature matrix), what's deliberately **not**
temper's job, the **composition** with a whole-machine updater, one
**restructuring** weighed and declined, the **suggestions** raised but not ruled
on, the macOS claims only a Mac can settle, and the migration-verification gap.
Each item has why it's parked, the current mitigation, and enough of a sketch to
act on cold.

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

**A password prompt in the package phase is invisible, and only its silence is
reported.** `sudo`, `pkttyagent` and PAM write their prompt to `/dev/tty`, not to
the pipes `providers::run_with_spinner` captures, so the question lands under the
live region and the region's redraw erases it. `ui::StallWatch` bounds the damage
— a child that says nothing for 90 seconds stops the animation and prints a line
telling the user a hidden prompt may be waiting, and since a static line
overwrites nothing, the password can be typed and the run continues. What the
user never gets is the question itself: which account, which tool, which of three
possible prompts. They are told to answer something, not what.

Two things would close the rest, and both are known:

- **Spend the signal temper already computes.** `sudo::reusable_by_children()`
  answers whether a child can use a credential temper holds, and `acquire`'s doc
  records the consequence when it cannot — "the run continues exactly as it did
  before, with Homebrew prompting for itself when it gets there". A phase that
  knows a prompt is coming could stream that child instead of animating it, the
  way `-v` already does at all four `run_child` sites, and the prompt would arrive
  intact rather than 90 seconds later in paraphrase.
- **Decide what "could prompt" means for polkit.** It is not sudo, and
  `sudo::cached()` says nothing about it, so `rpm-ostree` and a system-scope
  `flatpak` need their own answer. flatpak's `--noninteractive` does not supply
  one: it is documented as *"Produce minimal output and avoid most questions…
  suitable for use in non-interactive situations, e.g. in a build script"* —
  flatpak's own questions, with polkit unmentioned. The no-interaction flag that
  lets GNOME Software avoid dialogs is library-side and nothing says the CLI
  option sets it, so plan for flatpak being a prompter.
  → `flatpak update --system --noninteractive` over ssh, with no polkit agent.

Prior art agrees on the shape and only half of it transfers. Mole's `mo clean` hit
this exactly (tw93/Mole#1084) and fixed it (#1085) by putting `sudo -n` on every
privileged call so it fails closed, plus adopting a cached sudo session before the
phase begins; the general CLI pattern is to probe `sudo -n true` and branch before
any animation exists. temper cannot do the first half — Homebrew shells out to
`/usr/bin/sudo` from its own Ruby, and there is no flag to add to someone else's
invocation. The timing half is the part that applies.

The step phase has the whole answer already, because there temper owns every
escalation: `plan::apply_one` clears the region for any step `may_prompt` names —
`exec` and `sysfile`, the two primitives that reach `sudo` — so a password or
fingerprint asked there is legible.

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
- **`rpm-repo`'s four `n/a` columns** — deliberate, and each one a decision worth
  recording rather than re-arguing:
  - *No `prune`, no `[ignore].rpm_repo`.* Most of `/etc/yum.repos.d` is the base
    image's — fedora, updates, rpmfusion, terra — so enumerating the directory as
    extras would report a wall of state temper never wrote and must not remove,
    and the ignore list would exist only to silence it again. What temper *did*
    write is in the ledger, which is the `residue` column.

    A third kind exists, and this rationale must not read as though it does not:
    a repo left by whatever managed the machine before temper is neither image
    content nor ledger residue, so no direction reports it and no verb removes
    it. `retire` is its answer — the primitive for exactly the state the ledger
    structurally cannot see — and it composes with a provider that declines
    prune, reporting `retired-present` with a remedy. Exercised on a real
    machine against a stale `claude-desktop.repo`. If the decision is ever
    revisited, the distinction that carries it is temper-written or not, and the
    ledger already draws that line.
  - *No `reconcile`.* Absorbing a repo means copying a file **into** the folder
    and declaring it. Every existing reconcile appends a token to a list; this is
    authoring, and closer to what `snapshot` does. If it is ever built, it is that
    shape, not this one.
  - *No `enabled` / `disabled` field.* A disabled repo is a file whose bytes say
    `enabled=0`, and the declaration is byte-faithful by design — a flag would
    mean temper editing the file it just promised to reproduce. The case that
    motivated it (a vendor `%post` that rewrites its own repo with a dead URL) is
    two owners for one file, which PATTERNS names as an anti-pattern. Owning the
    path stays a deliberate hack rather than becoming a feature.
  - *No `install --config-only`.* Repos converge before the packages that resolve
    from them, so a flag for splitting the two phases by hand has no caller. Not
    to be confused with `update --config-only` above, which is a different verb
    for a different caller and still open.
- **`apt` has the same hole, and is not filled.** "Where do this provider's
  packages come from" is a per-provider prerequisite; `flatpak-remote` and
  `rpm-repo` are two instances of it and apt would be a third. It is not a cheap
  third: deb822 `.sources` files, dearmored binary keyrings under
  `/etc/apt/keyrings` referenced by `Signed-By:`, and a mandatory `apt update`
  refresh. Forcing it into `rpm_repos` would need either a wrong name or a fake
  abstraction over one real instance, so it waits for a real apt target. What is
  already paid for: `rpm_repos` is named for the **format**, not the tool, and its
  capability gate is rpm-ness rather than atomic-ness — so a plain dnf/yum host
  declares repos identically today, and a future `apt_sources` is a schema
  addition beside it reusing the same file primitive and the same pre-package
  stage, not a new mechanism.
- **Two declared repos may define the same repo id, and nothing says so.**
  `rpm_repos` deduplicates by destination *filename*, so two bundles shipping
  `brave.repo` and `brave-browser.repo` that both contain `[brave-browser]`
  install two files defining one repo. rpm-ostree tolerates the duplicate
  silently — it appears twice in the daemon's enabled-repo list and nothing
  errors — which is exactly the shape that has to be caught declaratively,
  because the machine will not complain. Duplicate GNOME extension uuids are
  rejected at load for the same reason, which is the sibling to copy.
  Not a load-time check as written: the id lives inside the asset, and a folder
  must still load where an asset has not synced yet. So it belongs in `drift`,
  as a finding over the declared set — one `rpm-repo` kind already exists to
  carry it.
- **`repo_gpgcheck=1` cannot be bootstrapped by a `key` entry alone**, and SPEC
  says so rather than temper working around it. A key on disk satisfies
  `gpgcheck` (packages); metadata verification needs it trusted before the first
  repomd fetch. Papering over it would mean either editing the vendor's bytes —
  which the byte-faithful decision forbids — or importing into dnf's mutable
  keyring, which was tried and fails too. The author picks among the three
  shapes SPEC lists. Field-observed on Bazzite against Proton's fedora-43 and
  fedora-44 stable repos, with the key both on disk and `rpm --import`ed.
- **No metadata-cache invalidation is needed between writing a repo and
  installing from it**, which is why there is no refresh step to find. Recorded
  because its absence looks like an oversight: `rpm-ostreed` re-reads
  `/etc/yum.repos.d` per package transaction and fetches metadata for any repo
  it has no cache for, without a daemon restart. Observed on one long-lived
  daemon (pid 13246, chronos-redux, 2026-09-09): six transactions enumerated a
  fixed repo set at 140360 solvables, the ghostty COPR file appeared, and the
  next transaction on the same process listed it and reported 140379. Adding a
  speculative `refresh-md` would cost a network round trip on every converge to
  guard a bug that does not exist.
- **Cold one-pass is not field-confirmed yet.** The ordering guarantee is pinned
  by test (`rpm_repos_order.rs`) and both fleet specs are converted, but every
  machine converted so far already had its repo files on disk, so none of them
  exercised a genuinely empty `/etc/yum.repos.d`. The next from-scratch
  converge is the real test.
- **`retire` cannot be made to refuse a path a declared package provides**, and
  the reason is worth keeping so it is not re-proposed. The ask came from a real
  incident: two `.desktop` launchers a spec wrote itself, which the vendors later
  began shipping at the same paths, leaving `retire` pointed at two working files.
  The obvious check is to resolve the path's owning package at plan time and
  refuse. It does not work, measured rather than reasoned: `rpm -qf` on a
  launcher in `~/.local/share/applications` answers "not owned by any package"
  and exits 1, because rpm installs into `/usr` and `/etc` and never into a home
  directory — so the query is blind to precisely the case that caused the
  incident. Building it would add a subprocess call per retired path to every
  prune, catch system-path collisions only, and read as protection where there is
  none. A recurrence check ("prune removed this and it came back") is
  manager-agnostic and does fire, but only *after* the first deletion, which is
  the harm. So this is a spec-authoring hazard, documented in SPEC where the
  field is defined, and `retire_packages` is the declaration for the intent
  behind it.
- **The repo-retention guard has no integration test.** When an un-layer fails,
  `prune` keeps a repo a still-layered package needs to re-resolve. The branch is
  written and the un-layer's honesty is covered
  (`rpm_repos_order.rs`), but reaching the guard itself needs residue at a real
  `/etc/yum.repos.d` path, which a sandboxed test cannot create. Covering it means
  a root-prefix seam for the file primitives — worth doing when something else
  needs the same seam.
- **`profile`** is the weakest row and is honestly scored: no machine scope, a
  GUI-gated apply, no prune, no reconcile, no ignore, not revertible, no
  residue story. It now carries a `ProviderSpec` saying so in seven written
  reasons, so those answers are held to the registry like everyone else's rather
  than being prose nothing checks. Its `observe` column is genuinely ✅ —
  `system_profiler` reads the installed set across both scopes without MDM or
  root — which is why drift on a profile is real even though nothing else about
  it is.

**The provider trait is half built.** `interface.rs` records each provider's
answers as data and cross-checks them against the finding registry, so a
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

## Composing with a whole-machine updater (`update` without the package half)

A whole-machine updater like `topgrade` upgrades every manager temper declares —
brew, cask, flatpak, rpm-ostree, mas, vscode, gnome-extensions — plus dozens
temper has no provider for, and on an image-based host it is the upgrade path the
OS blesses. It has no notion of a declaration, so it cannot converge config,
report drift, or revert a run. That is a clean seam and it points one way: **the
updater owns versions, temper owns declarations**, with the updater as the entry
point and temper as one of its steps.

What temper lacks is an invocation shaped for a caller that has already done the
packages. Parked because three of the four answers below are fleet-behaviour
calls rather than code; the mitigation is a folder-side `exec` step that runs the
updater from inside a converge, which is the opposite edge and cannot coexist
with this (see below).

- **Skip the upgrade, keep the trust.** The brew/flatpak upgrade is the caller's
  job; tap-trust is not — brew silently skips formulae from an untrusted tap, so
  dropping the trust step leaves the *caller's* `brew upgrade` quietly
  incomplete (#6). The mirror of `install --packages-only` in the existing
  vocabulary is `update --config-only`. Naming the flag for one caller
  (`--topgrade`) reads well at the call site and breaks #13 the moment a second
  updater exists: the engine is not supposed to know who is calling.
  Recommendation — ship the contract as the flag, and let a caller-named preset
  be sugar if it is wanted.
- **"Leaves no barriers" is a contract, not a flag name.** It means: never read
  stdin, never block on a human, and announce every skip that follows (#6, and
  each new human-facing line needs its `!json` guard). Four places can block —
  sudo acquisition for casks needing root, the version-skew self-update prompt
  (`update.mode`), a git push whose credential or passphrase prompt goes to the
  tty, and an `exec` that prompts (already a PATTERNS anti-pattern). Each needs a
  decided answer: pre-authorized, `off`, or reported-and-skipped. A step skipped
  for want of a password is a finding, not a silent pass.
- **The opposite edge has to be dropped, not left as a fallback.** A folder-side
  recipe that calls the updater from an `exec` step is a *soft* dependency
  (`when = { binary = … }`), which makes keeping it look free. It is not: with one
  user-owned updater config, `temper update` → `exec` → updater → `temper update`
  recurses, and a throttle stamp cannot break the loop, because the stamp is
  written after the sweep the inner run is still inside. `--config-only` does not
  help either — the recursion goes through the `exec`, not the packages.
- **The invocation itself is recipe-side.** On an image-based host the
  distribution's updater config lives under `/usr`, which is image content, so
  the entry that calls temper belongs in a user-owned config that temper deploys
  with `copy`. Bootstrapping it through the folder is not a cycle: nothing calls
  temper until the config is in place.

Unaffected either way: `drift`, because temper declares presence and never a
version, so a version moved by the updater was never drift; and `undo`, because
config writes are journaled like any other while the package half belongs to the
caller and is unjournaled on both sides of the seam — which is a plan-time
sentence, not a footnote (#8).

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
  and `undo` tries the whole set in one invocation. mas 1.8's usage line reads
  `mas uninstall [--dry-run] <app-id>` — singular. The blast radius is one
  invocation, not the run: `undo`'s package path is batch-then-isolate, so a
  batch that fails argument parsing retries one id at a time and each app is
  reverted or named individually. What is left to settle is whether the batch is
  wasted work on every mas revert, and whether `interface.rs` is documenting an
  arity mas does not have.
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
  unprivileged `undo` sees nothing to undo. temper reports it at startup —
  `journal::sudo_split_state_root` — naming the path the record went to and the
  two ways out. What a Mac still has to settle is the *path*: the warning tests
  for `/var/root`, which is root's home on macOS by convention, and no Linux box
  can confirm that a Mac under `sudo` actually resolves `HOME` there. (Not
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
provider ten columns, each `Yes` / `No(reason)` / `NA(reason)` — drift is derived
from observe and the two declaration cells rather than declared a fourth time —
cross-checked
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

The gain is also smaller than a grep suggests. Of the 55 per-manager match arms
(`grep -cE 'Manager::[A-Za-z]+.*=>'` across `temper-core/src`), **28 are in
`packages.rs`** and are total mappings (`as_str`, `journal_provider`,
`ignore_list`) — already exhaustive, already a compile error on a new variant,
and better left as matches. The real dispatch fan-out is the remaining 27, split
across `providers.rs` (18), `reconcile.rs` (8) and `plan.rs` (1). The counting
command is written down because the ratio is the argument, and a bare number
ages into something nobody can re-derive.

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

## Suggestions (raised, not ruled on)

Ideas with a case behind them and no decision taken. Each names what it would
fix, what it would cost, and what has to be answered before it could ship.
Nothing here is scheduled, and nothing here is refused.

### A way to declare a key *absent* — `setkey`'s missing delete

**The defect that raised it.** `setkey` sets a key and can never unset one. Drop
the step and the key stays on every machine that ever applied it, with nothing
reporting it — the file primitives' residue problem one level down, inside a
store temper does not own outright.

A folder hit it removing an MCP server from an app's config. The entry temper had
`setkey`'d carried a bearer token, so undeclaring it left a live credential in
`~/Library/Application Support/<app>/config.json` on every Mac that had
converged. Neither existing escape hatch fits. `retire` takes paths, and the path
here is a file temper must not delete — the other twenty keys in it belong to the
app. `exec` works, and is what the folder shipped: a `jq` one-liner, a second
script as a `check` hook so a clean box stops reporting work it never did, a
`run = "always"` step, and a TODO entry to delete the pair once the fleet is
clean. Four artefacts, nothing journaled, nothing undoable, to express *this key
must not exist*.

**Why this is the cheap case and not the hard one.** `ARCHITECTURE.md` states the
rule: *enumerable state needs no tombstone; non-enumerable state does.* A
`setkey`-owned key is the most enumerable state temper has — the manifest names
the backend, the file and the key, so what to remove is known exactly before the
converge and checkable after it. `key absent` is a real drift answer, unlike an
`exec`, where running it *is* the change. Every property that makes file residue
expensive — a ledger to build, hashes to guard it, resolved-key comparison — is
free here, because the declaration already *is* the enumeration.

It is also the recurring defect `AGENTS.md` names, in a kind of state nobody
checked for it: one direction of the matrix shipped and the other did not.
`declared-but-absent` has an answer — apply writes the key. Present-but-no-longer-
declared has none, in either column: no verb changes the machine, and the spec
edit that would express it has no field to write.

**The suggestion.** Two spellings, both worth writing out because they put the
feature in different halves of the model.

- **A step field** — `setkey = { backend, file, key, absent = true }`, or a
  sibling `unsetkey`. Ordered among the bundle's other steps, gated by
  `os`/`role`/`when` like anything else, and journaled as a file write exactly as
  a `setkey` on that file is today, so `undo` needs nothing new. `value` and
  `absent` are mutually exclusive: declaring both is a parse error, not a
  precedence rule.
- **A scope-level tombstone** — `retire_keys` on `[[machine]]`/bundle, beside
  `retire` and `retire_packages`, enacted by `prune` with the same confirm and
  listed by `temper retired`. The better classification, since this *is* a
  tombstone: temporary, and deleted once the fleet is clean. The addressing
  fights it, though — a key needs backend + file + key, which is three fields
  crammed into one string, or a list of tables where every other retire list is
  a list of strings.

The step field is the smaller change and the one a folder reaches for mid-edit;
the tombstone is the honest classification. They are not exclusive.

**What has to be decided first.**

- **Silent when already absent.** A delete on a missing key has to be in sync and
  quiet, or every converged machine reports work forever — the exact failure the
  `exec` workaround needs a hand-written `check` to dodge. That settles drift too:
  `absent` is in sync when the key is gone, drifted when it is present.
- **Does it remove a value temper never wrote?** The cautious reading is a value
  guard — delete only what still matches what temper last wrote, *report*
  otherwise, which is `undo`'s hash rule applied to a value. The case that raised
  this argues the other way: the value was a secret that had since rotated, and
  matching it would have been the one thing that failed. Unconditional deletion,
  with the old value journaled so `undo` restores it, is the likelier answer.
- **Empty ancestors.** Deleting `mcpServers.searxng` leaves `mcpServers: {}`. A
  deep dotted `setkey` creates its intermediate objects, so a symmetric delete
  should drop the ones it created — but nothing distinguishes those from a map
  the app wrote. A decidable rule: prune an ancestor only when it is empty *and*
  no other declaration names a key beneath it.
- **Per backend, and only where it means one thing.** json/toml/ini delete
  cleanly. `defaults` has `defaults delete`, but is not journaled, so `absent`
  would be irreversible there — consistent with `setkey(defaults)`, and worth
  saying out loud. dconf has no delete at all: `dconf reset` restores the
  schema's default, which is a different claim, and it collides with snapshot
  ownership (a `setkey`-owned key is stripped from a capture; an `absent` one
  would have to be too). json/toml/ini first is defensible. Note this does not
  fix the dconf `residue` ❌ above — a retired *subtree* is the case where
  nothing enumerates the keys, which is the opposite problem.
- **Ownership.** `absent` is an ownership claim like any other, so a folder
  declaring both a `setkey` and an `absent` for one key is a two-owner conflict
  and should fail the parse, exactly as the one-owner-per-key rule reads
  everywhere else.

### Split `role` into purpose and graphical session (`headless`)

**The defect that raised it.** `[[machine]]` carries `role = "desktop" |
"server"`, and every role gate written against it in practice — GNOME
extensions, PWA launchers, speaker EQ, audio devices, a touchpad quirk, an
`os-update` step whose polkit grant needs an active local session — asks one
question: *does this machine have a graphical session?* The word `server`
answers a different one: *what is this machine for?* Most fleets never notice,
because the two answers agree on a laptop and agree on a headless box.

They come apart on a Mac used as a server. macOS cannot be run headless, so such
a machine has a full GUI, a browser and a terminal emulator and is sat in front
of — while its role reads `server`. A folder hit exactly this: the machine's
Brewfile installed a terminal, a browser and a remote-desktop client, its `apps`
list named none of the three because it "was a server", and all three ran
unmanaged. The label had drifted from the machine, and nothing could catch it,
because a label is the one thing temper never observes.

**The suggestion.** Keep `os` and `role`; add `headless = true | false` as the
field the gates actually want.

| machine | `os` | `role` | `headless` |
|---|---|---|---|
| laptop, workstation | `mac` / `linux` | `desktop` | `false` |
| **Mac used as a server** | `mac` | `server` | **`false`** |
| headless Linux box | `linux` | `server` | `true` |

The middle row is the whole argument. It is the row no two-value taxonomy can
express, and it is not exotic — it is every Mac mini in a cupboard.

**What has to be decided first.**

- **Does `role` survive?** Once the gates move to `headless`, a folder can be
  left with nothing that gates on `role` at all — a field maintained for a
  comment. Three honest endings: keep it as a documented label; drop it and let
  `headless` carry the gate; or add no field and rename `role`'s values
  (`graphical` / `headless`) instead. The third is the cheapest and the most
  disruptive — it changes the meaning of a key every existing folder already
  sets.
- **What does an absent `headless` do?** A machine naming no `role` fails a role
  gate closed, on the reasoning that a bundle naming a role describes a group
  and a machine naming none is not in it. A boolean cannot simply inherit that:
  failing closed would mean an unset field silently excludes every desktop
  machine not yet migrated. Defaulting to `false` is the friendlier answer and
  the less consistent one.
- **`init` has to ask.** `temper init --role` prompts for a role today, so
  whatever wins needs the matching question — plus an inference worth trusting
  on a first run (a Mac is never headless; a Linux box with no session is).

### Also raised, deliberately not recommended yet: `desktop_environment`

A `desktop_environment = "gnome" | "kde" | …` field on `[[machine]]`, so a
bundle could branch on the DE. **This is a consideration and no call has been
made** — recorded so the argument does not have to be reconstructed later.

The case against it *today*:

- **The `apps` list already selects the DE.** A folder composes a `gnome` bundle
  or it composes a `kde` one. A DE field is a second declaration of a decision
  already taken, and two declarations of one decision can contradict each other
  — which is precisely the failure `role` produced above. Building a second one
  on purpose wants a better reason than symmetry.
- **It can be probed.** `$XDG_CURRENT_DESKTOP`, or an ordinary `when` binary
  probe, reads the live answer and cannot drift from the machine the way a
  declared label can.
- **A field with one value is a constant, not a taxonomy.** Nothing settles that
  except a fleet actually running two DEs.

What would change the answer: a *single* bundle that must branch on the DE
rather than split into two, or a provider whose observation needs the DE before
it can ask the machine anything. Either turns the field from redundancy into
information.

**The line both suggestions share.** *Declare intent, probe circumstance.* A
field on `[[machine]]` is a hand-maintained claim that can drift from the box; a
`when` probe cannot, which is the same instinct behind gating a step on reality
rather than on a machine name. `headless` is intent — this box is not meant to
be sat in front of — and is worth declaring. A DE is circumstance, and where it
is not, it is the `apps` list's job.

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
