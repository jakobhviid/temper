# temper — Manifest Schema (as implemented)

This documents the **actual** parsed schema (the serde structs in
`crates/temper-core/src/manifest.rs`). Unknown fields are rejected
(`deny_unknown_fields`), so anything not listed here is a parse error.

A temper-home folder holds `temper.toml`, `apps/<name>.toml` bundles, and the
asset files they reference.

## `temper.toml`

```toml
[vars]                      # optional: GLOBAL template variables, {{ var "X" }}
EDITOR = "hx"

[ignore]                    # optional: installed pkgs drift/prune must not flag
brew    = []
cask    = []
flatpak = []
mas     = []
vscode  = []
tap     = []

[brew]                      # optional
trust = ["ublue-os/tap"]   # third-party taps to `brew trust` before converge/upgrade

[eq_import]                 # optional; `temper eq-import` fetches speaker profiles
repo = "https://github.com/…/pipewire-speaker-profiles"
dest = "assets/speaker-eq"  # default; each <x>.calibrated.conf lands as <x>.conf

[[machine]]
name     = "chronos"        # required; resolved against `hostname -s`
os       = "mac"            # required; "mac" | "linux"
role     = "desktop"        # optional; "desktop" | "server"
apps     = ["shell", "ssh"] # bundle names in apps/
packages = ["cask \"raycast\""]  # optional loose Brewfile-grammar tokens
brewfile = "brewfiles/chronos"   # optional; a Brewfile whose lines join the set

[machine.vars]              # optional; per-machine vars, merged OVER [vars]
BREW_PREFIX = "/home/linuxbrew/.linuxbrew"   # e.g. override a Mac-valued global

[[machine.dconf]]           # optional; whole-desktop dconf snapshots (Linux)
path  = "/org/gnome/shell/"                 # subtree to dump/load (trailing /)
file  = "assets/gnome/shell.chronos.dconf"  # backup writes here; restore reads
strip = ["monitors/", "last-selected"]      # drop these key substrings on backup
```

> **TOML ordering:** `[machine.vars]` and each `[[machine.dconf]]` bind to the
> **preceding** `[[machine]]` — they must sit between that header and the next
> `[[machine]]`. Put them right under the machine they belong to.

Template vars resolve as global `[vars]` overlaid by a machine's own
`[machine.vars]` (per-machine wins). For the common Homebrew-prefix split, prefer
the live function `{{ brew_prefix }}` (§templating) over declaring `BREW_PREFIX`
per machine — it resolves `brew --prefix` on the box, so one template works on
both OSes.

Effective package set for a machine = union(each app's `packages`(+`_mac`/
`_linux`), the machine `packages`, and the machine `brewfile` lines) − `[ignore]`.

## `apps/<name>.toml` (a bundle)

```toml
os             = "linux"             # optional bundle-level gate: skip this bundle's
role           = "desktop"           #   `extensions`/`rpm` ONLY unless the machine's
                                     #   os/role match. It does NOT gate the bundle's
                                     #   [[step]]s — those gate on their own os/role/
                                     #   when/needs (a server composing this still runs
                                     #   its file/key steps).
packages       = ["brew \"jq\""]     # Brewfile-grammar tokens (all-OS)
packages_mac   = []                  # mac-only
packages_linux = []                  # linux-only
extensions     = ["ext@uuid"]        # GNOME extensions (gext; Linux) — os/role-gated
rpm            = ["proton-vpn-gnome-desktop"]  # rpm-ostree layered (Linux) — os/role-gated

[[step]]   # ordered; each step sets EXACTLY ONE primitive
# … see steps below …

[[assert]] # drift-only checks; each sets EXACTLY ONE check
# … see asserts below …
```

Package token grammar (same as a Brewfile line):
`brew "x"` · `cask "x"` · `tap "u/r"` · `flatpak "app.id"` · `vscode "ext"` ·
`mas "Name", id: 123`.

## Steps (`[[step]]`) — one primitive each

