# temper — Design Principles

> The guardrails that keep `temper` from degenerating into a worse Ansible / a
> private Nix. When a decision is unclear, resolve it in the direction these
> point. Refined after the 2026-07-27 sanity-check against ReinstallScripts.

## 1. Three sets, three rules for growing them

- **Primitives — closed.** `copy`, `block`, `setkey` (a backend family —
  dconf/defaults/ini/json/toml, *not* one per format), `profile`, `sysfile` (one
  root-owned `/etc` file), `exec`. Adding one is a big deal: new release, new
  drift/undo logic, new surface area. It grew during the sanity-check but stayed
  closed — each addition was a whole class the repo proved, not a one-app patch.
  If you're reaching for a new primitive to support one app, you want `exec`.
- **App-bundles — open, and free.** They're config. "The next ghostty clone" is a
  file you write, never a tool release.
- **Providers — open, behind an interface.** `brew`, `cask`, `brew-tap`,
  `brew-trust`, `flatpak`, `mas`, `vscode`, `gnome-extensions`, `rpm-ostree`,
  `dconf`. Adding one is neither a big deal nor free: it is **routine**, because
  it implements the eleven-column interface (#13) rather than inventing its own
  verb set. `apt`, `dnf`, `npm`, `cargo` should each be a normal piece of work.

The middle tier is what keeps a provider from being a bespoke decision. Listed
among the primitives, each one feels like a release-sized judgement call, and a
provider treated that way decides its own verb set — differently every time.

## 2. Steps stay declarative, idempotent, independently drift-checkable — with one named exception

Ordering within a bundle is fine; *data/effect flowing between steps* is the
smell. The one sanctioned violation is **the cask-artifact reset**: app config
that patches a cask-owned `.desktop` file needs a pristine reset before the next
brew converge, or the patch and the upgrade fight. It is **named as an
exception** — pretending the scopes are cleanly separated would be a lie the code
disproves — and it is currently resolved by hand or in one `exec`; there is no
schema annotation for it, and this document should not describe one until there
is. New couplings do **not** get this treatment; they get refactored or go in one
`exec`.

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

**Converge in one call per manager, not one per item.** Every provider's CLI
takes a list — `gext install UUID [UUID…]`, `mas install <id>…`,
`rpm-ostree install <pkg>…` — so a per-item loop pays N process spawns for
nothing, and for anything needing root it pays **N password prompts**, which is
what decides whether a converge can be walked away from. "This tool has no batch
mode" is a claim to check, not to assume; it was written in a comment about `mas`,
which has taken a list all along.

The forgiveness a loop buys is kept by *falling back* to one, not by starting
there: a failed batch says nothing about which item failed, so it is retried per
item, which contains the damage and names it (#6). The happy path costs one
invocation; the unhappy path costs one extra attempt and buys precise reporting.

That union — a machine's bundles plus its own list — is the scope model (#11) in
its earliest, package-shaped form. What is *not* redundant with #11 is the
dependency-closure half: `brew bundle cleanup` keeps a formula that is some other
package's transitive dependency, so drift cannot be computed by set subtraction.
That is a correctness argument about brew, not about scope, and it is why the
converge is one aggregate call per manager.

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

**A count is output, so a wrong count is a silent cap.** If a variant can be
acted on, it must be counted — an aggregate that omits one reports work it did
as work it didn't. `prune` once asked "remove **0** item(s)?", uninstalled three
GNOME extensions, and reported "0 item(s) removed", because the count summed two
of its three lists.

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

The converse matters as much: **nothing is *reported* without a resolution
story.** Items temper can't repair are still drift-*reported* as **status-only**,
but "status-only" is not a shrug — it carries the written reason from #11, naming
the file a human edits. A report with no stated way to act on it is the defect
this tool keeps shipping, and it hides behind exactly that phrase.

## 8. Every mutation is planned, reversible, and typed

Plan → apply → drift → undo is the contract **every** primitive implements.
`profile` is the acknowledged weak case, but on two of those four only: its apply
is a GUI dialog the user approves, and it isn't undoable. It plans and drifts like
the rest. Mutating runs are journaled
(amdl's content-addressed, after-hash-guarded model: a revert that finds the file
changed since skips-and-reports rather than clobbering).
`--json` on every command; an `--llm` guide; human → stdout, progress/errors →
stderr so pipes stay clean. Asserted, not merely intended: every read-only and
preview verb is run under `--json` and its **whole** stdout must parse as one
document — a second object is trailing garbage, which is how `init` broke, and a
stray line is not the first line, which is how a failing `exec` broke it.

**Advice is a mutation too.** A remediation temper names must be *executable for
the finding it names* — a verb with a real code path for that kind, on this host.
Naming one that silently does nothing is worse than naming none, because the user
runs it, sees no error, and concludes the tool is lying or they are. `drift` told
users to run `reconcile` for a missing GNOME extension for two releases, and
reconcile could not touch one.

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

## 11. Scope decides the verb set

Where a declaration lives decides what may be done to it — a lookup, not a
judgement. `ARCHITECTURE.md` carries the table; the principle is that the lookup
is **total**, and two things follow from it that have both been shipped wrong:

- **Every category needs both scopes.** "I want this on this box only" is
  ordinary. A category that exists at one scope is unfinished, and the
  compensation is always the same defect: a verb reaching up to edit a shared
  file on one machine's say-so.
- **A fleet declaration carries the same `os`/`role` gate a bundle does.** A list
  shared by every machine that cannot say which machines it describes is
  permanently red on the rest — and the verb offering to "fix" that breaks the
  ones it does describe.

The four cells fall out of this. A finding is *declared-but-absent* or
*installed-but-undeclared*, and each admits two resolutions — change the machine,
or change the spec — but the second column only exists at machine scope. A kind is
not done until all four are answered: with a verb, or with a written reason
**naming the file a human edits**. "Report-only" is an answer about scope, never
about effort.

## 12. Capability is per cell, and absence you could not observe is not evidence

Scope says what the *spec* permits; capability says what *this host* can do, and
it is not one bit. Three separate questions with three separate answers:

- **Can I observe it?** Without this, neither direction may be computed.
- **Can I converge it?** A host can enumerate GNOME extensions and be unable to
  install one; managed macOS preferences are readable and not writable.
- **Is there somewhere in the spec to write?** Orthogonal to both tools above.

One source of truth per feature, distinct answers per cell. A cell that cannot be
evaluated reports `unavailable` — **never absent** — and a drop is only ever
computed from state that was actually observed. Every write path reads "empty" as
"delete what the spec captured", so a read that could not happen must never reach
one as a fact.

**The verbs owe this as much as the report does, and it is easy to give it only
to the report.** `drift` distinguished the two and `prune` did not: on a machine
whose brew was installed and failing, `prune --json` emitted `"extras": []`,
byte-identical to a converged machine. Nothing was deleted wrongly — the empty
list was safe — but "nothing to remove" and "I could not look" are opposite
instructions to whoever reads it, and a removal verb is the worst place to
conflate them. Any verb that can act on an observation publishes what it could
not observe, in `--json` and on the terminal both.

**Residue is an observability question too.** When a declaration goes away, what
it deployed stays behind — and whether that needs machinery depends on exactly
this: *enumerable state needs no tombstone; non-enumerable state does.* Packages
self-clean, because temper can list what is installed and diff it against what is
declared. Files cannot, so removing a step leaves them on every machine forever
with nothing reporting it. Make the residue enumerable and `prune` answers it like
any package; where it cannot be, an explicit tombstone is the honest fallback —
reviewed, never expired, because behaviour that changes with the wall clock means
two machines on one commit doing different things.

## 13. One interface, named precisely

Every provider answers the **same eleven questions** (see ARCHITECTURE, "The
feature interface"), each with one written definition, so `prune` means one thing
to every implementer. A column may be declined — but only in writing, because the
awkwardness is the point: it is where you ask whether the thing simply has not
been built. This is what makes #1's third tier routine instead of bespoke, and it
is the enforcement mechanism for #11 and #12 rather than a separate idea.

Name a feature for **what it actually is**, at the narrowest honest level, because
the family always gets a second member and the desktop always gets a second store.
`rpm-ostree`, not `rpm` — a future `apt` is a different provider, not a variant,
and the generic name pre-empts a slot it does not deserve. `brew-trust`, not
`trust`. `gnome-extensions`, not `extensions` — VS Code extensions are managed
here too, so that collision is already live. `snapshot-dconf`, not
`snapshot-gnome` — the mechanism is the store, and dconf runs under KDE as well.
Renames ship with serde/verb aliases; the old name keeps parsing.

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
