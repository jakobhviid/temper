# fleet

> **Early design — no code yet.** `fleet` will converge a machine to a declared
> spec kept in a folder of human-readable files (git, Nextcloud, a USB disk —
> the tool doesn't care how the folder arrives). It generalizes the private
> `ReinstallScripts` bash into one open, manifest-driven CLI, in the same
> Rust-on-a-shared-tap pattern as [`grove`], [`amdl`], and [`dotsync`].

## Where the design lives

- **[ARCHITECTURE.md](ARCHITECTURE.md)** — the model: engine-vs-data, the two
  scopes, primitives, converge+probe+gate, lifecycle, engine operations.
- **[SPEC.md](SPEC.md)** — the concrete `fleet.toml` + app-bundle schema.
- **[PRINCIPLES.md](PRINCIPLES.md)** — the guardrails that keep it small.

## Build sequence

1. ✅ **Draft the design.**
2. ✅ **Sanity-check** against the *entire* ReinstallScripts repo (Mac tree,
   Linux justfile, Linux libs + installer); docs revised 2026-07-27 with the
   8 changes the pass surfaced (new primitives, drift subsystem, machine
   loose-packages + ignores, the `adopt` verb, lifecycle/copy/exec fixes, the
   named cask-artifact exception).
3. 🏗️ **Scaffold** the Cargo workspace from the grove/amdl/dotsync template.
   ← *next*
4. 🔨 **Build** primitives one at a time, each with plan/apply/drift/undo.
5. 🚚 **Migrate** ReinstallScripts onto fleet incrementally, shrinking the bash
   as each piece lands.

`README.md` and `WORKFLOWS.md` (the latter compiled into `--llm`) get written
for real once behavior exists.

[`grove`]: https://github.com/jakobhviid/grove
[`amdl`]: https://github.com/jakobhviid/amdl
[`dotsync`]: https://github.com/jakobhviid/dotsync
