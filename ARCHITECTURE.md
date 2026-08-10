# temper — Architecture

> **Status: design, sanity-checked.** Drafted from the design conversation,
> then checked against the **entire** ReinstallScripts repo (Mac tree, the
> 1,399-line Linux justfile, and the Linux libs + `install-bazzite.sh`) on
> 2026-07-27. The gaps that pass surfaced are folded in below. Still design, not
> code — but no longer un-vetted.

## What temper is

`temper` converges a machine to a declared spec. You describe what a machine
should be — its packages and its configuration — in a folder of human-readable
files, and `temper` makes the machine match, reports what's out of sync, and can
revert what it changed.

It is the generalization of the `ReinstallScripts` repo: ~3,900 lines of bash
(two aligned platform trees, `just` + `lib/*.sh`) that install and configure
Jakob's fleet of Macs and Linux boxes. That repo works, but its logic is
temper-private glue, duplicated byte-for-byte across platforms, and inseparable
from Jakob's own machines. `temper` extracts the *engine* into one open tool and
leaves the *data* in a private folder anyone can bring.

> **ReinstallScripts is the acceptance spec.** It is proven on Jakob's live
> fleet, so *it* — not this document — is the authority on **what temper must
> do**. Every RIS recipe must have a temper equivalent: a binary verb, or, only
> where a genuine constraint forbids a verb (the bootstrap paradox), a companion
> script that delivers the same result. Where these docs describe behavior that
> disagrees with a working RIS recipe, treat the **doc as the bug** — correct the
> doc (or temper) to match RIS, never dismiss the RIS behavior. "temper does it
> differently on purpose" is only legitimate once the difference is proven at
> least as good on a real machine; until then, RIS wins.

### The core split: engine vs data

- **The engine is the tool.** Public, on the Homebrew tap
  (`jakobhviid/homebrew-tap`, beside `grove`/`amdl`/`dotsync`), identical for
  everyone. It contains **zero** knowledge of any specific machine, person, or
  app.
- **The data is a folder.** Private, per-person. It holds the manifest and the
  real config files. Jakob keeps his in a **git** repo; his wife keeps hers in
  **Nextcloud**; a stranger uses a **USB disk**, Dropbox, or a plain directory.

This is the same move `dotsync` already made to `machine-sync`: take a private
`just`+`yq` shell tool and ship it as an open, manifest-driven Rust CLI where
the config lives in the user's own folder. `temper` does that for provisioning
instead of dotfile-sync, and reuses dotsync's `adopt` verb and mode-enforcement.

### Delivery-agnostic

**`temper` does not *manage* its config folder with git (or any sync client).**
It operates on "a folder that contains a manifest." How that folder arrived —
git clone, a continuously-synced cloud folder, a mounted USB disk, `rsync` — is
not temper's concern. (An `exec` *step* may still shell out to `git`/`curl` for a
specific job, e.g. cloning a tmux plugin manager — that's a step doing work, not
temper managing the folder.)

Folder discovery (built, `discovery.rs`) — first hit wins:

1. **`$TEMPER_DIR`** — explicit override.
2. **Walk up from the cwd** — you're inside the folder (or a subdir of it).
3. **A saved pointer** — `temper setup <dir>` writes `$XDG_CONFIG_HOME/temper/home`.
4. **Auto-scan** — a directory named `steel`, `temper-home`, or `.temper` under
   any of: `~`, a dev parent (`~/Developer`, `~/developer`, `~/dev`, `~/src`,
   `~/code`, `~/projects`, `~/git`, `~/repos`, … — so case/name conventions don't
   matter), or a cloud-sync root (the same set `dotsync` probes: each
   `~/Library/CloudStorage/*` client, iCloud Drive, `~/Nextcloud`, `~/Dropbox`,
   `~/OneDrive`, `~/ProtonDrive`, `~/Google Drive`, `~/Sync`), `/media`,
   `/run/media/$USER`.

   `steel` is in that list because it is the name of the folder temper was built
   for — the author's own fleet spec. Nothing about temper depends on it: name
   yours `temper-home` or `.temper` for the same zero-config discovery, or call it
   whatever you like and point temper at it with `temper setup` or `$TEMPER_DIR`.
   Where these docs show a path like `~/Developer/steel`, read it as "wherever
   your folder lives".

So a folder cloned/synced to e.g. `~/temper-home` or `~/Developer/temper-home` is
found with **no configuration** — machines just find it. A fresh
box with none of these errors with a message telling you to `temper setup <dir>`
or set `$TEMPER_DIR`. (Discovery only *locates* the folder — temper never clones
or syncs it; that's git/Nextcloud/rsync's job.)

### Humans and LLMs both compose it

The folder is a browsable tree of **real files** — a real Brewfile, a real
`starship.toml`, a real dconf dump — plus one manifest that ties them together.
The bar is "as readable as a Brewfile." The CLI carries the `amdl` house style:
`--json` on every command, an `--llm` guide, a global `-v/--verbose`,
journaled `undo`, human output to stdout / progress + errors to stderr.

A long converge is the one place that house style needs help: hushing the package
managers is right (nobody needs 200 "Using x" lines), but silence for the twenty
minutes it takes to pour `llvm` and `mactex` reads as a hang. So the quiet path
**captures** the manager's output and renders a `grove`-style spinner naming the
package in flight, replaying the whole log if the converge fails and surfacing
warnings even when it succeeds. temper reads Homebrew's `ohai` lines
(`==> Installing …`, `==> Pouring …`) purely as a progress feed — never as a
source of truth about what is installed; that always comes from a probe.

**Capture is not only about noise — it is about voice.** Every child temper shells
out to has its own opinion of the world: `flatpak update` says "Nothing to update."
about *its remotes*, `git pull` says "Already up to date." about *the folder's
upstream*, `brew trust` says "Already trusted tap". Left on the terminal, any of
them reads as temper's verdict on the run, moments before temper installs and
upgrades plenty — and, going to temper's stdout, breaks `--json` outright. So
every converge child goes through one door (`providers::run_child`) and every
phase reports its own effect in temper's words. Three rules fall out, and they
apply to new code as much as old:

