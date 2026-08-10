# temper — Design Principles

> The guardrails that keep `temper` from degenerating into a worse Ansible / a
> private Nix. When a decision is unclear, resolve it in the direction these
> point. Refined after the 2026-07-27 sanity-check against ReinstallScripts.

## 1. Closed primitive set; open app library

Adding a **primitive** is a big deal — new release, new drift/undo logic, new
surface area. Adding an **app-bundle** is free (it's config). The set is
deliberately small: `copy`, `block`, `setkey` (a backend family — dconf/defaults/
ini/json/toml — *not* one per format), `brew`/`flatpak`/`mas`/`gext`/`rpm-ostree`
(converge providers), `profile`, `sysfile` (one root-owned `/etc` file), `exec`. It grew during sanity-check but stayed
*closed*: each addition was a whole class the repo proved, not a one-app patch.
If you're reaching for a new primitive to support one app, you want `exec`.

## 2. Steps stay declarative, idempotent, independently drift-checkable — with one named exception

Ordering within a bundle is fine; *data/effect flowing between steps* is the
smell. The one sanctioned violation is **the cask-artifact reset**: app config
that patches a cask-owned `.desktop` file forces a pristine reset before the next
brew converge. It is handled by an explicit `reset-before-converge` annotation on
the cask, **named as an exception** — because pretending the scopes are cleanly
separated would be a lie the code disproves. New couplings do **not** get this
treatment; they get refactored or go in one `exec`.

## 3. `exec` is the pressure valve — with a defined contract

Every time the declarative model strains, the honest move is "write a script,"
not "extend the schema." But `exec` is no longer a black box: a step declares its
**privilege** (`sudo`), its **secrets/env**, and an optional **drift-hook**
(`check` script). Irreducible glue (1Password NMH surgery, speaker-eq,
chsh, tmux-plugin clone) lives here. The `setkey` family deliberately absorbed
what *used* to be exec (macOS `defaults`, `.desktop` key overrides, json/toml
merges) so `exec`'s volume stays honest.

## 4. Every package manager is whole-machine

Package drift is a **dependency-closure** computation, not set subtraction.
Packages **compose at declaration time** (union of a machine's apps **+ its loose
list**) but **converge at run time as one aggregate call** per manager. `[ignore]`
is **not** subtracted from that set — a declared package is always installed; the
list only stops installed-but-undeclared packages (the OS-preinstalled flatpak
baseline) from being flagged as extras by `drift`/`prune`, which is how the
"everything is traceable" rule survives contact with a real OS baseline.

## 5. Gate config on reality, not intent

Config steps run only when a **presence probe** passes — checking what's
*actually on the machine*, not what temper intended to install. This is what makes
image-baked (Linux Ghostty), hand-installed, and opted-out apps all behave under
one rule.

## 6. No silent skips, no silent caps

A gate that silently doesn't fire is a trap. Every skip is announced — naming the
step, not just its failed probe. Forgiving providers (MAS) report failures loudly
and continue. `brew` tap-trust runs before converge so third-party taps are never
*silently* skipped. A discarded exit code is a silent cap too: a best-effort child
may fail without stopping the run, but never without saying so.

## 6b. temper's output is temper's own; it reports effects, in temper's voice

Every tool temper shells out to has its own idea of the world, and each one is
happy to announce it: `flatpak update` prints "Nothing to update." about *its
remotes*, `git pull` "Already up to date." about *the folder's upstream*. On the
terminal mid-run, any of those reads as temper's verdict on the whole converge —
and on stdout it corrupts `--json`. So a child's output is captured and only
temper speaks: a live region while working, a `✓` only for what actually changed,
warnings and errors always, one summary at the end. Silence means converged.

What temper says is the **effect**, never the invocation: how many packages
changed version, how many commits landed, whether the remote actually moved — and
it learns that by comparing versions, refs and hashes, never by parsing a tool's
prose, which is localized and would work only in the author's language. Streaming
a child is right in exactly one case: when its output *is* the operation the user
asked for (`prune`'s removals, `brew upgrade temper` during a self-update).

## 7. Nothing is enforced without a drift story

If temper applies it, `drift` can check it — including things pushed to `exec`
(via a `check` hook) and things that aren't files or keys (via `[[assert]]`:
absent, mode, contains-line, not-member, executable-resolves,
json-semantic, shell). Enforcement that re-runs every `update` (git identity,
default shell) uses `run = always` + a drift hook so it stays checkable.
Items temper can't repair are still drift-*reported* as **status-only**. This
principle is what the sanity-check added — the first draft silently dropped a
third of `just drift`'s value.

## 8. Every mutation is planned, reversible, and typed

Plan → apply → drift → undo is the contract **every** primitive implements.
`profile` is the acknowledged weak case, but on two of those four only: its apply
is a GUI dialog the user approves, and it isn't undoable. It plans and drifts like
the rest. Mutating runs are journaled
(amdl's content-addressed, after-hash-guarded model: a revert that finds the file
changed since skips-and-reports rather than clobbering).
`--json` on every command; an `--llm` guide; human → stdout, progress/errors →
stderr so pipes stay clean.

## 9. The folder is human-readable; the tool doesn't manage it

Real files, browsable tree, "as readable as a Brewfile." `temper` does not manage
its config folder with `git` or any sync client — it operates on *a folder with a
manifest*, however that folder arrived. (An `exec` step may still shell out to
`git`/`curl` for a specific job — that's work, not folder management.)

## 10. Replicate all of ReinstallScripts; know the two things that are genuinely elsewhere

ReinstallScripts is the proven acceptance spec — **every RIS recipe gets a temper
equivalent**, and "temper does it differently on purpose" is only legitimate once
the difference is proven at least as good on a real machine. Exactly two RIS jobs
live *outside* the temper binary, for a real reason, not a scope preference — and
both are still delivered:

- **Bootstrap** — runs *before* the tap (and temper) exists (the paradox), so it
  stays a companion script, exactly as RIS uses `bootstrap.sh`.
- **Building the host image** — a different *artifact*, being spun out to its own
  repo (Stacks). temper *configures* a machine on top of that image; it never
  builds one. A *live* layering that is neither image nor bootstrap (`rpm-ostree`
  of proton-vpn) *is* in scope, as a converge provider that emits a reboot signal.

Everything else RIS does is temper's job — including `eq-import`, now built as
its own verb (it writes *into* the folder — authoring, the one labelled #9
exception). Scope discipline means not growing *past* RIS, never dropping a
proven RIS recipe.

---

### Prior art we are deliberately lighter than

- **Ansible** — bundles ≈ playbooks, primitives ≈ modules. Lighter: no inter-step
  data flow (bar the one named exception), no control flow in the manifest.
- **chezmoi** — templated deploys + run-scripts. We add package convergence +
  a real drift subsystem + machine identity; not a general dotfile manager.
- **Nix / home-manager** — the fully-declarative extreme. We keep an `exec`
  escape hatch and real, editable files.
- **dotsync** (sibling) — continuous cloud-folder dotfile *sync*. Different
  lifecycle; `temper` composes alongside it and reuses its `adopt` verb, does not
  absorb it.
