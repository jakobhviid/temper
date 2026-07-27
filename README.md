# fleet

> **Early design — no code yet.** `temper` will converge a machine to a declared
> spec kept in a folder of human-readable files (git, Nextcloud, a USB disk —
> the tool doesn't care how the folder arrives). It generalizes the private
> `ReinstallScripts` bash into one open, manifest-driven CLI, in the same
> Rust-on-a-shared-tap pattern as [`grove`], [`amdl`], and [`dotsync`].

## Where the design lives

- **[ARCHITECTURE.md](ARCHITECTURE.md)** — the model: engine-vs-data, the two
  scopes, primitives, converge+probe+gate, lifecycle, engine operations.
- **[SPEC.md](SPEC.md)** — the concrete `temper.toml` + app-bundle schema.
- **[PRINCIPLES.md](PRINCIPLES.md)** — the guardrails that keep it small.

## Build sequence

1. ✅ **Draft the design.**
2. ✅ **Sanity-check** against the *entire* ReinstallScripts repo; docs revised
   with the 8 changes the pass surfaced.
3. ✅ **Scaffold** the Cargo workspace from the grove/amdl/dotsync template.
4. ✅ **Build** the primitives + verbs (see status below). All filesystem/logic
   paths are unit- or integration-tested in sandboxes; the live package-manager
   and platform paths are built and await a VM run.
5. ⏭️ **Migrate** ReinstallScripts onto fleet incrementally (the `armada` repo),
   shrinking the bash as each piece lands.

## Status

| Area | Built | Verified |
|---|---|---|
| `install` / `update` / `drift` / `prune` / `backup` / `adopt` / `undo` | ✅ | sandbox (filesystem paths) |
| `--json`, `--dry-run`, completions, man, `--llm` | ✅ | ✅ |
| `copy` (verbatim · template · seed · mode) | ✅ | ✅ sandbox |
| `block` (marker region) | ✅ | ✅ sandbox |
| `setkey` — json backend | ✅ | ✅ sandbox |
| `setkey` — toml / ini / **dconf** / **defaults** | ⏳ | — (dconf/defaults need VM) |
| `exec` (check-hook · secrets · sudo) | ✅ | ✅ sandbox |
| `[[assert]]` — absent / contains-line / mode / executable-resolves | ✅ | ✅ sandbox |
| `[[assert]]` — not-member / shell / json-semantic | ⏳ | — |
| journal / `undo` (content-addressed, after-hash-guarded) | ✅ | ✅ sandbox |
| packages: parse · effective-set · missing/extras | ✅ | ✅ unit |
| providers: brew / cask / tap / flatpak / mas / vscode (probe + converge) | ✅ | ⏳ **VM** |
| providers: **gext** / **rpm-ostree** | ⏳ | — (Linux/VM) |
| discovery (auto-scan cloud folders) beyond `$TEMPER_DIR` + cwd walk-up | ⏳ | — |

**VM run checklist** (things only a real machine exercises): package converge
(`brew bundle`, `flatpak install`), `brew upgrade` on `update`, dependency-aware
`prune`, `brew bundle dump` on `backup`, dconf/`defaults` `setkey`, and
`gext`/`rpm-ostree`. Use `temper install --dry-run` / `temper drift` first — both
read-only-ish — before a live converge.

`WORKFLOWS.md` (compiled into `--llm`) gets written once the VM run confirms the
real-machine behavior.

[`grove`]: https://github.com/jakobhviid/grove
[`amdl`]: https://github.com/jakobhviid/amdl
[`dotsync`]: https://github.com/jakobhviid/dotsync