1. **A child's output never stands as temper's.** Capture, replay on failure, and
   let warnings through. Stream only where the child's output *is* the operation:
   `prune`'s removals (destructive, confirmed, the user is watching) and the
   self-update's `brew upgrade temper -y`.

   **But a prompt is not chatter.** `sudo`, polkit and PAM write to `/dev/tty`,
   which no pipe of ours can capture — so a live progress region gets *fused* onto
   the prompt and the next tick erases the one message the run is blocked on. Any
   step that may prompt therefore gets the terminal to itself: the step phase
   clears its region for the duration of an `exec`, and `sudo::acquire` asks up
   front, before any region exists. An animated line is not an option there — the
   prompt arrives at a moment we cannot predict (in practice within the first
   seconds), so a slow step is announced **once**, in the same shape as its `✓`
   (`⋯ <label>` then `✓ <label>`), which is the safe half of a spinner.

   Rows are aligned by `ui::Columns`, shared with `drift` so both views measure the
   same way rather than each hard-coding a width. It works because temper **plans
   before it applies**: the item list exists before the first row prints, so widths
   are exact without buffering. Three rules it encodes — measure *display* width
   (a `ø` is one column, two bytes), elide a path from the **head** (the tail names
   the step), and never shorten anything when stdout is not a terminal (a redirected
   log is evidence). Column *order* still differs between the two: `drift` groups
   under an app header, so repeating the app per row would be noise.
2. **Report the effect, never the invocation.** Not "we ran the upgrade" but how
   many packages moved version; not "we called push" but whether the remote moved;
   not "we pulled" but how many commits landed. Measured, never assumed.
3. **Never parse a tool's prose to learn what happened.** git and flatpak
   localize their messages, so string-matching works on the author's machine and
   silently stops working on someone else's. Compare refs, versions, hashes —
   things that mean the same in every locale. (Homebrew's `ohai` lines are the one
   exception, and only as a *progress label*, never as truth.)

The user-facing consequence: **silence means converged.** A run prints a live
region while it works (erased when done), a `✓` line only for something that
actually changed, warnings and errors always, and one summary at the end. `-v`
turns the children back on in full.

---

## Scope decides the verb set

This is the rule the rest of the model hangs off. It is mechanical: given a
declaration's scope, which verbs may touch it is a lookup, not a judgement.

| | **fleet / group scope** | **per-machine scope** |
|---|---|---|
| what it is | a group the machine belongs to — its `os`, its `role`, a bundle it composes | this machine's own declarations |
| drift | ✅ | ✅ |
| install (conform) | ✅ | ✅ |
| prune | ✅ — but only ever for what is **un**declared | ✅ — same |
| reconcile **add** | ❌ never write a shared file from one box | ✅ |
| reconcile **remove** | ❌ | ✅ |

**Fleet is drift + install. Conform.** **Machine is drift + install + prune +
reconcile, both directions.**

The scopes do not differ in whether something can be *removed* — they differ in
**who is allowed to change the declaration**:

- **Fleet:** the declaration changes in the shared spec (a commit — a group
  decision). Every machine's `prune` then enacts it.
- **Machine:** the declaration changes on that box, via `reconcile`. That
  machine's `prune` then enacts it.

So `prune` is the **universal enactment mechanism** in both cases, and
`reconcile` is precisely "edit *this machine's* declarations". The fleet
equivalent of `reconcile` is `git commit`. An item deployed from fleet scope and
later removed from the group becomes undeclared like any other, and prune cleans
it up — that is how a fleet-scope retirement lands on every machine.

Getting this wrong is the tool's recurring defect. Every category that was built
before this rule was written down re-decided its own verb set, and each got it
wrong differently: `gext` could be added but never removed, `[brew].trust` and
`[ignore]` existed only at fleet scope yet `reconcile` edited them from a single
machine, and `rpm-ostree` had no machine scope at all.

**Every category needs both scopes**, because "I want this on this box only" is
an ordinary thing to want. A category that exists at only one scope is not
finished.

## The feature interface

A *feature* is a kind of state temper manages — `brew`, `cask`, `brew-tap`,
`brew-trust`, `flatpak`, `mas`, `vscode`, `gnome-extensions`, `rpm-ostree`,
`dconf`, and the file primitives. A new one (`apt`, `dnf`, `npm`, `cargo`)
becomes possible by answering the same eleven questions, so the building blocks
are identical for everyone and "did we finish it?" stops being a matter of
opinion.

Each column has **one definition**, below, so every implementer means the same
thing. A column may be answered `Unsupported`, but only with a **written
reason** — the awkwardness is the point, because it is the moment to ask whether
the thing simply has not been built.

| # | column | definition |
|---|---|---|
| 1 | **fleet declaration** | which file/key declares it for a group, and what gates it (`os`/`role`) |
| 2 | **machine declaration** | which file/key declares it for one machine. Required — without it the feature has no spec column and cannot be reconciled |
| 3 | **observe** | how it enumerates what is present. Must distinguish *"the tool answered, and the answer is none"* from *"I could not ask"*. The second is `unavailable`, **never** absent |
| 4 | **install / conform** | how it makes the machine match the declaration, and what it reports when it can observe but cannot converge |
| 5 | **prune** | removes what is installed but declared at neither scope |
| 6 | **reconcile add** | absorbs an undeclared item into **machine** scope |
| 7 | **reconcile remove** | drops a **machine**-scope declaration. Never a fleet one |
| 8 | **ignore** | how an item is permanently silenced, and at which scope |
| 9 | **drift** | reports both directions, and names the file the declaration lives in |
| 10 | **revertible** | journaled, or explicitly not — and if not, the user learns *before* confirming |
| 11 | **residue** | what happens to what it deployed when the declaration goes away (see "Retirement") |

The table is also **data**, in `interface.rs`: each provider records how it
answers each column, and tests hold that against the finding registry —
a provider claiming it can prune must have a kind that actually names `temper
prune`, claiming reconcile requires a machine scope to write to, and a declined
column must carry a reason. That is the feature-level version of "advice is a
mutation" (Principle #8), which nothing was checking: the finding registry made a
missing *finding* answer loud, while a *provider* could still claim a capability
with nothing behind it.

