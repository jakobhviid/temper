# temper — scratch record, safe to delete

> **Working document. DO NOT COMMIT. Nothing here is load-bearing.**
>
> Everything that must outlive the redesign has been moved into the real docs —
> see the ledger below for where each piece went. What remains is the *audit
> trail*: what the code looked like before, how the conclusions were reached, and
> which of my own decisions got overturned on the way. That is worth reading once
> while the work is in flight and worth nothing afterwards.
>
> **Delete this file when the redesign lands.** If you find yourself wanting to
> keep a paragraph, that paragraph is in the wrong place — move it first.

## Where the durable content went

| what | now lives in |
|---|---|
| Four cells per kind; capability is not one bit; a drop needs observed state | `PRINCIPLES.md` #11 |
| Absorbing writes machine scope; fleet is opt-in, gated, reported | `PRINCIPLES.md` #12 |
| Name the mechanism, not the family; define the interface once | `PRINCIPLES.md` #13 |
| A wrong count is a silent cap | `PRINCIPLES.md` #6 |
| Nothing is *reported* without a resolution story | `PRINCIPLES.md` #7 |
| Advice is a mutation too | `PRINCIPLES.md` #8 |
| `reset-before-converge` names a mechanism that does not exist | `PRINCIPLES.md` #2 (corrected) |
| Scope decides the verb set; prune is the universal enactment mechanism | `ARCHITECTURE.md` |
| The eleven-column feature interface + per-column definitions | `ARCHITECTURE.md` |
| The filled feature matrix (every ⚠ and ❌) | `ARCHITECTURE.md` |
| Retirement: enumerable vs non-enumerable residue; ledger + tombstone | `ARCHITECTURE.md` |
| dconf has three owners; `enabled` becomes a declaration field | `ARCHITECTURE.md` |
| Constraints on a second settings backend; the two rejected decisions | `ARCHITECTURE.md` |
| Where a declaration lives decides what can happen to it | `SPEC.md` |
| The matrix questions (incl. observe, literal kinds, revertibility) | `AGENTS.md` |
| Every open gap, ranked, with the sequencing note | `ROADMAP.md` |
| How to move a folder across a major | `MIGRATION-GUIDE.md` |

If something below is *not* in that table and you think it matters, it has not
been written down yet. Fix that before deleting this file.

---

## The bug that started it

A user removed two GNOME extensions on purpose. `drift` reported them missing and
printed `→ temper reconcile — interactively add extras / drop missing entries`.
They ran it; it did nothing; every `install` put the extensions back. The only way
out was a hand edit.

`plan.rs` put `extension` into the `missing_pkg` bucket, and that bucket emitted
the reconcile line — while `reconcile` had no extension-drop code path at all.

The interesting part was *why* it shipped: commit `4fcb962` ("give a machine its
own extensions list, **and make matrix gaps a test failure**") created both the
gap and the discipline meant to prevent it, and the test suite written for
exactly this defect class passed straight over it. The registry it added was a
flat bag of verbs per kind — and **a bag cannot be incomplete, because it has no
shape to violate.**

## How the conclusions were reached

Four passes, each of which changed the answer:

1. **Matrix audit** — found the missing cell, plus the registry/remediations split
   and its untested reverse direction.
2. **Portability audit** (mac + GNOME/KDE/COSMIC) — found a live spec-deletion
   path that the matrix lens could not see, because it was a *capability* defect,
   not a completeness one.
3. **Linux and macOS reviews** — overturned three of my decisions (below).
4. **The scope conversation** — produced the model that actually explains every
   defect found in passes 1–3, and made most of my earlier framing redundant.

The lesson worth keeping from that sequence is in the docs already: the recurring
defect was never bad code, it was **features deciding their own verb set** because
no rule said what the verb set was.

## Decisions of mine that were overturned

Recorded because being wrong in a specific way is more useful than the conclusion
alone.

- **I guarded the wrong verb.** I proposed guarding the drift-side comparison and
  refusing `--csw` drops. `snapshot-gnome` was the shorter path to the same
  catastrophe and I had not mentioned it: it wrote the snapshot unconditionally,
  no preview, no confirm, then committed. And it never called the function I was
  guarding. → The guard belongs where the store is *read*.
- **My "empty live dump" heuristic was wrong in both directions.** False positive
  on precisely the workflow that started the document; false negative on partial
  loss; and the premise was wrong (uninstalling an extension leaves its dconf
  values behind). It also contradicted a principle stated three sections earlier
  in my own draft. → Test whether the store is *readable*, not whether its output
  is empty.
- **I reasoned from the wrong precedent on `--csw`.** I refused machine-scope
  drops by analogy with tap-trust — but trust drops are refused because trust is
  *fleet* scope. Machine scope is exactly what `--csw` exists to absorb.
- **I asserted casks were mac-only**, relaying a subagent without checking. The
  Linux Brewfiles in the fleet declare 15, 15 and 72.
- **I claimed `defaults` cannot enumerate.** It can (`defaults domains`, `read`,
  `export`). The real constraint is subtler and is now in ARCHITECTURE.
- **I proposed a `desktop` axis.** Both platform reviews rejected it
  independently; the rejection is now recorded in ARCHITECTURE so it is not
  re-proposed.
- **I proposed an "extension also owns these paths" override.** Better answer:
  an extension owns only its own subtree, and anything outside is explicit policy.

## Investigations that closed clean

- **Does `brew bundle cleanup` remove mas/vscode extras?** Yes. With no type
  flags every cleanup-supported extension runs, and each guards with "declare none
  of a type, clean none of it" — the same probe invariant temper states
  independently. Also confirmed no data-loss path: brew ships a flatpak cleanup
  extension that runs by default, but zero declared flatpaks removes zero
  flatpaks. The residual finding (temper inheriting brew's env-dependent
  defaults) is on the roadmap.
- **Is the scope model documented anywhere?** No — scattered mentions, no rule.
  That was the root cause of the whole thread and is now the first thing in
  ARCHITECTURE.
- **Is 95% of a snapshot really extension settings?** Measured: 262/274 and
  267/280 keys across two machines; the non-extension remainder was five sections.

## What was implemented in this pass

Green on `cargo clippy --workspace --all-targets -- -D warnings` and
`GIT_CONFIG_GLOBAL=/dev/null cargo test --workspace`. Never pushed.

The reported bug's missing cell; the registry typed by cell with
`remediations()` derived from it; the capability split with three-valued
observation; the shared read seam for dconf; and the verified defects that were
unambiguously defects (prune's count and confirm text, prune inert on a
package-less machine, adopt/prune agreeing on what an extra is, `--json`
completeness and guards, `undo` firing the repo hook, bundle version-skew
guidance). Plus the tests that make the completeness properties structural —
including one mutation-verified against the exact shipped bug.

The gaps that were *not* closed, and why, are on the roadmap rather than here:
each one either changes documented behaviour or the schema, and those are the
folder owner's call.
