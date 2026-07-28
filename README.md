# temper

**Keep your machines the way you declared them.** Describe a machine — its
packages *and* its configuration — in a folder of plain, human-readable files.
`temper` makes the machine match, shows you exactly what has drifted, and can
revert what it changed.

The **tool** is public and identical for everyone. Your **spec** is a folder
*you* bring, however you like — a git repo, a synced Nextcloud/Dropbox folder, a
USB stick. temper never manages that folder; it only reads it.

```sh
temper drift      # what's out of sync on this machine (read-only)
temper install    # converge the machine to the spec
temper undo        # revert the last run
```

## Why temper

- **Real files, not a DSL.** A recipe is "as readable as a Brewfile" — an actual
  `starship.toml`, a real dconf dump, a real Brewfile, tied together by one
  manifest. No new language to learn.
- **Packages *and* config, one tool.** brew / cask / tap / flatpak / mas /
  VS Code / GNOME extensions / rpm-ostree, plus file, key, marker-block, and
  root-owned `/etc` config primitives.
- **Drift is first-class.** `temper drift` is read-only and tells you what's out
  of sync **and the exact command to fix it** — in both directions.
- **Two directions.** Converge the machine toward the spec, *or* absorb the
  machine's live state back into the spec (`temper reconcile`).
- **Reversible.** Every file write (and every dconf key) is journaled; `temper
  undo` rolls back the last run, guarded so it never clobbers a since-changed file.
- **Gates on reality.** A config step runs only where its app is actually present
  (a presence probe), so one machine's app list can be generous without config
  landing where it shouldn't.
- **Lighter than the alternatives.** No inter-step data flow like Ansible, no
  world-rebuild like Nix; it adds package convergence, real drift, and machine
  identity that a dotfile manager like chezmoi doesn't have.

## Install

```sh
# Prebuilt binary — no Homebrew, compiler, or root required:
curl -fsSL https://raw.githubusercontent.com/jakobhviid/temper/main/install.sh | sh

# …or via Homebrew:
brew install jakobhviid/tap/temper
```

## Quickstart

A temper folder is one manifest, a recipe per app, and the real files they deploy:

```
temper.toml           # machines + which apps each composes
apps/shell.toml       # a bundle: ordered steps (copy / setkey / block / exec / …)
assets/starship.toml  # the real config files the recipes deploy
```

```toml
# temper.toml
[[machine]]
name     = "laptop"                        # matched against your hostname
os       = "mac"                           # "mac" | "linux"
apps     = ["shell"]
packages = ['brew "jq"', 'cask "ghostty"'] # loose Brewfile-grammar tokens
```

```toml
# apps/shell.toml
[[step]]
copy = "assets/starship.toml"
to   = "~/.config/starship.toml"
```

Then, from inside the folder (or after `temper use <dir>` records where it lives):

```sh
temper drift              # what's out of sync — reads only
temper install --dry-run  # what an install would change — writes nothing
temper install            # converge for real
```

`drift` is the hub: its report ends with **Next steps** — the exact command for
each way out of the drift (add the missing packages, remove the extras, absorb
them into the spec, re-apply config, or undo). See **[WORKFLOWS.md](WORKFLOWS.md)**
for the day-to-day loops, or run `temper --llm` for the whole guide in one blob.

## How it works

temper splits the **engine** (this tool — it knows nothing about any specific
machine, person, or app) from the **data** (your folder). The engine has a small,
closed set of **primitives** — `copy`, `block`, `setkey` (json/toml/ini/dconf/
macOS-defaults), `sysfile`, `exec`, the package providers — that your open,
code-free **app bundles** compose. Adding an app is a config file you write, never
a tool release.

- **[ARCHITECTURE.md](ARCHITECTURE.md)** — the model: engine-vs-data, the two
  scopes, primitives, converge/probe/gate, lifecycle, the verbs.
- **[SPEC.md](SPEC.md)** — the concrete `temper.toml` + app-bundle schema.
- **[WORKFLOWS.md](WORKFLOWS.md)** — the operating loops (drift → decide → run).
- **[PRINCIPLES.md](PRINCIPLES.md)** — the guardrails that keep it small.
- **[ROADMAP.md](ROADMAP.md)** — what's parked and what's deliberately out of scope.

## Origin

temper generalizes a private ~3,900-line bash repo (`ReinstallScripts`) that
provisioned one fleet of Macs and Linux boxes — extracting the *engine* into an
open tool and leaving the *data* in a folder anyone can bring. It's built in the
same Rust-on-a-shared-tap style as its siblings [`grove`], [`amdl`], and
[`dotsync`].

## Known Limitations

- **`setkey(toml)`** preserves comments and formatting, *except* the changed
  key's own inline comment.
- **`setkey(defaults)`, `sysfile`, and `exec` aren't undoable** — they mutate
  system-side or arbitrary state, not a file temper can snapshot (dconf writes
  *are* journaled and undoable).
- **`profile`** (macOS `.mobileconfig`) install is a manual System-Settings step;
  drift on it is status-only.

See `ROADMAP.md` for the full ledger of deferred features and scope boundaries.

## AI disclosure

Parts of this codebase were written with the assistance of AI coding agents
(Claude Code, opencode, and others). All changes were reviewed by the maintainer.

## License

MIT

[`grove`]: https://github.com/jakobhviid/grove
[`amdl`]: https://github.com/jakobhviid/amdl
[`dotsync`]: https://github.com/jakobhviid/dotsync
