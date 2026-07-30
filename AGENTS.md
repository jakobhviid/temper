# Agent guidelines

Instructions for any AI coding agent (Claude Code, opencode, Cursor, …) working
in this repository.

## Attribution — never attribute AI in the repo

- **Never** add AI/assistant attribution to commits or pull requests: no
  `Co-Authored-By: Claude` (or any other assistant) trailer, and no
  "🤖 Generated with …" line. Author every commit solely as the repository owner.
- AI assistance is disclosed **once**, in the README's "AI disclosure" section —
  that is the only place it belongs. Keep it out of the commit history entirely.

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
