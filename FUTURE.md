# FUTURE.md — temper's deferred work & known limitations

Not bugs (nothing here is broken) — **deliberately deferred features** and
**inherent limitations**, each with why it's parked, the current mitigation, and
enough of a sketch to act on cold. The user-facing summary lives in the README
status table (and rides `--llm`); this is the maintainer's "why + plan" ledger.

See `ARCHITECTURE.md` for the model and `SPEC.md` for the implemented schema.

---

## Deferred features (buildable — just not built yet)

### Presence-gating (`when` / `needs`)
**Status:** designed, not built. ARCHITECTURE describes it; the parser rejects
`when`/`needs` (deny_unknown_fields).
**Now:** steps gate on `os` + `role` only. The "run config only if the app is
present" intent is met today by (a) exec scripts' own `command -v` guards and
(b) drift's `executable_resolves` assert. A `copy` to `~/.config/ghostty/config`
on a box without Ghostty just leaves a dead file — harmless.
**Sketch:** add `when`/`needs` to `Step`; a probe enum
`binary|brew|cask|flatpak|mas|gext|rpm|path|exec`; evaluate before apply; skip
loudly (Principle #6). Default `when` = "my declared package is installed."

### `extensions` / `rpm` not os/role-gated
**Status:** deferred. These bundle-level lists are aggregated across a machine's
apps with no os/role filter.
**Now:** convention (servers don't list gnome/proton-vpn) + `have(gext|rpm)`
guards + the host-OS install guard. A Fedora **server** that wrongly listed a
desktop bundle could still `rpm-ostree install` proton-vpn.
**Sketch:** either move them to `[[step]]`s (which have os/role), or add os/role
to the bundle and filter in `effective_extensions`/`effective_rpm`.

### Per-machine / per-OS template vars
**Status:** deferred. `[vars]` is global.
**Now:** `BREW_PREFIX` is Mac-valued, so a Linux machine's `zsh` image drifts on
the prefix (Homebrew is at `/home/linuxbrew/.linuxbrew` on Bazzite).
**Sketch:** `[[machine]].vars` and/or per-OS var tables, merged over global
`[vars]`. Or a `{{ brew_prefix }}` dynamic function.

### Discovery auto-scan
**Status:** deferred. Only `$TEMPER_DIR` + a cwd walk-up.
**Sketch:** port dotsync's `discovery.rs` (scan common cloud-folder locations, a
saved pointer, first-run prompt).

### `mas` converge is not "forgiving"
**Status:** deferred. ARCHITECTURE says MAS failures should be reported-not-fatal
(no App Store sign-in → skip, continue). Today `mas` rides the aggregate
`brew bundle`, which bails on any failure → a MAS failure fails the whole run.
**Sketch:** converge mas separately from `brew bundle` (own `mas install` loop)
so its failures are warned, not fatal.

### Declarative system-file primitive (the clean `/etc` path)
**Status:** idea. Root-owned config (the 1Password `/etc` allowlist) is done via
an `exec` script that self-escalates.
**Sketch:** a first-class primitive that writes one root-owned file with
mode/owner, escalating internally for just that write (Ansible's per-task
`become` on a `copy`). Lets `/etc` writes be declarative + drift-checkable
instead of buried in `exec`.

---

## Inherent limitations (can't fully fix; document, don't chase)

- **`exec` / `setkey(defaults)` / `setkey(dconf)` aren't journaled** → `undo`
  can't revert them (they mutate a domain / dconf DB / arbitrary state, not a
  file we can snapshot). Drift still catches them; they degrade to `unavailable`
  when their tool is absent. A future defaults/dconf journal could snapshot the
  prior `read` value, but it's fiddly and low-value.
- **`setkey(toml)` drops comments** — the `toml` crate reserializes. Fix would be
  `toml_edit` (format/comment-preserving). Fine for machine-written config; a
  trap for hand-commented TOML.
- **`profile` (macOS `.mobileconfig`) install is manual** — modern macOS needs
  user approval in System Settings (no scriptable install without MDM). Drift is
  status-only ("manual"); apply just `open`s it.
- **`run = "ensure"` for an `exec` with no `check`** is skipped on `update` (we
  can't tell "missing"). Give such steps a `check`, or use `run = "always"`.

---

## Not temper's job (scope boundary)

- **Bootstrap** — getting brew + temper onto a bare machine runs before the tap
  exists (the paradox). The plan is a small public getting-started repo (like
  grove/amdl/dotsync's `install.sh` fallback). Deferred.
- **OS image** — building the Bazzite image (rebase, cosign, baked system layer)
  is the Stacks repo's job, never temper's.

---

## Verification gap (a state, not a limitation)

The Linux half of the `steel` migration is transcribed + parse-valid but has
**never run** — the dconf loads, 1Password NMH surgery, PWAs, speaker-eq exec
scripts await a VM. See the README "VM run checklist". Mac config is
drift-verified against a real machine. ReinstallScripts stays as the fallback
until the VM run confirms Linux.