It is the registry half, not the dispatch half. Providers still have their own
function signatures; harmonising those behind a real trait is the remaining work,
deliberately sequenced after enough providers fill their columns to shape it.

### Where each feature stands

Filled in so the gaps are *readable* rather than re-argued every time someone
touches one. ✅ done · ⚠ present but wrong · ❌ absent. Columns are numbered as
above.

| feature | 1 fleet | 2 machine | 3 obs | 4 inst | 5 prune | 6 r+ | 7 r− | 8 ign | 9 drift | 10 rev | 11 res |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `brew` / `cask` / `brew-tap` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | n/a |
| `brew-trust` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | n/a |
| `flatpak` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | n/a |
| `flatpak-remote` | n/a | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | n/a |
| `mas` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | n/a |
| `gnome-extensions` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | n/a |
| `rpm-ostree` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | n/a |
| `dconf` | ✅ | ✅ | ✅ | ⚠ | ❌ | ✅ | ✅ | ⚠ | ✅ | ✅ | ❌ |
| `deployed-files` (`copy` / `sysfile` / `block`) | ✅ | n/a | ✅ | ✅ | ✅ | n/a | n/a | ❌ | ✅ | ✅ | ✅ |
| `profile` | ✅ | ❌ | ✅ | ⚠ | ❌ | ❌ | ❌ | ❌ | ⚠ | ❌ | ❌ |

**One command per type, not one per item.** Every provider's CLI takes a list —
`gext install UUID [UUID…]`, `mas install <id>…`, `rpm-ostree install <pkg>…`,
`brew bundle`, `flatpak install` — so a converge issues one invocation per
provider. That is not only faster than N process spawns: for anything needing
root it is the difference between one password prompt and one per item, which is
what decides whether a converge can be walked away from.

