# Agent guidelines

Instructions for any AI coding agent (Claude Code, opencode, Cursor, …) working
in this repository.

## Attribution — never attribute AI in the repo

- **Never** add AI/assistant attribution to commits or pull requests: no
  `Co-Authored-By: Claude` (or any other assistant) trailer, and no
  "🤖 Generated with …" line. Author every commit solely as the repository owner.
- AI assistance is disclosed **once**, in the README's "AI disclosure" section —
  that is the only place it belongs. Keep it out of the commit history entirely.
- If your tooling adds attribution by default, **turn it off at the source instead of
  fighting it per commit, and help the user do the same.** For Claude Code, set
  `includeCoAuthoredBy` to `false` in `~/.claude/settings.json` (it is on by default).
  A one-liner to hand the user (needs `jq`):

  ```sh
  f=~/.claude/settings.json; [ -f "$f" ] || printf '{}' > "$f"; \
    tmp=$(mktemp); jq '.includeCoAuthoredBy = false' "$f" > "$tmp" && mv "$tmp" "$f"
  ```

  Once it is off, no attribution is emitted at all and this rule holds effortlessly.

## Documentation is load-bearing — keep it in sync with the code

`SPEC.md`, `WORKFLOWS.md`, `PATTERNS.md`, `ARCHITECTURE.md`, `README.md`,
`PRINCIPLES.md`, and `MIGRATION-GUIDE.md` are **compiled into `temper --llm`**
(see `crates/temper/src/main.rs`) — that guide is how humans *and* LLMs learn to
operate and author a temper folder. Stale docs don't just read wrong; they
actively mislead every downstream agent that builds a spec from them.

`MIGRATION-GUIDE.md` is the **one** doc whose job is the before/after, and the
only place exempt from the "documenting the diff" rule below. It gains a section
per **major** version — the ones where a folder has to be edited — and loses one
when nobody can still be migrating from it.

So a change to behaviour ships with the doc change **in the same commit**:

- **Touched the schema** (a serde struct / manifest field / allowed value /
  `deny_unknown_fields`, a `setkey` backend, a primitive)? Update **`SPEC.md`** —
  it is the **parser-of-record** and must match the serde structs *exactly*,
  including the "Not in the schema" list. Add/adjust a worked example. A test
  scrapes the structs and fails on a field SPEC does not name, so this half is
  mechanical; the worked example and the "Not in the schema" list are still
  yours.
- **Added/changed/removed a verb, flag, or its behaviour**? Update the clap
  help (it renders into `--llm`), **`WORKFLOWS.md`** (the operating loops), and
  the **`README.md`** if it changes what the tool is or how you start. Renaming
  one is the case to watch: the alias keeps every example working, so nothing
  fails and the docs quietly go on teaching the dead name. A test now asserts
  every `` `temper <verb>` `` in the docs is the **canonical** spelling — an
  alias does not count, which is the whole point.
- **Changed the model** (scopes, gating, lifecycle, journaling, a design
  principle)? Update **`ARCHITECTURE.md`** / **`PRINCIPLES.md`**.

Then **rebuild** so `--llm` reflects it (the docs are `include_str!`-embedded at
compile time). When SPEC and the code disagree, the code wins and the doc is the
bug — fix the doc. A quick self-check: could a fresh LLM author a correct folder
from `temper --llm` alone? If your change would make it guess or fail, the doc
isn't done.

### "Documenting the diff" — the doc failure mode, named

Updating a doc is not the same as narrating the update. The reflex is to *edit
around* the stale sentence so it describes the transition: "drift **is** now
checked", "this **no longer** needs root", "opened **instead of** being skipped",
"a real answer, **now that** the check exists". Every word can be true and the doc
still be wrong, because it is describing your commit instead of the software.

Two reasons it costs real work. A reader can't tell a live constraint from a dead
one, so the old state keeps steering them and they route around a problem that no
longer exists — not hypothetical, it is how the dconf-float warning outlived its
bug. And it ages into trivia: one release later, "used to compare doubles as text"
is a fact about a version nobody runs.

The tells are greppable, and worth grepping **your own diff** for before you
commit: `now`, `now that`, `no longer`, `used to`, `actually`, `really`,
`instead of <the old behaviour>`, emphatic italics (`*is*` checked — arguing with a
claim the reader never saw), `(fixed in 3.1.1)`, `DONE`.

The fix, in order:

1. **Rewrite as if the new behaviour were the only one that ever existed.** Present
   tense, no memory. Say *why* only where the why is non-obvious and durable.
2. **Then ask whether the line still earns its place.** A sentence that only made
   sense as a contrast with the old state should be *deleted*, not reworded — and
   one the surrounding text now implies should go too. Prefer a shorter doc to a
   curated stale one.
3. **Put the before/after in the commit message.** That is the artefact built for
   it, and git keeps it with a date and a diff.

## Releases & versioning — auto-incremented from commit type

CI cuts a release on every push to `main`, and the version is **derived
automatically from the commit history** (Conventional Commits) — nobody bumps a
version by hand, so a forgotten manual release still versions correctly. The
commit **subject prefix** decides the bump:

- `feat: …` — a new feature → **minor** bump (1.2.0 → 1.3.0)
- `fix: …` — a bug fix / hotfix → **patch** bump (1.2.3 → 1.2.4)
- `feat!: …` (or any `type!:`, e.g. `fix!:`), or a `BREAKING CHANGE` — a breaking
  change → **major** bump (1.4.2 → 2.0.0)
