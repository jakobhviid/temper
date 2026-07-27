# fleet — Manifest Spec

> **Status: design, sanity-checked 2026-07-27.** Field names are still
> provisional, but the shape reflects the full ReinstallScripts gap analysis.
> See `ARCHITECTURE.md` for the model behind these choices.

Files in a fleet-home folder:

1. **`fleet.toml`** — machine registration, per-machine composition, ignores.
2. **`apps/<name>.toml`** — one app-bundle recipe each (open set).
3. **`assets/…`** — the real, human-readable files the recipes reference.

---

## `fleet.toml`

```toml
[[machine]]
name = "chronos"          # matches hostname (or a saved marker)
os   = "mac"              # mac | linux
# role is DERIVED from a gnome-shell probe, not trusted from here (safety:
# a server can't look like a desktop). Declare only to override/assert.
apps = ["base", "ghostty", "1password", "starship"]
packages = [             # per-machine LOOSE packages that belong to no app
  "cask 'raycast'", "cask 'figma'",   # the ~130 hand-curated casks land here
]

[[machine]]
name = "eternium"
os   = "linux"
role = "server"           # explicit; still cross-checked against the probe
apps = ["base"]

# Packages installed on the machine that fleet must NOT flag as extras or offer
# to prune (OS-preinstalled Bazzite flatpaks, etc.). The reconcile "ignore this"
# disposition writes here.
[ignore]
flatpak = ["org.gnome.Calculator", "…"]   # was bazzite-flatpak-ignore.txt
brew    = []
```

**Effective package set for a machine** = union(composed apps' `packages`) +
machine `packages` − `[ignore]`. One aggregate converge call per manager.

---

## `apps/<name>.toml` — an app-bundle

```toml
packages       = ["cask 'ublue-os/tap/1password-gui-linux'"]
packages_mac   = ["cask '1password'"]
packages_linux = ["cask 'ublue-os/tap/1password-gui-linux'"]

when = { cask = "1password" }   # presence gate; default = "my packages installed"

[[step]]
copy = "assets/ghostty.config"
to   = "~/.config/ghostty/config"        # run defaults "always" for copy

[[step]]
copy = "assets/ssh/shared.conf"
to   = "~/.ssh/config.d/shared.conf"
mode = "0600"                            # file perms (SSH refuses loose perms)

[[step]]
block = "assets/ssh/include-line"        # ensure a marker line is present, once
in    = "~/.ssh/config"
marker = "# fleet — ssh include"

[[step]]
copy = "assets/zshrc-bootstrap"
to   = "~/.zshrc"
mode_lifecycle = "seed"                  # create-once, then hands-off, no drift

[[step]]
setkey  = { backend = "dconf", key = "/org/gnome/settings-daemon/…/command",
            value = "{{ which \"1password\" }} --silent" }   # dynamic value
os      = "linux"  ;  needs = "gnome-shell"

[[step]]
setkey  = { backend = "dconf", key = "…/custom-keybindings", append = true,
            value = "/…/custom0/" }      # list-union append, preserves user entries
os      = "linux"

[[step]]
setkey  = { backend = "defaults", domain = "com.brave.Browser",
            key = "NSUserKeyEquivalents", dict_add = { "Close Window" = "@$w" } }
os      = "mac"                          # the macOS dconf-twin

[[step]]
exec    = "assets/1p-nmh-setup.sh"       # the escape hatch
check   = "assets/1p-nmh-check.sh"       # drift hook: exit 0 = in sync
os      = "linux"  ;  run = "install"  ;  sudo = true
secrets = ["ACOUSTID_KEY"]               # env/secret passthrough

[[assert]]
absent = "~/.zshrc.local"                # must-NOT-exist
[[assert]]
not_member = { group = "onepassword" }   # user must not be in group
os = "linux"
```

### Step fields

| Field | Meaning |
|---|---|
| *one of* `copy` / `block` / `setkey` / `profile` / `exec` | the primitive + source |
| `to` / `in` | target path (string, per-OS table, or **list** for multi-target — rustdesk native + flatpak) |
| `mode` | file permission (`copy`) |
| `template` | `copy`: substitute declared vars + `{{ … }}` apply-time probes |
| `mode_lifecycle = "seed"` | `copy`: create-once, hands-off, excluded from drift |
| `os` / `role` | skip on other OS / role |
| `needs` | extra presence probe for this step (`gnome-shell`) |
| `run` | `always` \| `install` \| `ensure` \| `manual` — lifecycle; defaults by primitive |
| `check` | `exec`: companion drift-hook script (exit code = in/out of sync) |
| `sudo` | `exec`/`rpm-ostree`: needs privilege; shown in plan, best-effort undo |
| `secrets` | env vars / `secrets/` entries passed to the step |

### `setkey` backends

`dconf` · `defaults` (macOS) · `ini`/`.desktop` · `json` · `toml`. Common
options: `key`/`value`, `append` (list-union), `dict_add`, dynamic `{{ … }}`
values. Sets the named key(s), preserves all siblings. Drift reads the key back;
dynamic values compare semantically.

### Assertions (`[[assert]]`) — drift-only, no converge

`absent` · `mode`/`owner` · `contains_line` · `not_member` · `executable_resolves`
· `json_semantic` · `shell`. Each is OS/role-gatable. These express the
must-not-exist / property / semantic checks `just drift` does today.

### Probe vocabulary (`when` / `needs`)

`binary` · `brew`/`cask` · `flatpak` · `mas` · `gext` · `rpm` · `path` · `exec`.

### Target shapes

```toml
to = "~/.config/starship.toml"
to = { mac = "~/Library/Preferences/…/RustDesk2.toml",
       linux = ["~/.config/rustdesk/RustDesk2.toml",
                "~/.var/app/com.rustdesk.RustDesk/config/rustdesk/RustDesk2.toml"] }
```

---

## Resolved open questions (were flagged for sanity-check)

- **App-first** recipes (`apps/*.toml`), tiered assets, machine-scope state under
  `machines/`. ✔
- **Machine-scope config is its own thing** (loose `packages`, `[ignore]`, dconf
  snapshots) — *not* forced into a synthetic `base` app. A `base` app still
  exists for the shared CLI baseline, but the escape hatch is the loose list. ✔
- **`update` is upgrade + `ensure`-allowlist**, not strictly upgrade-only. ✔
- **`packages` reuse Brewfile line grammar** so `brew bundle` keeps working; the
  effective Brewfile is a generated artifact (union + loose − ignore),
  materializable for inspection. ✔
- **`template` var source:** declared in `fleet.toml`/bundle + `{{ … }}`
  apply-time probes for live values (`BREW_PREFIX`, `which`, sink-match). ✔

## Still open (decide during build)

- Exact `[[assert]]` type list — start with the 8 above, grow only as RIS needs.
- `setkey` merge semantics for deeply-nested json/toml (shallow key-set first).
- Whether `gext`/`rpm-ostree` share the `packages` grammar or get their own
  bundle fields.
