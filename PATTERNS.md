# PATTERNS.md — composing primitives for common problem shapes

SPEC documents each primitive in isolation; this is the **assembly** layer: given
a problem *shape*, which combination of `[[step]]` primitives solves it the temper
way. For exact fields see SPEC; for the verbs you run see WORKFLOWS.

**The heuristic:** reach for the *most declarative* primitive that fits. Prefer
`copy` / `block` / `setkey` (journaled, drift-checked, undoable) over `exec` (the
escape hatch — not journaled, not undoable, drift only via a `check`). Gate steps
on **reality** (`when` probes), not on assumptions about the machine.

## Problem shapes

- **Own a whole file you control** → `copy`. Add `template = true` if it needs
  apply-time values; `seed = true` for create-once-then-hands-off.
- **Own one key in a structured file another tool also writes** (JSON/JSONC/TOML)
  → `setkey(json|toml)`. It preserves the siblings — and, for JSONC, the comments
  — you don't own. A deep dotted key (`a.b.c`) creates the intervening objects.
  Own *only* keys the other writer never touches: don't `setkey` a key it (or the
  user, via a TUI) actively drives — a default the app rewrites — or you revert
  their choice every converge.
- **Own a marked region inside a user-owned text file** (`~/.ssh/config`, a
  shell rc) → `block`. The markers are yours; the rest of the file stays the
  user's.
- **A value that must resolve on the target box** — a binary's absolute path, the
  brew prefix, an env-derived path (differs mac↔linux; GNOME's PATH excludes the
  brew prefix) → `template = true` on `copy` or `setkey`, with `{{ which "<bin>" }}`
  / `{{ brew_prefix }}` / `{{ env "X" }}` / `{{ var "X" }}`. Pair with a
  `when = { binary = "<bin>" }` gate when the target may be absent — an
  unresolvable template errors.
- **Register your one entry in a shared list** — a keybinding path, a PATH-like
  array → `setkey append = true` (`json`/`toml`, or `dconf` for a GVariant `as`
  list). Idempotent union; other members survive. Use it only where temper is the
  member's *sole registrar*: if a dconf **snapshot** (backup/restore) captures the
  whole array — curated whole-desktop state like `enabled-extensions` that mixes
  image-baked, userspace, and the user's own toggles — the snapshot owns it, not
  append.
- **Write a root-owned system file** (`/etc/…`, a specific mode/owner) →
  `sysfile`. Escalates internally for just that write; drift compares content +
  mode + owner.
- **Apply a step only where its app is actually present** → a `when` probe
  (`binary` / `path` / `brew` / `cask` / `flatpak` / `mas` / `gext` / `rpm`).
  Deploy an app's config only where the app is installed, however it got there —
  gate on the probe, not the machine's `role`/`os`.
- **Genuinely imperative — no primitive fits** (multi-step surgery, a vendor
  tool's own wiring) → `exec`, the escape hatch. Give it a **read-only** `check`
  so it's idempotent and drift-aware — drift *runs* the check, so a mutating one
  breaks the read-only contract (a `grep`/`test` is ideal). It is *not* journaled/
  undoable, so keep it to the irreducible part — pull anything a primitive can own
  back out into `setkey`/`copy`/`block`.
- **Retire a file/config across the fleet** (there is no delete primitive) →
  `[[assert]] absent = "<path>"` as a permanent drift guard, plus a one-time
  `exec` that removes the file and any include line it left in a user-owned
  (`seed`) file temper won't otherwise rewrite — with a dated note to drop the
  `exec` once every machine has converged.

## Anti-patterns (reach for the primitive instead)

- **Two owners for one key/array** — `setkey`/`append` a key or array a plugin,
  the user's TUI, or a dconf snapshot already drives → a revert war every
  converge. One owner per key; leave the rest.
- **`exec` running `gsettings get`/`set` to union a list** → `setkey(dconf)
  append = true`.
- **`exec` computing a path with `$(command -v …)` then writing it** → a
  `template = true` value, `{{ which "…" }}`.
- **`copy` onto a file a plugin/tool co-writes** → `setkey(json)`; `copy`
  overwrites the tool's keys and drops its comments.
- **An `exec` with no `check`** → it re-runs every update and can't drift-check.
  Add a `check`, or use a real primitive.
- **Gating on `role`/`os` when you mean "if the app is installed"** → use a
  `when` presence probe. `role`/`os` are for machine *shape*, not app presence.

See SPEC for the exact grammar of every field named here.