Common: `os = "mac"|"linux"` and `role = "desktop"|"server"` (skip on a
non-matching OS/role; an unknown os/role errors at load); `run = "always"|
"install"|"ensure"|"manual"` (lifecycle; default: copy/block/setkey → always,
exec/seed → install). Presence gates (gate config on **reality**):
`when = { <probe> }` skips the step (loudly) unless the probe passes;
`needs = { <probe> }` errors unless it passes. A probe is exactly one of
`binary` / `path` / `brew` / `cask` / `flatpak` / `mas` / `gext` / `rpm` /
`exec` (e.g. `when = { binary = "ghostty" }` — deploy ghostty config only where
ghostty is actually present, however it was installed). `always` re-applies every update (fixes drift); `ensure`
is **install-if-missing** on update (creates an absent target, never overwrites
a present one — an `exec` `ensure` needs a `check` to be applied on update,
without one it's skipped); `manual` is skipped by automated flows — run it only
when explicitly invoked; `install` runs once (on install, not update).

```toml
# copy: deploy a file
[[step]]
copy     = "assets/x.conf"   # source, relative to the temper-home
to       = "~/.config/x"     # target (single path; ~ expands)
template = false             # true → substitute {{ var "X" }} / {{ which "x" }} / {{ env "X" }} / {{ brew_prefix }}
seed     = false             # true → create-once if absent, then hands-off, excluded from drift
mode     = "0600"            # optional octal file mode

# block: ensure a marker-delimited region in a user file (idempotent)
[[step]]
block  = "assets/snippet"    # content to place inside the markers
in     = "~/.ssh/config"     # the user-owned file
marker = "ssh-include"       # marker label

# setkey: set ONE key in a structured store, preserving siblings.
# Exactly one `setkey` table per step. One worked example per backend:
[[step]]
setkey = { backend = "json",     file = "~/.config/app.json",        key = "ui.theme",           value = "dark" }
[[step]]
setkey = { backend = "toml",     file = "~/.config/starship.toml",   key = "add_newline",        value = false }
[[step]]
setkey = { backend = "ini",      file = "~/.local/share/x.desktop",  key = "Desktop Entry.Icon", value = "myicon" }
[[step]]
setkey = { backend = "defaults", file = "com.apple.dock",            key = "autohide",           value = true }        # macOS
[[step]]
setkey = { backend = "dconf",    key = "/org/gnome/desktop/interface/color-scheme", value = "prefer-dark" }             # Linux; NO file
#
#   backend: "json" | "toml" | "ini" (a.k.a. ".desktop") | "defaults" (macOS) | "dconf" (Linux)
#   file:    REQUIRED for json/toml/ini (target file) and defaults (a domain like
#            "com.apple.dock", or a plist path). OMIT for dconf — the key is the path.
#   key:     json/toml → dotted path ("ui.theme"); ini → "Section.Key"; defaults →
#            the key name; dconf → the ABSOLUTE dconf path ("/org/.../color-scheme").
#   value:   STATIC scalar/array — {{ … }} is NOT rendered here. Write a NATIVE TOML
#            value (string/int/bool/array); temper renders it for the backend. For
#            dconf, DO NOT pre-quote: value = "prefer-dark" becomes GVariant
#            'prefer-dark'; value = true → true; value = 42 → 42; value = ["a","b"]
#            → ['a','b']. (GVariant types beyond scalar/string-array — uint32,
#            tuples — aren't rendered; use `exec` for those.)
#   append:  true → list-union into an array-valued key (json/toml only).
#
#   File backends CREATE the file + parent dirs if absent (drift shows `missing`
#   until first apply); json/toml refuse a file whose root isn't an object/table.
#   defaults/dconf report `unavailable` in drift (and skip in apply) when their CLI
#   is absent (e.g. dconf on a Mac) — degrade, never abort. dconf writes are
#   journaled/undoable; defaults writes are not.

# exec: run a user script (the escape hatch)
[[step]]
exec    = "assets/setup.sh"  # runs via sh AS YOU (not root); cwd = temper-home; env TEMPER_HOME/MACHINE/OS
check   = "assets/check.sh"  # optional drift-hook: exit 0 = in sync; gates re-run
sudo    = false              # deprecated no-op — escalate inside the script with sudo per-command
secrets = ["ACOUSTID_KEY"]   # env vars passed through to the script. A live apply
                             # errors if a declared secret is missing; a read-only
                             # `drift`/`install --dry-run` DEGRADES that step to
                             # status-only ("unavailable — secret …") — never aborts.
# exec is NOT journaled (not reversible by undo).

# profile: install a macOS .mobileconfig (opens System Settings; manual)
[[step]]
profile = "assets/x.mobileconfig"   # drift is status-only ("manual")

# sysfile: write one ROOT-owned system file (the clean /etc path)
[[step]]
sysfile = "assets/1password.policy"           # source in the folder
to      = "/etc/1password/custom_allowed_browsers"
mode    = "0755"
owner   = "root"                              # enforced via `sudo install`
group   = "root"                              # drift compares content+mode+owner
```

## Assertions (`[[assert]]`) — drift-only, one check each

```toml
[[assert]] absent = "~/.zshrc.local"                       # must NOT exist
[[assert]] contains_line = { file = "~/.zshrc", line = "source ~/.zshrc.image" }
[[assert]] mode = { path = "/etc/x", mode = "0644" }       # octal file mode
[[assert]] executable_resolves = "git"                     # on PATH
[[assert]] not_member = { group = "onepassword" }          # user NOT in group
[[assert]] shell = "/bin/zsh"                              # login shell equals
[[assert]] json_semantic = { file = "~/deployed.json", against = "reference.json" }  # against: relative to the temper-home
# each also accepts os = "mac"|"linux"
```

## Not in the schema (rejected by `deny_unknown_fields`)

Unknown fields are a parse error. `when` / `needs` (step presence-gating) and
`owner` / `group` (on a `sysfile` step) **are** valid — they're documented above.
A few names from older design notes are **not** fields and will error: `dict_add`
/ `domain` on `setkey`, `mode_lifecycle`, and `owner` as an *assert* check (owner
is a `sysfile` field, not an assertion). When in doubt, the parser is the
authority — an unknown field names itself in the error.
