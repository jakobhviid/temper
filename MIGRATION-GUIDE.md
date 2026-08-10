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
| `/org/gnome/shell/extensions/<uuid>/…` | the extension's own declaration — captured and replayed with it |
| a key you want **always set** (global shortcuts, 1Password) | a `setkey` step in a bundle |
| a key you want **always absent** | the absence primitive, not a captured value |
| genuinely machine-specific live state | a narrowly-rooted `[[machine.dconf]]`, still supported |

`strip` loses its ownership job entirely — not derived, *gone*, because nothing
else captures that keyspace any more. Keep only genuine noise (`monitors/`,
`last-selected`).

**The one that will surprise you.** `enabled-extensions` and
`disabled-extensions` stop being captured. Whether an extension is enabled is now
a field on the extension declaration, and those keys are computed from it. This
is deliberate: they were previously a *second, unlinked* fact about the same
extension, so a uuid enabled in a snapshot but declared nowhere was switched on
by `restore` and never installed by `install` — and GNOME fails soft, so the
breakage was silent.

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

The `[brew].trust` **table** is unchanged: `trust` is already namespaced by
`[brew]`, so it does not carry the collision the bare kind name did.

**Not aliased:** the `Finding.kind` values above. If you parse `temper drift
--json`, they are renamed fields rather than aliases — `.items[].kind`, and the
`remediation` list beside it. The `dconf-*` kinds are **unchanged**; they were
already named for the store.
