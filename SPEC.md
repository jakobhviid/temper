# temper — Manifest Schema (as implemented)

This documents the **actual** parsed schema (the serde structs in
`crates/temper-core/src/manifest.rs`). Unknown fields are rejected
(`deny_unknown_fields`), so anything not listed here is a parse error.

A temper-home folder holds `temper.toml`, `apps/<name>.toml` bundles, and the
asset files they reference.

## `temper.toml`

```toml
[vars]                      # optional: template variables, used by {{ var "X" }}
BREW_PREFIX = "/opt/homebrew"

[ignore]                    # optional: installed pkgs drift/prune must not flag
brew    = []
cask    = []
flatpak = []
mas     = []
vscode  = []
tap     = []

[brew]                      # optional
trust = ["ublue-os/tap"]   # third-party taps to `brew trust` before converge/upgrade

[[machine]]
name     = "chronos"        # required; resolved against `hostname -s`
os       = "mac"            # required; "mac" | "linux"
role     = "desktop"        # optional; "desktop" | "server"
apps     = ["shell", "ssh"] # bundle names in apps/
packages = ["cask \"raycast\""]  # optional loose Brewfile-grammar tokens
brewfile = "brewfiles/chronos"   # optional; a Brewfile whose lines join the set
```

Effective package set for a machine = union(each app's `packages`(+`_mac`/
`_linux`), the machine `packages`, and the machine `brewfile` lines) − `[ignore]`.

## `apps/<name>.toml` (a bundle)

```toml
packages       = ["brew \"jq\""]     # Brewfile-grammar tokens (all-OS)
packages_mac   = []                  # mac-only
packages_linux = []                  # linux-only
extensions     = ["ext@uuid"]        # GNOME extensions (gext; Linux)
rpm            = ["proton-vpn-gnome-desktop"]  # rpm-ostree layered (Linux)

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
exec/seed → install). `manual` steps are skipped by automated flows
(install/update) — run them only when explicitly invoked. **Note: `ensure`
currently behaves like `always`.**

```toml
# copy: deploy a file
[[step]]
copy     = "assets/x.conf"   # source, relative to the temper-home
to       = "~/.config/x"     # target (single path; ~ expands)
template = false             # true → substitute {{ var "X" }} / {{ which "x" }} / {{ env "X" }}
seed     = false             # true → create-once if absent, then hands-off, excluded from drift
mode     = "0600"            # optional octal file mode

# block: ensure a marker-delimited region in a user file (idempotent)
[[step]]
block  = "assets/snippet"    # content to place inside the markers
in     = "~/.ssh/config"     # the user-owned file
marker = "ssh-include"       # marker label

# setkey: set a key in a structured store, preserving siblings
[[step]]
setkey = { backend = "json", file = "~/.claude/settings.json", key = "env.X", value = "0", append = false }
#   backend: "json" | "toml" | "ini" | "defaults" (macOS) | "dconf" (Linux)
#   file:    file backends → the file; defaults → a domain or plist path; dconf → key is absolute
#   key:     dotted path (json/toml) | "Section.Key" (ini) | absolute dconf path
#   value:   scalar or array — STATIC only ({{ … }} is NOT rendered for setkey)
#   append:  true → list-union into an array-valued key

# exec: run a user script (the escape hatch)
[[step]]
exec    = "assets/setup.sh"  # runs via sh, cwd = temper-home, with TEMPER_HOME/MACHINE/OS
check   = "assets/check.sh"  # optional drift-hook: exit 0 = in sync; gates re-run
sudo    = false              # deprecated no-op — escalate inside the script with sudo per-command
secrets = ["ACOUSTID_KEY"]   # env vars that must be set; passed through (loud error if missing)
# exec is NOT journaled (not reversible by undo).

# profile: install a macOS .mobileconfig (opens System Settings; manual)
[[step]]
profile = "assets/x.mobileconfig"   # drift is status-only ("manual")
```

## Assertions (`[[assert]]`) — drift-only, one check each

```toml
[[assert]] absent = "~/.zshrc.local"                       # must NOT exist
[[assert]] contains_line = { file = "~/.zshrc", line = "source ~/.zshrc.image" }
[[assert]] mode = { path = "/etc/x", mode = "0644" }       # octal file mode
[[assert]] executable_resolves = "git"                     # on PATH
[[assert]] not_member = { group = "onepassword" }          # user NOT in group
[[assert]] shell = "/bin/zsh"                              # login shell equals
[[assert]] json_semantic = { file = "~/deployed.json", against = "reference.json" }
# each also accepts os = "mac"|"linux"
```

## Not in the schema (rejected by `deny_unknown_fields`)

These appear in older design notes but are **not** implemented; using them is a
parse error: `when` / `needs` (presence-gating), `owner` (assert), `dict_add` /
`domain` (setkey), `mode_lifecycle`. See the README status table for the
built-vs-designed boundary.