- anything else (`docs:`, `chore:`, `refactor:`, …) or an un-prefixed subject →
  **patch** bump

So **pick the right commit-subject prefix for the change** and the release version
follows automatically. Never hand-edit `version` in `Cargo.toml` to release — CI
computes and stamps it.

**Green-gate before you push, or no release is cut.** The release job first runs
`cargo clippy --workspace --all-targets -- -D warnings` and the test suite; if
either fails, the push does **not** publish — the intended version simply never
appears (a stale binary keeps installing). `cargo build`/`cargo test` alone is
not enough — **clippy is the gate** (warnings are errors), so run
`cargo clippy --workspace --all-targets -- -D warnings` locally before every
push. (There is deliberately **no** `cargo fmt` gate — don't add one.)

Two failure modes look identical from the outside — "no release appeared" — and are
worth telling apart before you go looking for a bug:

- **Tests/clippy red** → your code. Fix and push again.
- **A build job cancelled without failing**, annotated *"The job was not acquired by
  Runner of type hosted even after multiple attempts"* → GitHub could not allocate a
  runner (seen 2026-08-06: three of four matrix legs cancelled at the same second,
  ~15 min after queueing, while the leg that got a machine passed). Nothing is wrong
  with the commit; re-run the failed jobs and the same SHA publishes normally:

  ```sh
  gh run rerun <run-id> --failed
  ```

  Because the version is derived from commit history, nothing is lost by re-running —
  the same SHA yields the same version. Do **not** push an empty commit to "retry".

Also worth knowing when tests touch the environment: run the suite once with an
isolated git config before pushing, or a local setting can hide a CI failure —
`GIT_CONFIG_GLOBAL=/dev/null cargo test --workspace`. A machine whose
`init.defaultBranch` is `main` will pass tests that assume the branch name and fail
on a stock git, which cost a published release once.

## Adding a feature: walk the matrix before you call it done

temper's model is a matrix — *kind of state* × *direction* × *verb* — and its
recurring defect has been shipping one cell of it: a **report with no way to
act** (gext extras were reported a release before `prune` could remove them), or
a **direction with nowhere to write** (an extension could only be declared in a
shared bundle, so `reconcile` had nothing to absorb into). Both shipped green;
both were found by a user, not by a test.

So when you touch any kind of state, answer all of these in writing before
pushing:

1. **Can it drift both ways, and does each way have TWO answers?** The shape is
   four cells, not two directions: {installed-but-undeclared, declared-but-absent}
   × {change the machine, change the spec}. Register both directions in
   `plan::KIND_ANSWERS` — a test fails on an empty one. `Answer::Hand` is a
   legitimate answer *only* when it names the file a human edits and why.
2. **Does each direction name a verb that exists *and can reach this kind*?**
   "It's a hand edit" is legitimate only after checking that no verb *could* do
   it — and naming a verb that has no code path for the kind is worse than
   naming none, because the user runs it and it silently does nothing. That is
   how a removed GNOME extension came back on every converge for two releases.

2b. **Can the state be OBSERVED here, and is that separate from being able to
   change it?** A host may enumerate a kind and be unable to converge it, or the
   reverse. Where the state cannot be read, report `unavailable` — never absent —
   and compute no drops at all: every write path reads "empty" as "delete what
   the spec captured", which is how one command could publish an emptied spec to
   the whole fleet.
3. **What SCOPE does absorbing it write to?** Anything absorbed from one
   machine's live state must land somewhere that belongs to **that machine** — its
   own Brewfile, its own `[[machine]]` block, its own snapshot file. A shared
   bundle and fleet config (`[brew].trust`, `[ignore]`) are both off-limits by
   default: writing either from one box silently changes every other. If it must
   be fleet-scope, make it an explicit opt-in flag and *report* what you skipped.
   (`--include-trust` deleted a tap the rest of the fleet needed, because this
   question got answered "it's symmetric" instead of "whose file is it?".)
4. **Does the spec-writing path fire `after_repo_change`?** A verb that writes the
   folder and skips it leaves a git-backed home silently dirty — `init` did,
   because it delegated to a `reconcile` that returned early.
5. **Does its output survive `--json`, and agree with `drift`?** Any new
   human-facing line needs a `!json` guard (one stray `println!` makes stdout
   unparseable — Principle #6b), and any "nothing to do" message must not
   contradict what `drift` reports in the same breath. `reconcile` claimed
   "already in sync" while drift listed six findings.
6. **Is the new `Finding.kind` in `KIND_ANSWERS`, with both cells filled?** The
   coverage tests fail otherwise — which is the point. Reaching for `NA`/`Hand`
   is the moment to ask whether the verb simply hasn't been built. Keep kinds as
   **literal** strings: the completeness test scrapes source for `kind: "…"`, so
   a kind built with `format!` is invisible to it.

7. **Is the converge revertible, and does the user learn that BEFORE
   confirming?** `undo` covers less on macOS than on Linux (`setkey(defaults)`
   is deliberately not journaled), and a run whose only changes were unjournaled
   reverts nothing while reporting success. If a step's effect can't be undone,
   say so at plan time, not afterwards.

`plan::KIND_ANSWERS` now makes 1, 2 and 6 mechanical: every kind must fill both
cells, every verb it names must exist and parse, and `remediations` may not offer
a command the registry does not record for that kind — the reverse check, which
was missing while drift advised `reconcile` for three kinds reconcile cannot
touch. A CLI test still asserts every named command is a real verb (drift once
told users to run `temper snapshot` for a whole release after that verb was
renamed). The rest are judgement, which is why they are written down here.
