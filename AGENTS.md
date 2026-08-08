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

`SPEC.md`, `WORKFLOWS.md`, `ARCHITECTURE.md`, `README.md`, and `PRINCIPLES.md`
are **compiled into `temper --llm`** (see `crates/temper/src/main.rs`) — that
guide is how humans *and* LLMs learn to operate and author a temper folder. Stale
docs don't just read wrong; they actively mislead every downstream agent that
builds a spec from them.

So a change to behaviour ships with the doc change **in the same commit**:

- **Touched the schema** (a serde struct / manifest field / allowed value /
  `deny_unknown_fields`, a `setkey` backend, a primitive)? Update **`SPEC.md`** —
  it is the **parser-of-record** and must match the serde structs *exactly*,
  including the "Not in the schema" list. Add/adjust a worked example.
- **Added/changed/removed a verb, flag, or its behaviour**? Update the clap
  help (it renders into `--llm`), **`WORKFLOWS.md`** (the operating loops), and
  the **`README.md`** if it changes what the tool is or how you start.
- **Changed the model** (scopes, gating, lifecycle, journaling, a design
  principle)? Update **`ARCHITECTURE.md`** / **`PRINCIPLES.md`**.

Then **rebuild** so `--llm` reflects it (the docs are `include_str!`-embedded at
compile time). When SPEC and the code disagree, the code wins and the doc is the
bug — fix the doc. A quick self-check: could a fresh LLM author a correct folder
from `temper --llm` alone? If your change would make it guess or fail, the doc
isn't done.

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

1. **Can it drift both ways?** installed-but-undeclared *and*
   declared-but-absent. If only one is reported, say why in a comment.
2. **Does each direction name a verb that exists?** Machine→spec and spec←machine.
   "It's a hand edit" is a legitimate answer only after checking that no verb
   *could* do it.
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
6. **Is the new `Finding.kind` in `KIND_ANSWERS`?** The coverage test fails
   otherwise — which is the point. Reaching for `NoVerb` is the moment to ask
   whether the verb simply hasn't been built.

`plan::KIND_ANSWERS` makes 5 mechanical, and a CLI test asserts every command a
remediation names is a real verb (drift once told users to run `temper snapshot`
for a whole release after that verb was renamed). The rest are judgement, which
is why they are written down here.
