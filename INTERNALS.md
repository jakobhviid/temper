# INTERNALS — how temper is built

`ARCHITECTURE.md` describes the model a **folder author** works against. This file
describes the **tool's own construction**: how it speaks, and what it would take
to extend it. Read it before changing temper; you never need it to write a spec.

Deliberately **not** in `temper --llm`. That guide teaches operating and authoring
a folder, and a reader learning to write `apps/ghostty.toml` does not need to know
how columns are measured.

---

## How temper speaks — capture, voice, and the converge display

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

## Adding a second settings backend

### Constraints

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

#### Two decisions recorded, so they are not re-proposed

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
