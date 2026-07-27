# temper

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
5. ⏭️ **Migrate** ReinstallScripts onto temper incrementally (the `armada` repo),
   shrinking the bash as each piece lands.

## Status

| Area | Built | Verified |
|---|---|---|
| `install` / `update` / `drift` / `prune` / `backup` / `adopt` / `undo` | ✅ | sandbox + real drift |
| `--json`, `--dry-run`, completions, `--man`, `--llm` | ✅ | ✅ |
| `copy` (verbatim · template · seed · mode) | ✅ | ✅ sandbox |
| `block` (marker region) | ✅ | ✅ sandbox + real drift |
| `setkey` — json · toml · ini/.desktop · **defaults** | ✅ | ✅ sandbox |
| `setkey` — **dconf** | ✅ | — (Linux/VM) |
| `exec` (check-hook · secrets; runs as the user) | ✅ | ✅ sandbox |
| `[[assert]]` — absent / contains-line / mode / executable-resolves / not-member / shell / json-semantic | ✅ | ✅ sandbox + real drift |
| journal / `undo` (named-run or newest · `--list` · after-hash-guarded, skip-and-report) | ✅ | ✅ sandbox |
| os + role step-gating (validated; unknown os/role errors) | ✅ | ✅ sandbox |
| host-OS guard (live `install` refuses cross-OS; drift/dry-run don't) | ✅ | ✅ sandbox |
| presence-gating (`when`/`needs` probes) | ❌ design | — (only os/role gating today) |
| packages: parse · effective-set · missing/extras | ✅ | ✅ unit + **real brew drift** |
| providers: brew / cask / tap / flatpak / mas / vscode — probe | ✅ | ✅ real (drift) |
| providers: … converge (`brew bundle` / `flatpak install`) | ✅ | ⏳ **VM** (writes) |
| providers: **gext** / **rpm-ostree** | ✅ | — (Linux/VM) |
| `profile` (macOS .mobileconfig) | ✅ | — (manual/System Settings) |
| discovery (auto-scan cloud folders) beyond `$TEMPER_DIR` + cwd walk-up | ⏳ | — |

**VM run checklist** (things only a live *write* exercises): package converge
(`brew bundle`, `flatpak install`), `brew upgrade` on `update`, dependency-aware
`prune`, `brew bundle dump` on `backup`, dconf/`defaults` writes, and
`gext`/`rpm-ostree` layering. The read-only paths (all of `drift`, package
probing, `install --dry-run`) are verified against a real machine. Known
limitations: `setkey` toml reserializes (drops comments); `defaults`/`dconf`
writes aren't journaled; `profile` install is manual; `run = "ensure"` currently
behaves like `always`; presence-gating (`when`/`needs`) is unbuilt (os/role
only); role-gating is per-step (bundles must opt in — steel guards servers by
their app list too).

`WORKFLOWS.md` (compiled into `--llm`) gets written once the VM run confirms the
real-machine behavior.

[`grove`]: https://github.com/jakobhviid/grove
[`amdl`]: https://github.com/jakobhviid/amdl
[`dotsync`]: https://github.com/jakobhviid/dotsync