The per-item loops that batching replaced were buying something real, though, and
it is kept: a batch that fails says nothing about *which* item failed, and one
bad entry must not strand the rest. So a failed batch is retried per item, which
isolates the damage and names it (Principle #6). Only what actually lands is
journaled, so a failed install never leaves an undo entry for something that was
never there.

**Install and uninstall are a pair, in every provider.** `brew install`/
`uninstall`, `flatpak install`/`uninstall`, `gext install`/`uninstall`,
`rpm-ostree install`/`uninstall` — column 5 is just column 4 backwards, and a
provider claiming it cannot prune is usually comparing itself to the wrong
sibling. rpm-ostree looked exceptional because it stages a deployment and needs a
reboot, until you notice its own *install* does exactly that too.

The one real deviation is **brew**, which prunes via `brew bundle cleanup` rather
than `brew uninstall`, because cleanup is dependency-aware: a formula kept only as
another package's transitive dependency must not be removed. That is a
correctness requirement, and it is the documented exception rather than a second
pattern.

**Column 10 was never a real constraint, and saying so cost a release.** "Packages
cannot be journaled" was repeated until someone asked why: the set temper installs
is known *before* the converge — temper computes what is missing in order to
install it — and every provider's uninstall is its own install backwards. `gext`
and `rpm-ostree` now journal what they installed, and `undo` removes exactly that.

The one genuinely unrevertible operation is an **upgrade** — reverting one means
pinning a prior version whose bottle or commit may be gone — and that belongs to
exactly two providers, because `update` only ever upgrades **brew** and
**flatpak**. temper never runs `rpm-ostree upgrade`: on an atomic host the OS
owns that, layered packages come along with the deployment, and temper only ever
layers or un-layers. `gext` likewise only installs. So for those two the revert
story is unconditional.

**VS Code extensions are deliberately outside this table.** temper converges a
`vscode "…"` token if you declare one, but the probe invariant means a spec that
declares none never runs `code --list-extensions` — Settings Sync stays the sole
registrar. Listing it as a managed provider would claim an ownership temper does
not want.

**Every package provider journals its installs**, so `undo` removes exactly what
a converge added. What remains unrevertible is narrow and specific: the
brew/flatpak **upgrade** phase of `update` (reverting one means pinning a prior
version whose bottle or commit may be gone), and the deliberately-unjournaled
`setkey(defaults)` (`defaults read` loses the value's type), `sysfile` and `exec`.

Those limits are named in the **plan**, not the report. `install --dry-run` lists
every step it would change that `undo` could not revert, each with its reason, so
the answer arrives while the run is still a forecast (AGENTS.md question 7). Only
steps that would actually change something are listed: an in-sync `sysfile` costs
nothing and is not a limit anyone has to weigh.

- **col 4, `dconf`** — `restore` is excluded from `install`/`update`, which is a
  symptom of the recording model, not a property of the store (see below).
- **col 4, `profile`** — its apply is a GUI dialog a human must approve, so it
  cannot converge unattended or headless.
- **col 9, files/profile** — one direction only: there is no extras direction,
  because of col 11.
- **`block` residue is the region, not the file.** The file belongs to the user —
  a `.zshrc` — so retiring a block is an *edit*: the marker-delimited region is
  removed and the file stays. The ledger records `(file, marker)` and hashes the
  region body, so the same "untouched or reported" guard applies to the part
  temper actually wrote. Recording the path and deleting it would have been the
  most destructive thing in the tool.

### Naming: be specific, minimise overlap

A feature is named for the **mechanism**, not the family or the desktop, because
the family will get a second member and the desktop will get a second store.
`rpm-ostree`, not `rpm` — a future `apt` is a different feature, not a variant.
`brew-trust`, not `trust` — flatpak remotes and apt keys are also trust.
`gnome-extensions`, not `extensions` — VS Code extensions are managed here too,
and that collision is already live. `snapshot-dconf`, not `snapshot-dconf` —
dconf is present under KDE, so the desktop was never the right noun.

## Retirement — what happens to residue

Removing a declaration leaves residue, and whether that matters depends on one
property:

> **Enumerable state needs no tombstone. Non-enumerable state does.**

Packages self-clean: drop it from the spec, `prune` sees an extra, it goes.
Nothing accumulates. Files do not — temper keeps a per-run journal for `undo`,
not a ledger of what a spec deployed, so a deleted `copy` step leaves its file on
every machine forever with nothing reporting it.

So retirement is two mechanisms, not one:

- **A deployment ledger** makes file residue enumerable, which gives the file
  primitives a real extras direction and lets `prune` answer it like any package.
  It is hash-guarded exactly as `undo` is: remove it if it is unmodified since
  temper deployed it, **report** it if you have edited it. Most retirements then
  need no tombstone at all.
- **An explicit tombstone** — a `retire` list, at bundle or machine scope —
  covers what a ledger structurally cannot: files deployed before the ledger
  existed, and things temper never deployed but that must be gone. It is
  distinct from `[[assert]] absent`, which *reports* a condition you resolve
  yourself; a `retire` entry temper enacts, via `prune`, with the confirm every
  destructive thing gets.

Tombstones are reviewed, not expired — `temper retired` lists every entry and
whether it is still doing work, which is what stops them accumulating unseen. A
date on one would be **metadata a listing sorts by**, never a trigger: behaviour that changes with the wall clock would mean two
machines on the same commit doing different things, and a machine offline past
the date would skip the retirement silently. The review sweep is a verb that
lists tombstones oldest-first — which is what stops them accumulating unseen,
without a second folder to become a junk drawer.

## dconf has three owners, not one store

A desktop key store is not one feature. Measured on a real fleet, **95% of the
keys in a whole-desktop snapshot were extension settings** (262 of 274 on one
machine, 267 of 280 on another); the remainder was five sections, one of them
global shortcuts. Treating that as a single blob made the snapshot a *second
owner* of keys a bundle already declared, kept apart only by a hand-maintained
`strip` list that rots the moment you forget an entry.

Split by who owns the key:

| owner | what it covers | how it is declared |
|---|---|---|
| **the extension** | `/org/gnome/shell/extensions/<uuid>/…` | `settings = "…"` on the extension — synthesised into a snapshot rooted at its own subtree, so capture, restore, drift and per-section reconcile are the machinery that already existed |
| **policy — always set** | a value the fleet or the machine insists on (global shortcuts, 1Password) | a `setkey` step |
| **policy — always absent** | a key that must not be set | the absence primitive, not a captured value |
| **machine-specific live state** | the residue: genuinely this-box-only settings | a narrowly-rooted `[[machine.dconf]]` |

Three consequences follow, and they are the reason for the split:

- **Settings inherit the scope of the thing that owns them.** An extension
  declared in a bundle carries its settings at fleet scope; one declared on a
  machine carries them per machine. No new scope rule — the existing one, applied
  to a smaller unit.
- **Enabled is a field on the declaration**, not a separate captured fact. A bare
  uuid means installed *and* enabled; `{ uuid = "…", enabled = false }` means
  installed and switched off. temper asserts its own declarations with
  `gnome-extensions enable/disable` — a **union**, never a rewrite of
  `enabled-extensions`, which would drop the image-baked extensions temper does
  not declare. Previously these were two unlinked facts, so a uuid enabled in a
  snapshot but declared nowhere got switched on by `restore` and never installed
  by `install`; GNOME fails soft, so nothing said so.
- **`strip` goes back to one job.** Its ownership half is **derived**: temper
  already knows every dconf key the machine's bundles declare via `setkey`, so it
  excludes them from capture and from both sides of the drift comparison, and
  reports how many it left out. What remains in `strip` is a noise filter
  (`monitors/`, `last-selected`) for keys that would corrupt a capture→restore
  round trip. Ownership is matched **exactly** — a `setkey` on `a/b` does not
  take `a/bc` with it — because ownership is not a pattern.

An extension owns **only its own subtree**. Anything it touches outside that is
shared keyspace — two extensions can both want
`/org/gnome/desktop/interface/gtk-theme` — so an implicit ownership claim there
would be exactly wrong. Out-of-tree keys are declared explicitly as policy:
always-set, or always-absent.

This also bounds the blast radius. "The live dump came back empty, so drop every
captured key" needs a *whole-tree* dump to be dangerous; a per-extension capture
is bounded by an extension you declared and could observe.

## Constraints on a second settings backend

`dconf` is the only settings store implemented. KDE (KConfig), COSMIC, and macOS
(`defaults`) are the candidates for a second, and each fails the current model in
a *different* place — so the seam must be shaped by all three, not extrapolated
from dconf.

| store | dump/load pair | sectioned `key=value` | a subtree is one prefix |
|---|---|---|---|
| dconf | yes | yes | yes |
| KConfig | **no** — files under `~/.config` | yes (INI-shaped) | **no** — a *set* of files |
| COSMIC | **no** | **no** — one RON file per key | yes (`~/.config/cosmic/<c>/v<N>/`) |
| macOS `defaults` | per domain | plist (typed, nested) | **no** — N unrelated domains |

What actually generalises is smaller than it looks: the **diff core** — a map of
`(section, key) → value`, diffed both ways, grouped by section, absorbed per
section. Parsing, serialising, the id grammar, the transport and the reload are
all per-store.

Specific traps, each of which has already cost someone somewhere:

- **dconf's model rests on an invariant the others lack.** dconf stores only
  *non-default* values, so an absent key means "the schema default" — a known
  value. macOS has no such registry: absent means "whatever this app registered",
  which varies by app version. So "declared, not present ⇒ missing" is meaningful
  on dconf and meaningless on `defaults`, and dropping a key as the absorb action
  is safe on one and a guess on the other.
- **The journalable grain and the reviewable grain can differ.** A `defaults`
  domain round-trips as a blob but not per key; per-key prompts are what makes
  reconcile reviewable. A store where those disagree gets coarse drift, and that
  must be stated rather than discovered.
- **KConfig's syntax breaks a naive INI reuse**: nested `[A][B][C]` headers, entry
  flags (`Key[$e]`, `[$i]`), locale suffixes (`Name[de]`), and an `/etc/xdg`
  cascade where the user file holds only overrides. A writer that does not know
  about `Key[$e]` appends a duplicate key — corruption, not a formatting nit.
- **Section identity is not always stable.** COSMIC's `v<N>` directory means a
  version bump renames every section at once, so everything reads as
  simultaneously missing and extra.
- **Reload is a store property.** dconf notifies over D-Bus, so `load` is live.
  KDE needs per-component pokes; without them a write is correct and invisible
  until next login. That belongs in the feature's column 4 answer, not in a
  footnote.

### Two decisions recorded, so they are not re-proposed

**There is no `desktop` axis on a machine.** It was proposed and rejected: it
duplicates the capability question (what would `desktop = "gnome"` gate that
"is `gnome-extensions` present" does not?), it cannot describe a box with two
desktops installed, it carries nothing on macOS beyond `os = "mac"`, and it
cannot express Wayland-vs-X11 — a third axis it would immediately need. Where a
store must be named, the **backend names itself**; where a tool must be present,
that is a capability. Both compose; enum axes multiply.

**The desktop is not what makes a machine a desktop, either.** `role` already
carries "has a graphical session" *and* "extensions are meaningful here" *and*
"desktop rpms are wanted here". That overload is why the gate is worth fixing
rather than supplementing.

## Two scopes

Configuration lives at two scopes, and the distinction is load-bearing. See
"Scope decides the verb set" above for what each scope *permits*; this section is
about how drift is *computed* at each.

### Machine scope — aggregate / snapshot

Things that must be computed *whole* to be correct, or that represent a
machine-wide state:

- **The effective package set** (brew / flatpak / mas / gext / rpm-ostree).
  Package drift is a dependency-closure computation, not a set subtraction (see
  below), so it can only be evaluated against the *complete* declared set for a
  machine. The declared set = union of composed apps' packages **+ a per-machine
  loose list + minus an ignore/baseline list** (see "Machine model").
- **Whole-desktop dconf** — the entire GNOME shell / Ptyxis state. Captured
  through a configurable **strip-keys** filter (bookkeeping + per-monitor panel
  keys that would corrupt a capture→restore round-trip). The filter is a manifest
  field, not tool-baked knowledge, and it is applied to **both** sides of a drift
  comparison so a stripped key never reads as drift.

Drift at machine scope is a set/snapshot operation. For dconf it is key-level,
grouped by the sections the dump itself defines — which is why a snapshot rooted
at a narrow subtree (`/org/gnome/shell/extensions/`) yields one reconcile prompt
per extension without the engine learning what an extension is. The grain is the
data's, not the tool's; the manifest chooses it by choosing `path`.

### App scope — the composable library

The per-app recipes: a config file to deploy, a key to set, a one-time setup
script to run. The open, composable library. Each machine picks which
app-bundles it wants. Drift at app scope is per-file / per-key / per-assertion.

---

## The taxonomy: four layers, two are code

1. **Primitives — closed set, tool code (Rust).** The atomic operations:
   `copy`, `block`, `setkey`, `profile`, `sysfile`, `exec`. Adding one is a big
   deal (new release, new drift/undo logic).
2. **Providers — open set, tool code, behind an interface.** The converge
   providers: `brew`/`cask`/`brew-tap`/`brew-trust`, `flatpak`, `mas`, `vscode`,
   `gnome-extensions`, `rpm-ostree`, and the settings stores. Adding one is
   neither a big deal nor free — it is **routine**, because it answers the
   eleven-column interface above rather than inventing its own verb set. This
   tier used to be filed under (1), which is why each provider decided its own
   verbs and each got it wrong differently (Principle #1).
3. **App-bundles — open set, user config (no code).** A named, ordered list of
   primitive steps, each OS/role-gated. Where ghostty and 1Password live. "The
   next ghostty clone" is a new *config file you write*, never a tool release.
4. **Machine registration (`temper.toml`).** Which machines exist (name + OS +
   role) and which bundles + loose packages + ignores each one composes.

### The primitives (closed set) and the providers (open, behind the interface)

Both are listed together below because they are both tool code. The rows for
`brew`, `flatpak`, `mas`, `gnome-extensions` and `rpm-ostree`
are **tier 2 — providers**, not primitives: they are the implementations the
eleven-column interface exists to standardise, and a future `apt` or `npm` joins
that list without touching tier 1.

| Primitive | Scope | What it does |
|---|---|---|
| `copy` | app | deploy a file/dir → target(s). Modes: `verbatim`, `template` (variable + apply-time-probe substitution), `seed` (create-once, then hands-off, excluded from drift). Fields: `to`, `mode` (file perms), `template`. |
| `block` | app | ensure a marker-delimited block / line is present in a user-owned file, idempotently (the grove-`setup` pattern: SSH `Include`, zshrc `source` line). |
| `setkey` | both | set one or more keys in a structured store, preserving siblings. **Backends:** `dconf`, macOS `defaults`, `ini`/`.desktop`, `json`, `toml`. Supports **list-append** (json/toml/dconf array-union, e.g. dconf `custom-keybindings`) and opt-in apply-time value templating (`template = true`, all backends). The `json` backend is **JSONC/comment-preserving** (reads `//` + trailing commas, writes without reformatting). Generalizes the old standalone `dconf`. |
| `brew` | machine | converge the aggregate Brewfile (`brew bundle`); internalizes tap-trust (and drift-checks it both ways vs `brew trust --json`); knows the `vscode` sub-type |
| `flatpak` | machine | converge the flatpak set (with ignore-list); `flatpak override` env/perms is a `setkey`-style op on the override store |
| `mas` | machine | converge Mac App Store apps in a separate, **forgiving** `mas install` loop — skips apps already installed (per `mas list`), mutes Spotlight-reindex noise (`MAS_NO_AUTO_INDEX`), and a MAS failure is warned + skipped, never fatal (see below) |
| `gext` | machine | converge GNOME extensions (install from EGO + `gext update`); distinct from *enabling* them (a dconf key) |
| `rpm-ostree` | machine | layer an rpm that can't be image-baked (proton-vpn); emits a **reboot-required** signal temper reports but never automates |
| `profile` | app/machine | install a macOS `.mobileconfig` — **weaker contract** on the *apply* side only (a GUI `open` the user approves; not undoable). Drift is real: the file's `PayloadIdentifier` is matched against `system_profiler`'s installed profiles (user **and** device scope, no root, no MDM), giving `missing` / `drifted` (installed but the source moved, per a content stamp) / `in sync`. A **signed** profile is CMS-wrapped, so its identifier can't be read → `unavailable` |
| `sysfile` | app | write one **root-owned** system file (`/etc/…`) with mode/owner/group, escalating internally (`sudo install`) for just that write. Drift compares content + mode + owner; not journaled (system-side) |
| `exec` | app | run a user-supplied script — the escape hatch (see "exec's contract") |

Every primitive is **planned and drift-checked**. The **file-writing** ones
(`copy`, `block`, `setkey` json/toml/ini) are **journaled** for `undo` (the
`plan.rs`/`apply.rs` shape from `dotsync`, the journal from `amdl`), and so is
**`setkey(dconf)`** — dconf values round-trip cleanly, so undo snapshots the
prior value and restores it (or resets a previously-unset key). The same holds a
subtree at a time for **`restore-dconf`**: undo stores the unfiltered prior dump, then
reverts by `dconf reset -f` **then** reload — a bare reload merges, and would
leave behind every key the restore introduced. Its guard is the strip-filtered
dump, since a raw-dump guard would go stale within minutes of desktop churn and
silently disqualify every undo. **`setkey
(defaults)`**, `sysfile`, and `exec` stay **not journaled**: `defaults read`
loses the value's type (an undo couldn't rewrite it faithfully), and `sysfile`/
`exec` mutate root-owned/arbitrary state — so `undo` can't revert them. All the
system-side backends degrade to `unavailable` in drift when their tool is absent
rather than aborting.

### Dynamic (apply-time) values

`template` (`copy`) and `setkey` (opt-in `template = true`) values may be
**resolved from live state at apply time**, not just from declared vars:
`{{ which "ghostty" }}` (absolute path — GNOME's PATH excludes the brew prefix,
so keybinding commands must be resolved on the box), plus `{{ env "…" }}`,
`{{ var "…" }}`, and `{{ brew_prefix }}`. `setkey` is literal by default (static
drift = trivial equality); `template = true` opts a value in and renders its
string leaves on every backend. Drift on a dynamic value compares
**semantically** (does the current value equal the re-resolved probe?), never
byte-for-byte — a byte compare would report permanent false drift.

### Composition modes live in providers, not the schema

There is **no generic `merge` mode**. dconf, ghostty, and Brewfile each need a
different algorithm, so a universal "merge" would be a lie. `copy` is
`verbatim`/`template`/`seed`; structured key-*setting* (preserve siblings) is
`setkey`, parameterized by backend; whole-file/whole-subtree *merges* (dconf
snapshot load order, the extensions-sync union-add) are **provider-internal**.

---

## Package managers = converge + probe

Every package manager is two operations:

- **converge** — aggregate install + whole-set drift: `brew bundle`,
  `flatpak install`-set, `mas install`-set, `gext install`-set, `rpm-ostree`.
- **probe** — "is *this one* present?": `brew list` / `flatpak info` /
  `mas list` / `gext` / `rpm -q` / `command -v` / path-exists.

### Compile-time compose, run-time aggregate (the brew rule)

App-bundles declare packages as **pure data** tokens. `temper` collects the
**union** of a machine's composed apps' packages **+ its loose list, minus its
ignore list**, synthesizes **one** effective Brewfile per manager, and makes
**one** converge call. Composition happens on paper; each manager runs once.

This is required for *correctness*, not speed: `brew bundle cleanup` is
dependency-aware — a formula kept only as another package's transitive dependency
is **not** listed as an extra. Split brew per-app and cleanup can't distinguish a
real orphan from a shared dependency; drift gets **wrong**. (RIS documents this
in `brew_cleanup_extras`: "*a kept entry's deps are NOT listed — which is why
this can't be replaced by naive set subtraction*.")

**MAS.** `mas` is converged **separately** from the aggregate `brew bundle`, in
its own `mas install` loop, because it is the flakiest provider (no App Store
sign-in, an app not tied to the Apple ID). A MAS failure is **warned (to stderr)
and skipped**, never fatal — so it can't abort the rest of a converge
(Principle #6). Sign in to the App Store first for the installs to succeed.

### The gate: presence probes config

> **Status: BUILT.** `when`/`needs` presence gating is implemented (`probe.rs`);
> steps also still gate on `os`/`role`.

Config runs in a second phase; each app's steps are **gated on a presence
probe** — "is this actually here? → run my config." The gate checks *reality,
not intent*: on Linux, **Ghostty is baked into the image and in no Brewfile**, so
a gate of `when = { binary = "ghostty" }` fires correctly however it was
installed. Probe vocabulary (declarative, exactly one per probe): `binary` /
`brew` / `cask` / `flatpak` / `mas` / `gext` / `rpm` / `path` / `exec`. `when`
skips the step when the probe fails; `needs` errors (a hard requirement).

**Skips are loud** (Principle #6): install/update print
`⚠ ghostty  copy  ~/.config/ghostty/config — skipped: binary \`ghostty\` absent`
as the phase reaches the step, and `drift` reports the gated-out step status-only
(never as red drift). (The implicit "my declared package is installed" default is
not inferred — declare the probe explicitly.)

### The cask-artifact exception (a named Principle-#2 violation)

App-scope config sometimes patches `.desktop` files that brew records as **cask
artifacts** (1Password, VS Code). The *next* machine-scope `brew bundle` then
refuses to upgrade the modified artifact, so it must be reset to pristine first,
then re-patched. This is a genuine app→machine effect-dependency that no clean
two-phase model absorbs. temper handles it explicitly: a cask can be **annotated
"config patches my artifacts → reset-before-converge,"** and the brew provider
honors it. Documented as an exception rather than pretended away.

---

## Drift is a subsystem, not a diff

Drift is more than "package-set + file-byte + key." It also evaluates
declarative **assertions** and **exec drift-hooks**, and reports **status-only**
items:

- **`assert`** — checks that aren't a converge action: `absent` (must-not-exist —
  `~/.zshrc.local`, retired PWAs), `mode` (root:root 0755 on
  `/etc/1password/…`), `contains-line` (`~/.zshrc` sources `.image`),
  `not-member` (user not in `onepassword` group), `executable-resolves` (a
  keybinding command is on PATH), `json-semantic` (order-independent
  missing-vs-extra — the brave policy), `shell` (default login shell).
- **exec drift-hook** — an `exec` step may supply a companion **check** script
  that reports in/out-of-sync (exit code + message). Without it, anything pushed
  to `exec` loses drift coverage.
- **status-only** — items temper drift-*reports* but has no verb to repair (image
  origin, the image-baked brave policy). Read-only.

This is where a third of `just drift`'s real value lived; the model now has a
home for it.

---

## Lifecycle

Steps declare which flows they participate in; the default derives from the
primitive, so the modifier is written only for exceptions.

| Value | Runs during | Notes |
|---|---|---|
| `always` | install + update | default for `copy`/`template`/`setkey`; re-applied and drift-tracked |
| `install` | install only | default for `seed`, `profile`, one-time `exec`; update skips (reloading whole-desktop dconf clobbers live tweaks, and re-opening System Settings for a declined `profile` would nag every run) |
| `ensure` | install + update, **install-if-missing only** | the corrected "update installs a little": backfill `grove`/`amdl`/`pwtune` and the zsh tool set if absent, without upgrade-churn |
| `manual` | never automated | only when explicitly invoked (`restore-dconf`, `speaker-eq`, `eq-import`) |

Enforcement steps that today re-run every `update` (git identity via
`git config`, default shell via `chsh`) are `exec` with `run = always` + a drift
hook, so `update` keeps re-asserting them.

The two automated flows:

- **`temper install [machine]`** — full converge: add missing packages, apply
  everything, run one-time setup + profiles + dconf reload.
- **`temper update`** — upgrade packages + re-apply `always` + honor `ensure`
  (install-if-missing for the allowlist). Does **not** add newly-declared apps
  wholesale — adding an app is an `install`. *(Corrected from the first draft's
  "update never installs," which the repo disproved.)*

---

## exec's contract

`exec` is the pressure valve, but its execution *context* is now defined, not
assumed:

- **Privilege** — a step may declare it needs `sudo` (the `/etc/1password/…`
  edits, `rpm-ostree`). `plan` shows it; `undo`/journal semantics for privileged
  system mutations are best-effort and labeled as such.
- **One password per run** — root is needed in several unrelated places (a
  pkg-based cask's installer, a `sysfile` write, `rpm-ostree`), each of which
  would otherwise prompt on its own schedule, minutes apart, because sudo's
  timestamp expires during the downloads between them. A mutating run therefore
  determines up front whether it needs root at all (`providers::casks_needing_root`
  — a batched cask-artifact query over just the packages this run would touch),
  asks **once** with a reason if so, and keeps the timestamp warm for the duration
  (`sudo::keep_alive`, a `sudo -n -v` refresh that can never itself prompt).
  Nothing needed → no prompt; `--dry-run` and every read-only verb never ask.
- **Secrets / env** — a step may declare env vars / a `secrets/` source to pass
  through (the `ACOUSTID_KEY` amdl case). The private folder makes a `secrets/`
  dir viable; this is the mechanism that consumes it.
- **Drift hook** — the optional companion check script above.

---

## Engine operations

All `--json`-capable, all with an `--llm` guide, mutating ones journaled for
`undo`:

- **`install [machine]`** / **`update`** — the two lifecycle flows. A live
  `install` refuses to run when the machine's `os` ≠ the host os (drift and
  `--dry-run` work from any host; only a converge is host-guarded). `manual`
  steps are skipped by both flows.
- **`drift [machine]`** — read-only: package set + tap-trust (`[brew].trust` vs
  `brew trust --json`, both directions) + every managed file + keys + assertions
  + exec-hooks + installed macOS profiles. Findings are `ok` / `drifted` /
  `missing` / `untrusted` /
  `trusted-extra` / **`unavailable`** (a backend whose tool is absent here, e.g.
  dconf on a Mac — degraded, not a failure); `manual` steps and image-baked items
  are status-only, never counted as drift.

  An `[[assert]]` may declare `severity = "notice"` with a `message`. A failing
  notice reports a **pending state, not a defect**: it prints as a `ⓘ` line
  carrying its message, stays out of the out-of-sync count, and is given no
  remediation. The motivating case is a staged ostree deployment — the machine
  matches the spec and an update is waiting for a reboot, so a red `✗` that no
  verb could ever clear was both wrong and unactionable, and a permanent red
  teaches people to stop reading the report. Which conditions are pending rather
  than wrong is a **data** decision (the manifest says so), exactly as `strip`
  declares which dconf keys are noise — the engine learns nothing about ostree.
- **`prune`** — remove installed-but-not-declared (dependency-aware, honoring the
  ignore/baseline list), uninstall user-scope GNOME extensions no bundle declares
  (the machine→spec answer to an `extension-extra`, which otherwise had none),
  and `brew untrust` any tap trusted on the machine but
  not in `[brew].trust` (the machine→spec mirror of `reconcile`'s trust absorb);
  previews and confirms first (`--yes` skips; under `--json` it previews unless
  `--yes`).
- **`init [name]`** — scaffold **this** machine into the folder: append a
  `[[machine]]` block (creating `temper.toml` if absent), wire `brewfiles/<name>`,
  then seed it via `reconcile --current-state-wins --include-trust`. The name is
  inferred from the hostname when omitted, matching how every other verb decides
  which machine it is. Refuses a machine that already exists (rewriting a
  hand-authored block would lose intent). Distinct from `setup`, which records
  *which folder* to use rather than putting a machine in one.
- **`snapshot-dconf [machine]`** (alias `snapshot`) — capture each declared `[[machine.dconf]]` subtree
  through its strip-keys filter into its file. Unlike a one-shot seed this is
  **recurring**: it's the spec←machine half of the capture/restore pair and the
  wholesale sibling of a per-key `reconcile`. Errors where dconf is absent
  rather than silently writing nothing.
- **`restore-dconf [machine]`** (alias `restore`) — load the machine's snapshot(s) back into live
  dconf (confirm-gated, `--yes` to skip, `--dry-run` to preview). Clobbers live
  desktop tweaks, so it is a standalone verb, **never** part of `update` (RIS
  excludes gnome-restore from its update for the same reason). Journaled per
  subtree, so `undo` reverts it.
GNOME extensions report **both** directions and answer both: an extension that is
declared but not installed is installed by `install` or dropped from the
machine's own list by `reconcile`; one installed but undeclared is removed by
`prune` or declared for this machine by `reconcile`. The extras side is
user-scope only — system extensions ship with the image, and image-baked items
are status-only. `[ignore].gnome_extensions` silences one.

Reconcile writes only `[[machine]].gnome_extensions`, never a bundle's shared list, and
computes nothing at all unless `gnome-extensions` answered: capability decides
whether a direction may be evaluated, not just whether a verb may run.

- **`adopt`** — report installed extras not in the spec (advisory / non-mutating)
  so you can add each to a bundle, the machine loose list, or `[ignore]`. The
  read-only sibling of `reconcile`.
- **`reconcile [machine]`** — the interactive spec←machine capture (RIS's
  `reconcile`): per-item, absorb installed-but-undeclared extras INTO the
  machine's own Brewfile, drop declared-but-absent entries FROM it, or route a
  flatpak extra to `[ignore]` (comment-preserving, via toml_edit). It also
  reconciles **tap-trust**: absorb a trusted-but-undeclared tap into
  `[brew].trust` (or `[ignore].tap`), or drop a declared-but-untrusted tap from
  it — the same both-direction diff, written to `temper.toml` via toml_edit.
  Missing entries default to *keep*, extras default to *skip*; a unified preview
  + one confirm precede any write. Edits only the machine's **own** `brewfile`
  (and the fleet `temper.toml` for `[brew].trust`/`[ignore]`), never a shared
  bundle. It also absorbs **desktop keys** per section (see machine scope above).
  **`--current-state-wins`** (`--csw`) answers every item with "the machine" and
  skips the prompts, keeping the preview + one confirm (`--yes` waives it). It
  is deliberately **machine-scope only**: `[brew].trust` and `[ignore]` are fleet
  config, so absorbing them from one machine would silently change the others —
  tap-trust drift is *reported* and left alone. **`--include-trust`** opts in the
  **adds** (taps this machine trusts that the fleet doesn't declare); it never
  removes one, because a declared-but-untrusted tap usually means the machine
  hasn't converged yet rather than that the fleet is wrong. `--json` previews the
  plan without prompting. Converging the other way,
  machine←spec, *is* `install`/`update`.
- **`undo [run]`** — revert a run — the one named by its id, else the newest;
  **`undo --list`** enumerates revertible runs (read-only). amdl's
  content-addressed journal: after-hash-guarded reverts skip-and-report (a file
  changed since, or a missing object) rather than clobber or abort mid-run.

---

## Delivered outside the temper *binary* (still replicated — not refused)

RIS-parity is the goal, so nothing RIS does is dropped. A few RIS jobs are
delivered by something other than a temper *verb* — because of a genuine
constraint, not a scope preference. RIS itself delivers these outside its `just`
recipes too (a `bootstrap.sh`, an image tier), so this is parity, not a gap.

- **Bootstrap** — getting brew + temper onto a bare machine runs *before* the tap
  (and thus temper) exists — the paradox. It stays a small companion shell script
  (`install-bazzite.sh`'s bootstrap tier + the `curl | sh` fallback), exactly as
  RIS bootstraps with `bootstrap.sh`. Phase-1 image work (cosign key,
  `policy.json` JSON-merge, signed `rpm-ostree rebase`, reboot) rides there too.
- **Image-side system layer** — the OS image bakes browsers, CLI baseline, brave
  policy, etc. Building the image is a different *artifact* (the Stacks repo), the
  same split RIS draws with `install-bazzite.sh`. temper *configures* a machine on
  top of that image; drift still reports image-baked items status-only.
- **`eq-import` — folder-authoring, and built.** The `eq-import` verb shallow-
  clones the configured `[eq_import].repo` and lands each `<x>.calibrated.conf`
  as `<dest>/<x>.conf`. It writes *into* the config folder (authoring) rather
  than converging a machine — the one clearly-labelled Principle-#9 exception —
  so it lives as its own verb, not a converge step. Was a working RIS recipe;
  now replicated, not carved out.

---

## Folder layout — building your own spec folder

temper *requires* only `temper.toml` at the root; everything else is convention.
The recommended shape (app-first recipes, real files under `assets/`):

```
<your-folder>/           a git repo, a synced cloud folder, or a USB copy
  temper.toml            machines (name/os/role) + apps + loose pkgs + [vars] + [ignore] + [brew] + [eq_import]
  apps/                  one file per app — the composable, code-free recipes
    shell.toml           copy/block/setkey/exec steps, os/role/when-gated
    ghostty.toml
    1password.toml       e.g. setkey keybinding + exec(sudo) NMH setup + a sysfile /etc write
  assets/                the real files the recipes deploy
    starship.toml  ghostty.config  gnome/shell.<machine>.dconf  …
  brewfiles/             optional per-machine Brewfiles (a machine's `brewfile = "brewfiles/<name>"`)
    <machine>
  secrets/               git-ignored; consumed by exec/setkey steps that declare them
```

Get the folder onto a box however you like, then let temper find it (§discovery:
drop it at a scanned location like `~/temper-home` or `~/Developer/temper-home`,
or run `temper setup <dir>`). See `SPEC.md` for the schema of each file, `WORKFLOWS.md`
for the day-to-day loops, and `PRINCIPLES.md` for the guardrails.
