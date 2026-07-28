# temper

> **Built and released** — every verb is live (on `jakobhviid/homebrew-tap`,
> beside [`grove`], [`amdl`], [`dotsync`]). `temper` converges a machine to a
> declared spec kept in a folder of human-readable files (git, Nextcloud, a USB
> disk — the tool doesn't care how the folder arrives). It generalizes the
> private `ReinstallScripts` bash into one open, manifest-driven CLI. Read-only
> paths (drift, dry-run, package probing) are verified against a real machine;
> the live-write platform paths still await a VM run and the ReinstallScripts
> migration hasn't started — see **Status** below, and `ROADMAP.md` for the
> workflow/HMI parity gaps still open against RIS.

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
| presence-gating (`when` soft-skip / `needs` hard-require; 9 probe kinds) | ✅ | ✅ sandbox + live |
| packages: parse · effective-set · missing/extras | ✅ | ✅ unit + **real brew drift** |
| providers: brew / cask / tap / flatpak / mas / vscode — probe | ✅ | ✅ real (drift) |
| providers: … converge (`brew bundle` / `flatpak install`) | ✅ | ✅ **brew bundle live on Bazzite**; flatpak ⏳ |
| `install --packages-only` (additive "install-missing" — packages, no config) | ✅ | ✅ live (brew) |
| `reconcile` (interactive spec←machine: add/drop Brewfile entries, flatpak→[ignore]) | ✅ | ✅ live drop + `--json` preview |
| dconf snapshot `backup` (filtered) + `restore` (confirm-gated) | ✅ | ✅ **live round-trip on Bazzite** |
| `eq-import` (fetch calibrated speaker profiles into the folder) | ✅ | ✅ live clone/scan/cleanup |
| providers: **gext** / **rpm-ostree** | ✅ | — (Linux/VM) |
| `profile` (macOS .mobileconfig) | ✅ | — (manual/System Settings) |
| discovery (auto-scan cloud folders) beyond `$TEMPER_DIR` + cwd walk-up | ⏳ | — |

**VM run checklist** (things only a live *write* exercises): `brew bundle`
formula converge is now **verified live on a Bazzite host** (install-missing of a
throwaway formula); still pending a full run: `flatpak install`, `brew upgrade`
on `update`, dependency-aware `prune`, `brew bundle dump` on `backup`,
dconf/`defaults` writes, and `gext`/`rpm-ostree` layering. The read-only paths
(all of `drift`, package probing, `install --dry-run`) are verified against a
real machine. Known
limitations: `setkey` toml now preserves comments/formatting (toml_edit) —
except the *changed* key's own inline comment; `defaults`/`dconf`
writes aren't journaled; `profile` install is manual; presence-gating
(`when`/`needs`) is unbuilt (os/role gating only). Steps, asserts, **and
bundle-level `extensions`/`rpm`** now os/role-gate (a server can't layer a
desktop bundle's extensions/rpm even if it composes it).

`WORKFLOWS.md` (compiled into `--llm`) gets written once the VM run confirms the
real-machine behavior.

[`grove`]: https://github.com/jakobhviid/grove
[`amdl`]: https://github.com/jakobhviid/amdl
[`dotsync`]: https://github.com/jakobhviid/dotsync
