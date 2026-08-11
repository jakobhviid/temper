# temper — Migration Guide

> How to move a temper folder across a **major** version, where the schema or the
> model changed and the folder itself has to be edited.
>
> This is the one document whose job **is** the before/after. Everywhere else,
> "documenting the diff" is the failure mode AGENTS.md warns about; here the
> transition is the content. Entries are kept until the fleet has rolled past
> them, then deleted — an entry nobody can still be migrating *from* is trivia.

## How you find out you need this

temper stamps the version that last wrote a folder (`temper_version` in
`temper.toml`). A binary older than the stamp reports a skew rather than a raw
parse error, and `[update].mode` decides whether it also offers the upgrade.
Since every schema struct is `deny_unknown_fields`, a new key is a hard parse
error on an older binary — which is why upgrading temper comes **before**
editing the folder, on every machine.

```sh
temper --version                     # what you are running
grep temper_version temper.toml      # what last wrote the folder
```

## Order of operations

Fleet-wide, and in this order. Doing it out of order leaves machines that can
neither read the folder nor tell you why.

1. **Upgrade the binary everywhere first.** A new temper reads an old folder; an
   old temper does not read a new one.
2. **Schema keys** — rename or move declarations. Renames ship with serde
   aliases, so the old key keeps parsing: this step is *not* urgent and can be
   done incrementally.
3. **Verb names** — update scripts and CI. Old names stay as aliases.
4. **`Finding.kind` consumers** — anything parsing `temper drift --json`. These
   are **not** aliased; a renamed kind is a renamed field.
5. **Verify**, per machine:

```sh
temper drift --json > /tmp/before.json    # BEFORE editing the folder
# … make the edits …
temper drift --json > /tmp/after.json
diff <(jq -S '[.items[].kind]|sort|unique' /tmp/before.json) \
     <(jq -S '[.items[].kind]|sort|unique' /tmp/after.json)
```

A migration is done when the *kinds* reported are the ones you expect and the
out-of-sync count has not grown. A migration that silently reduces findings to
zero has usually deleted the checks, not satisfied them.

## What temper will not do for you

temper does not rewrite your folder. Principle #9: it operates on *a folder with
a manifest*, however that folder arrived, and it is not a manager of it. So a
migration is advice plus a drift check that proves you followed it — never an
automatic edit. The exceptions are the verbs that already author the folder by
design (`init`, `reconcile`, `snapshot-*`, `eq-import`), and they only ever write
what you accepted at a prompt.

---

## 4.0 — scope becomes the rule, and dconf splits by owner

The largest change since the folder format settled. Three things move.

### 4.0.1 Machine-scope counterparts for fleet-only lists

