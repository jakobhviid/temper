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
  member's *sole registrar*: if a dconf **snapshot** (snapshot/restore) captures the
  whole array — curated whole-desktop state like `enabled-extensions` that mixes
  image-baked, userspace, and the user's own toggles — the snapshot owns it, not
  append.
- **Write a root-owned system file you author** (`/etc/…`, a specific
  mode/owner) → `sysfile`. temper runs one `sudo install` for just that write;
  drift compares content + mode + owner, and it is idempotent — an in-sync file
  means no escalation attempted at all.

  **Only where temper owns the whole file.** `sysfile` writes your asset over the
  destination, so it is right when *you* author the content and wrong when a
  package also writes it: temper would revert the vendor's additions on every
  converge, and any ad-hoc edit you made by hand. When a vendor ships a
  **drop-in directory** (`sudoers.d`, `sysctl.d`, `*.conf.d`), point `sysfile` at
  a file in there instead — then temper owns a path nobody else touches and you
  get the full drift story with nothing contested. For a co-authored file with no
  drop-in, keep the additive write in an `exec` and give it a *subset* `check`
  (see below).

  **It also collapses password prompts, which no `exec` can.** temper is the
  parent process of every `sudo install` it runs, so *N* `sysfile` steps cost
  **one** authentication — even on a machine that keys sudo's timestamp to the
  parent process (some Fedora builds do, whatever `man 5 sudoers` claims). An
  `exec` that escalates internally is parented by its own shell, so each such
  script costs its own prompt and no amount of asking earlier changes that. Moving
  root work from `exec` into `sysfile` is therefore the one reliable way to make a
  fleet bring-up ask once rather than once per script.

  **Caveat:** `sysfile` is *not* journaled — `undo` skips it, like `exec` and
  `setkey(defaults)`. Don't move something you rely on reverting into it.
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

  For an **additive** write into a file others also own, make the `check` a
  *subset* test: assert your own entries are present (and the mode/owner if they
  are load-bearing), and say nothing about the rest. Then a vendor's additions and
  your own experiments neither trip drift nor get reverted — which is precisely the
  case `sysfile` cannot serve. If the script's expected values would otherwise be
  duplicated between it and the check, put them in a small data file both read;
  one source of truth, and adding a value stays a one-line change.
- **Retire a file/config across the fleet** (there is no delete primitive) →
  `[[assert]] absent = "<path>"` as a permanent drift guard, plus a one-time
  `exec` that removes the file and any include line it left in a user-owned
  (`seed`) file temper won't otherwise rewrite — with a dated note to drop the
  `exec` once every machine has converged. Give that `exec` a `check` that reports
  its own precondition (the file present, the include line still there): the step
  then goes silent once a machine is clean — an `exec` otherwise counts as
  "changed" on every converge forever — and `drift` becomes the answer to "is the
  fleet clean enough to delete this yet?", which is the question the dated note
  turns on.

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
- **`sysfile` onto an `/etc` file its package also writes** → same revert war, as
  root. Prefer a drop-in file temper owns outright; failing that, an additive
  `exec` with a subset `check`. Ask "who authors this content?" — if the answer
  includes anyone but you, temper should not own the whole file.
- **An `exec` with no `check`** → it re-runs every update and can't drift-check.
  Add a `check`, or use a real primitive.
- **Gating on `role`/`os` when you mean "if the app is installed"** → use a
  `when` presence probe. `role`/`os` are for machine *shape*, not app presence.

See SPEC for the exact grammar of every field named here.