**Why.** Scope decides which verbs apply (see ARCHITECTURE, "Scope decides the
verb set"): a fleet declaration is drift-and-install, a machine declaration is
drift, install, prune *and* reconcile. Three categories existed only at fleet
scope, so "I want this on this box only" had nowhere to go, and `reconcile`
compensated by editing fleet files from a single machine — which changes every
other machine silently.

**Edit.** Nothing is required. The fleet keys keep working and keep meaning
"every machine in the group". Move an entry down to machine scope only where you
actually want it per-machine:

```toml
# before — fleet scope: every machine, and reconcile must never edit it
[brew]
trust = ["user/tap"]

[ignore]
flatpak = ["org.example.Preinstalled"]

# after — the same, plus what belongs to one box
[[machine]]
name = "atlas"
brew_trust = ["user/personal-tap"]        # this machine only
[machine.ignore]
flatpak = ["org.example.JustOnAtlas"]
```

**Behaviour change to expect.** `reconcile` no longer offers to drop a
*fleet*-declared tap or write a fleet `[ignore]` entry. That was never a
per-machine decision; removing one is a spec edit, and then every machine's
`prune` enacts it.

### 4.0.2 dconf snapshots split by owner

**Why.** Measured on a real fleet, **95% of the keys in a whole-desktop snapshot
were extension settings**. The remaining ~5% were either fixed policy (global
shortcuts) or genuinely machine-specific. One blob carrying all three meant the
snapshot became a second owner of keys a bundle already declared — kept apart by
a hand-maintained `strip` list that silently rotted whenever you forgot an entry.

**Edit.** Split each `[[machine.dconf]]` by who owns the keys:

| what it is | where it goes now |
|---|---|
| an extension's own subtree under `/org/gnome/shell/extensions/` | `settings = "…"` on the extension: `{ uuid = "x@y", settings = "assets/gnome/ext/x.dconf" }`. temper reads *which* subtree from the extension's gschema; the uuid is not it. Give each machine its own file unless you mean the two boxes to hold identical values — and if you do, declare the extension in a shared bundle instead, which is what group scope is for. |
| a key you want **always set** (global shortcuts, 1Password) | a `setkey` step in a bundle |
| a key you want **always absent** | the absence primitive, not a captured value |
| genuinely machine-specific live state | a narrowly-rooted `[[machine.dconf]]`, still supported |

**`strip` keeps only part of its ownership job.** Any dconf key a `setkey` step
declares is excluded from capture and from drift automatically, and a capture
reports how many it left out. A `strip` entry naming exactly that key is
therefore redundant, and deleting it changes nothing.

**A `strip` entry naming a whole subtree is a different thing.** The derivation
covers the keys a `setkey` declares; a subtree entry covers their unowned
siblings too, and deleting it surfaces every one of them as `dconf-extra`.
Cutting two subtree entries on the fleet this was developed against added four
findings — three `dconf-extra` under `blur-my-shell/applications/` and one
changed key under `quick-settings-audio-panel/` — all of them keys that bundle
deliberately leaves unowned. That report is not wrong: an unowned key does not
survive a rebuild. But it is a decision about what you want to own, so make it
deliberately rather than as part of a rename.

```toml
# `blur-my-shell/applications/blur` is declared by a setkey → the derivation
# handles it, and a strip entry for that exact key can go.
# `blur-my-shell/applications/` is the SUBTREE — it also hides `enable-all`,
# `pipeline` and `blur-on-overview`, which nothing declares.
strip = ["monitors/", "last-selected", "blur-my-shell/applications/"]
```

So this is a decision rather than a cleanup, and the honest options are: keep the
subtree entry (it is suppressing keys you have chosen not to own), declare the
siblings with a `setkey` and then drop it, or drop it and let them land in the
snapshot — which makes the snapshot a second owner inside a subtree a bundle
also writes, the very thing the split removes.

The reason to reach for the first two is that an unowned key does not survive a
rebuild: nothing declares it and nothing captured it, so a fresh machine simply
will not have it.

**The one that will surprise you: extensions you keep installed but switched
off.** Whether an extension is enabled is now part of its declaration, and a bare
uuid means *enabled*. So an extension you deliberately disabled will report
`gnome-extension-enable` until you say so:

```toml
# before — the uuid says "I want this", the machine says "switched off",
#          and nothing related the two
gnome_extensions = ["CoverflowAltTab@palatis.blogspot.com"]

# after — the declaration carries the answer
gnome_extensions = [{ uuid = "CoverflowAltTab@palatis.blogspot.com", enabled = false }]
```

Expect one finding per extension in that state; on the fleet this was developed
against there were two, both previously invisible. temper only ever asserts its
own declarations — it enables and disables by uuid and never rewrites
`enabled-extensions`, so image-baked extensions it does not declare are left
exactly alone.

### 4.0.2b Two gates that used to fail open

**Why.** A bundle's `os`/`role` gate covered two of the five ways a bundle
carries machine-specific content, and the three it missed failed **silently and
green**.

**Edit.** Check two things, both of which are no-ops for most folders:

1. **A machine that declares no `role` no longer composes a bundle that gates on
   one.** Previously the gate fired only when *both* sides named a role, so a
   role-less machine layered every `role = "desktop"` bundle's extensions and
   rpms — the opposite of what the gate was for. Give every machine a `role`, or
   drop the `role` from bundles you want everywhere.

   ```sh
   # machines with no role — these change behaviour
   grep -A6 '^\[\[machine\]\]' temper.toml | grep -B4 -L '^role'
   ```

2. **A bundle's `packages` are now gated like its `extensions` and `rpm_ostree`.**
   An `os = "linux"` bundle no longer contributes packages to a Mac that composes
   it. If you relied on that — a shared bundle whose `packages` were meant for
   everyone but which declared an `os` for its *steps* — split the steps out, or
   drop the bundle-level `os`.

**Verify** with the recipe above: the kinds reported should be unchanged and the
out-of-sync count should not grow. On the fleet this was developed against, both
were identical before and after.

### 4.0.3 Names get specific

Every rename ships with a serde alias or a verb alias; the old name keeps
working. Update at your leisure, except the `--json` kinds.

| was | is | why |
|---|---|---|
| schema field `rpm` | `rpm_ostree` | it is ostree layering specifically — a future `dnf`/`apt` is a different type, not a variant |
| schema field `extensions` | `gnome_extensions` | collided with VS Code extensions, which temper also manages |
| `[ignore].gext` | `[ignore].gnome_extensions` | same collision, and `gext` named a tool the probe never runs |
| verb `snapshot-gnome` / `restore-gnome` | `snapshot-dconf` / `restore-dconf` | the mechanism is the store, not the desktop; dconf is present under KDE too |
| kind `rpm` | `rpm-ostree` | as above |
| kind `trust` / `trust-extra` | `brew-trust` / `brew-trust-extra` | flatpak remotes and apt keys are also "trust" |
| kind `extension` / `extension-extra` | `gnome-extension` / `gnome-extension-extra` | as above |
| kind `package` / `package-extra` | `brew-package` / `flatpak-package` / `mas-package` / `vscode-package` (+ `-extra`) | one kind for three providers meant none of them could be answered for on its own |

The `[brew].trust` **table** is unchanged: `trust` is already namespaced by
`[brew]`, so it does not carry the collision the bare kind name did.

The package split follows the same rule as the rest of #13: `brew`, `cask` and
`brew-tap` converge through one `brew bundle` and are **one** provider, so they
share `brew-package`. `flatpak` and `mas` are their own. `vscode` gets a kind
without being a managed provider — Settings Sync stays the sole registrar of
those extensions, and naming the kind makes the ownership temper declines
explicit rather than implicit.

> **Upgrade the binary before you rename the field, not after.** A temper from
> before this release appends the old `extensions` key when `reconcile` absorbs
> an extension. On a folder already renamed to `gnome_extensions` that is a
> second key beside the first, and serde rejects the pair as a **duplicate
> field** — the manifest stops parsing on every machine, and `[git].auto_commit`
> commits it. This is why step 1 of the order of operations is not optional.

**Not aliased:** the `Finding.kind` values above. If you parse `temper drift
--json`, they are renamed fields rather than aliases — `.items[].kind`, and the
`remediation` list beside it. The `dconf-*` kinds are **unchanged**; they were
already named for the store.

### 4.0.4 Behaviour you will notice, with nothing to edit

These need no change to your folder. They are listed because each one alters
what a verb does on a folder that is already correct, and finding that out from
a diff is worse than reading it here.

**`prune` is narrower in three places, and wider in one.**

- A spec that declares **no tap at any scope** no longer untrusts anything. Tap
  trust had no opt-in, unlike every other category, so `prune` on a folder that
  simply never mentioned taps ran `brew untrust` on all of them — including the
  ones its own formulae come from. Declare one tap and the extras direction
  works exactly as before.
- `[ignore]` now protects a package from removal, not just from the report.
  `brew bundle cleanup` decides for itself what to remove, and temper had been
  handing it a file that never mentioned the ignored ones — so they were
  uninstalled, outside the preview and outside the confirm.
- A retired path that is a **directory** is removed. It used to fail and be
  reported as removed anyway.
- The count is what happened. A removal that fails is listed as still present
  rather than counted as done, and `--json` gains a `failed` array.

**`prune --json` gains two keys**: `flatpak_remotes` and `retired`. Both were
already being *removed*; neither appeared in the document or the preview.

**`undo --dry-run` no longer uninstalls packages.** It did — every provider's
real `uninstall`, then "(dry-run)". If you have been avoiding it, stop.

**`undo` lands on the run you meant.** A converge whose changes were all
unrevertible (an `exec`, a `sysfile`) recorded no run at all, so a later bare
`undo` reached past it and reverted the previous one.

**`temper init` seeds what is installed.** It was capturing taps and nothing
else, because it reconciles the block it has just written and that block declares
nothing. `--json` also emits one document instead of two.

**One new `Finding.kind`: `package-unavailable`.** A package manager that is
present and **fails** — `mas list` when you are not signed into the App Store, a
`brew list` broken by a bad tap — is now reported as unreadable and skipped in
both directions, instead of reading as "nothing is installed". That last reading
is what made `reconcile --current-state-wins` capable of emptying a Brewfile and,
with `auto_push`, sending it to the fleet. Status-only, so it does not count as
out-of-sync; if you assert on the count, nothing changes.

**A `copy` step's `mode` is drift-checked.** It was enforced on every converge
and compared by nothing, so a file whose permissions had been widened reported
*in sync* while temper silently chmod'd it back and said it changed nothing. If a
declared mode does not match on your machines, this is the release where drift
starts saying so — and `install` fixes it and counts it.

**rpm-ostree reads one deployment, not all of them.** A rollback keeps the
`requested-packages` it was built with, so an un-layered package used to be
reported as layered forever and `prune` claimed to remove it on every run.

**Flatpak remotes are read from both installations** and written to the user
one. A remote your image provides system-wide now satisfies a declaration
instead of reading as permanently missing, and no duplicate user-scope copy is
added. A declared remote whose url has changed is re-pointed rather than
reported forever.

> **What is still open.** `install` writes flatpaks to the **system**
> installation (flatpak's default) while `prune` and `undo` act on the **user**
> one, so on an image-based host an undo of a flatpak install finds nothing.
> ROADMAP, "Which flatpak installation temper owns", has the evidence and why
> neither obvious fix is right. It is stated rather than silently half-fixed.
