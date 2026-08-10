# temper — Manifest Schema (as implemented)

This documents the **actual** parsed schema (the serde structs in
`crates/temper-core/src/manifest.rs`). Unknown fields are rejected
(`deny_unknown_fields`), so anything not listed here is a parse error.

A temper-home folder holds `temper.toml`, `apps/<name>.toml` bundles, and the
asset files they reference.

## Where a declaration lives decides what can happen to it

Read this before adding anything, because it answers most "can temper do X?"
questions mechanically. Every declaration sits at one of two scopes, and the
scope — not the category — decides which verbs may touch it.

**Fleet / group scope** — `[brew].trust` and `[ignore]` (fleet: *every* machine,
ungated), and everything in an `apps/<name>.toml` bundle (group: gated by the
bundle's `os`/`role`, which is how a declaration says which machines it
describes). It describes a *group* a machine belongs to (its `os`,
its `role`, the bundles it composes). Verbs: **drift and install. Conform.**
`reconcile` will never add to it or remove from it, because doing that from one
machine silently changes every other machine in the group.

**Per-machine scope** — the `[[machine]]` block and the files it names (its
`brewfile`, its snapshots). It describes one box. Verbs: **drift, install, prune,
and reconcile in both directions** — this is the part you are meant to edit from
the machine itself.

Removal works at both scopes; what differs is *who edits the declaration*. At
fleet scope you edit the shared spec and commit — then every machine's `prune`
enacts it, because the item is now declared nowhere. At machine scope
`reconcile` does the same edit for that one box. So `prune` is the enactment
mechanism either way, and it only ever removes what **neither** scope declares.

Consequently: **a category you want to control per machine must be declared at
machine scope.** Declaring it in a bundle means every machine in the group gets
it and no single machine may opt out — which is correct, and is the point of a
bundle. Both scopes exist for every category for exactly this reason; see
ARCHITECTURE, "Scope decides the verb set" and "The feature interface".

## `temper.toml`

```toml
temper_version = "1.42.0"   # optional/MANAGED: the temper that last WROTE this file.
                            #   temper stamps it on every write (monotonic — never
                            #   lowered). On load, a stamp NEWER than the running temper
                            #   means the folder came from a newer temper — see [update].
                            #   Hand-editing is pointless (temper rewrites it).

[vars]                      # optional: GLOBAL template variables, {{ var "X" }}
EDITOR = "hx"

[ignore]                    # optional: installed pkgs drift/prune must not flag
brew    = []
cask    = []
flatpak = []
mas     = []
vscode  = []
tap     = []
gnome_extensions = []       # user-installed GNOME extension UUIDs not to flag
                            #   (old name `gext` still parses)
rpm_ostree = []             # layered rpms not to flag as extras
flatpak_remote = []         # flatpak remote NAMES not to flag as extras

[ui]                        # optional; how temper draws its status markers
icons = "unicode"           # "unicode" (default) | "nerd"
                            #   nerd uses Material-Design Nerd Font glyphs, which are
                            #   Private Use Area: crisp with a patched font, an EMPTY BOX
                            #   without one — hence the safe default. `TEMPER_ICONS=nerd`
                            #   (or =unicode) overrides per terminal, because font coverage
                            #   belongs to the terminal, not to the spec or the machine.

[brew]                      # optional
trust = ["ublue-os/tap"]   # third-party taps to `brew trust` before converge/upgrade.
                            #   drift/reconcile/prune check this both ways vs `brew trust --json`:
                            #   declared-but-untrusted (install/update re-trusts) and
                            #   trusted-but-undeclared (reconcile absorbs / prune untrusts;
                            #   [ignore].tap silences)

[eq_import]                 # optional; `temper eq-import` fetches speaker profiles
repo = "https://github.com/…/pipewire-speaker-profiles"
dest = "assets/speaker-eq"  # default; each <x>.calibrated.conf lands as <x>.conf

[git]                       # optional; convenience for a GIT-backed home (no-op
                            #   on a non-git folder). Set via `temper configure set git.*`.
remind      = true          # hint whenever any command finds the folder dirty (unless auto_commit)
auto_commit = false         # commit right after reconcile/dump/snapshot/eq-import (auto message)
auto_push   = false         # …and push
auto_pull   = true          # `git pull` before a run; warn (never abort) if it can't
auto_rebase = false         # when auto_pull runs, `--rebase` instead of `--ff-only`
                            #   (so a pull still lands when local has un-pushed commits)
                            # A `[machine.git]` block wholly overrides this for that machine.

[update]                    # optional; what to do when this folder was written by a
                            #   NEWER temper than the one running (via temper_version).
mode = "prompt"             # off | warn | prompt (default) | auto
                            #   off:    ignore the stamp — a skew errors plainly
                            #   warn:   report the skew + print the upgrade command
                            #   prompt: also OFFER to run it on a Homebrew install (mac/Linux)
                            #   auto:   run `brew update && brew upgrade temper -y` unattended
                            # If the newer temper added a field this one can't parse, the load
                            #   fails and (unless off) becomes an upgrade offer instead of a
                            #   cryptic `unknown field` error. If it still parses, temper nudges
                            #   and carries on. A taken upgrade re-runs your command.
                            # Set via `temper configure set update.mode <…>`.

[[machine]]
name     = "chronos"        # required; resolved against `hostname -s`
os       = "mac"            # required; "mac" | "linux"
role     = "desktop"        # optional; "desktop" | "server"
apps     = ["shell", "ssh"] # bundle names in apps/
packages = ["cask \"raycast\""]  # optional loose Brewfile-grammar tokens. Machine
                            #   scope, so `reconcile` both absorbs into and drops
                            #   from this list.
gnome_extensions = ["tilingshell@ferrarodomenico.com"]  # optional; for THIS
                            #   machine, unioned with the composed bundles' lists. The
                            #   machine-scoped counterpart of a bundle's `extensions` —
                            #   `reconcile` absorbs an undeclared extension here, because
                            #   a bundle's list is shared by every machine composing it.
brewfile = "brewfiles/chronos"   # optional; a Brewfile whose lines join the set
flatpak_remotes = ["vendor https://example.com/vendor.flatpakrepo"]
                            # optional; remotes THIS machine adds, as "<name> <url>".
                            #   The name is the identity; the url can drift.
rpm_ostree = ["proton-vpn"] # optional; rpms THIS machine layers, unioned with its
                            #   bundles' lists. Machine scope, so reconcile absorbs
                            #   and drops here.
brew_trust = ["me/tap"]     # optional; taps THIS machine trusts, unioned with
                            #   [brew].trust. Machine scope, so `reconcile` both
                            #   absorbs into and drops from it — the fleet list
                            #   is a group decision one machine never edits.

[machine.ignore]            # optional; extras to silence on THIS machine only,
flatpak = ["org.example"]   #   unioned per-manager with the fleet [ignore] and
                            #   with any composed bundle's. Same shape as
                            #   [ignore]; `reconcile` writes here when you answer
                            #   `i` to an extra, for any manager.

[machine.vars]              # optional; per-machine vars, merged OVER [vars]
BREW_PREFIX = "/home/linuxbrew/.linuxbrew"   # e.g. override a Mac-valued global

[[machine.dconf]]           # optional; whole-desktop dconf snapshots (Linux).
                            #   Captured by `snapshot-gnome`, applied by `restore-gnome`:
                            #   the verbs are named for the DESKTOP because a future KDE or
                            #   macOS equivalent won't be dconf at all (KDE uses INI files,
                            #   macOS `defaults`), while this field is named for the
                            #   MECHANISM it actually reads.
path  = "/org/gnome/shell/"                 # subtree to dump/load (trailing /)
file  = "assets/gnome/shell.chronos.dconf"  # snapshot writes here; restore reads
strip = ["monitors/", "last-selected"]      # NOISE only: key substrings that would
                                            #   corrupt a capture/restore round-trip.
                                            #   You do NOT list keys a `setkey` step
                                            #   owns — temper derives those and never
                                            #   captures them. Applied to
                                            #   BOTH sides of a drift compare, so a
                                            #   stripped key never reads as drift
label = "shell"                             # optional; name used in drift/reconcile
                                            #   output instead of the raw path
```

**Per-extension prompts come free; splitting is optional.** drift/reconcile group
by the sections the dump itself defines, and dconf names sections by path
*relative to the dumped root* — so a snapshot rooted at `/org/gnome/shell/`
already yields `extensions/caffeine`, `extensions/dash-to-dock`, … and one
reconcile prompt each, with no GNOME knowledge in the tool. You do **not** need
to re-root a snapshot to get that.

`[[machine.dconf]]` is still repeatable, and splitting is worth it when you want
separate *files* (to diff or restore one area alone) or a distinct `label` per
area. It does not change the prompt granularity. The one genuinely coarse ask is
the root section (`enabled-extensions`, `favorite-apps`, …), and splitting
doesn't help that either — those keys live at the root wherever you root it.

Drift on a snapshot is key-level and reported in the same vocabulary as
packages: `missing` (in the file, not on the machine), `extra` (on the machine,
not captured), `changed` (both, differing), plus `never captured` when the file
doesn't exist yet. `reconcile` absorbs per section/key (spec←machine); `restore-gnome` pushes the file
back out (spec→machine). Both directions are named in drift's Next steps.

> **`missing` means "at the schema default", not "unset".** dconf stores only
> non-default values, so a key absent from a dump is one the machine holds at its
> default — itself a value. Absorbing such a key therefore *removes* it from the
> snapshot, which is exactly right after you deliberately reset something and
> re-tuned a few keys, and exactly wrong on a machine where `restore-gnome` has never
> run (there, the key is not "reset" — it was simply never applied). temper
> cannot tell those apart; you can. Interactive `reconcile` defaults to keeping
> it, and `--current-state-wins` groups removals by section in the preview so a
> large drop is visible before you confirm.

> **TOML ordering:** `[machine.vars]` and each `[[machine.dconf]]` bind to the
> **preceding** `[[machine]]` — they must sit between that header and the next
> `[[machine]]`. Put them right under the machine they belong to.

Template vars resolve as global `[vars]` overlaid by a machine's own
`[machine.vars]` (per-machine wins). For the common Homebrew-prefix split, prefer
the live function `{{ brew_prefix }}` (§templating) over declaring `BREW_PREFIX`
per machine — it resolves `brew --prefix` on the box, so one template works on
both OSes.

Effective package set for a machine = union(each app's `packages`(+`_mac`/
`_linux`), the machine `packages`, and the machine `brewfile` lines). A declared
`brewfile` that doesn't exist yet contributes nothing rather than erroring — that
is the seed case (`init`). `[ignore]`
is **not** subtracted here — it only stops installed-but-undeclared packages from
being flagged as extras by `drift`/`prune`. A package that is both declared and
ignored is still installed (declaration wins). `[ignore].tap` does double duty: it
also silences a *trusted-but-undeclared* tap so `drift`/`reconcile` stop offering
it as `[brew].trust` (see `[brew].trust` above).

> **A manager is only probed if you declare at least one of its packages.** This
> is load-bearing, not an accident: with no `vscode "…"` entry anywhere, temper
> never runs `code --list-extensions`, so a VS Code Settings Sync setup stays the
> sole registrar of your extensions and nothing is ever reported as an extra.
> The same holds for `flatpak` and `mas`. Declaring *one* opts that manager in —
> thereafter its installed-but-undeclared entries are reported, with
> `[ignore].<manager>` as the escape hatch. (brew-family is the exception: any
> declaration at all enables the dependency-aware brew extras computation.)
>
> **`gext` follows the same rule**, and reports both directions once opted in.
> Only **user-scope** extensions count as extras: system ones ship with the image,
> and drift reports image-baked items status-only, so a Bazzite box doesn't list
> seventeen you never chose. Silence one with `[ignore].gext`.
>
> `reconcile` handles **both** directions against the machine's own
> `[[machine]].extensions` list: it offers to declare an installed extension no
> bundle claims, and to drop one this machine declares but no longer has. A
> bundle's `extensions` is shared by every machine composing it, so neither edit
> ever touches one — a bundle-declared extension that is absent stays a hand
> edit, and drift names the file.
>
> Both directions are computed only where `gnome-extensions` actually ran and
> answered. On a host that cannot enumerate, "declared but absent" and "I cannot
> tell" are the same observation, and acting on the second would delete a list
> the machine simply could not see.

## `apps/<name>.toml` (a bundle)

```toml
os             = "linux"             # optional bundle-level gate: skip this bundle's
role           = "desktop"           #   `packages`, `gnome_extensions` and `rpm_ostree`
                                     #   unless the machine's os/role match. A machine
                                     #   that declares NO role fails a role gate closed:
                                     #   a bundle naming a role describes a group, and a
                                     #   machine naming none is not in it.
                                     #   It does NOT gate the bundle's [[step]]s — those
                                     #   gate on their own os/role/when/needs (a server
                                     #   composing this still runs its file/key steps).
brew_trust     = ["vendor/tap"]      # taps this bundle needs trusted — GROUP scope,
                                     #   gated with the bundle, which the fleet
                                     #   [brew].trust cannot be. A mac-only cask tap
                                     #   belongs in an os = "mac" bundle.
flatpak_remotes = ["vendor https://example.com/vendor.flatpakrepo"]
                                     # remotes this bundle's apps come from —
                                     #   GROUP scope, gated with the bundle.
[ignore]                             # extras this bundle knows aren't worth
flatpak = ["org.example.Baseline"]   #   reporting (the OS baseline it brings).

packages       = ["brew \"jq\""]     # Brewfile-grammar tokens (all-OS)
packages_mac   = []                  # mac-only
packages_linux = []                  # linux-only
gnome_extensions = ["ext@uuid"]      # GNOME extensions (Linux) — os/role-gated
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

> This section is the grammar of each primitive in isolation. For which
> *combination* solves a given problem shape, see **PATTERNS.md**.

Common: `os = "mac"|"linux"` and `role = "desktop"|"server"` (skip on a
non-matching OS/role; an unknown os/role errors at load); `run = "always"|
"install"|"ensure"|"manual"` (lifecycle; default: copy/block/setkey → always,
exec/seed/profile → install — a profile's apply is a GUI window, so `update`
leaves it alone rather than re-opening System Settings every run). Presence gates
(gate config on **reality**):
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
[[step]]  # template = true → render {{ … }} in the value at apply time (any backend)
setkey = { backend = "json", file = "~/.config/x.json", key = "bin", value = "{{ which \"ghostty\" }}", template = true }
#
#   backend: "json" | "toml" | "ini" (a.k.a. ".desktop") | "defaults" (macOS) | "dconf" (Linux)
#   dconf doubles are compared NUMERICALLY, not as text: dconf prints them with
#     GVariant's %.17g (0.46999999999999997) while the manifest says 0.47 — the
#     same f64 spelled two ways. A whole-numbered double is written with its
#     `.0` so the key keeps its double type. So `value = 1.0` and `value = 0.47`
#     converge and stay converged; there is no value you must avoid declaring.
#   file:    REQUIRED for json/toml/ini (target file) and defaults (a domain like
#            "com.apple.dock", or a plist path). OMIT for dconf — the key is the path.
#   key:     json/toml → dotted path ("ui.theme"); ini → "Section.Key"; defaults →
#            the key name; dconf → the ABSOLUTE dconf path ("/org/.../color-scheme").
#   value:   scalar/array (a table too, on json/toml). {{ … }} is literal here
#            unless `template = true` (below). Write a NATIVE TOML
#            value (string/int/bool/array); temper renders it for the backend. For
#            dconf, DO NOT pre-quote: value = "prefer-dark" becomes GVariant
#            'prefer-dark'; value = true → true; value = 42 → 42; value = ["a","b"]
#            → ['a','b']. (GVariant types beyond scalar/string-array — uint32,
#            tuples — aren't rendered; use `exec` for those.)
#   append:  true → idempotent list-union into an array key. json/toml, and
#            dconf for a GVariant `as` list (the custom-keybindings /
#            enabled-extensions / favorite-apps shape). On dconf the value is a
#            string (one member) or array-of-strings (each a member); drift is
#            subset ("the array contains the declared member(s)"). NOT ini/defaults.
#   template: true → render {{ which "x" }} / {{ brew_prefix }} / {{ env "X" }} /
#            {{ var "X" }} in the value's string leaves at apply time (like copy's
#            `template`; works on ALL backends). Default false. Drift re-renders +
#            compares, so a resolved path is not permanent false drift. Render
#            errors if a probe can't resolve — pair with a `when` gate when the
#            target may be absent. Use for a value temper must resolve per-machine,
#            e.g. a dconf keybinding command = "{{ which \"1password\" }}".
#
#   File backends CREATE the file + parent dirs if absent (drift shows `missing`
#   until first apply); json/toml refuse a file whose root isn't an object/table.
#   json is JSONC-aware: it reads targets with // and /* */ comments + trailing
#   commas (e.g. opencode.jsonc, VS Code settings.json), and writes
#   comment/format-preserving — only the changed key is spliced, so sibling keys,
#   comments, and layout survive. A deep dotted key creates the intermediate
#   objects (mcp.searxng.type builds mcp + searxng). json/toml also accept an
#   object/table value, to set a whole block in one step.
#   defaults/dconf report `unavailable` in drift (and skip in apply) when their CLI
#   is absent (e.g. dconf on a Mac) — degrade, never abort. dconf writes are
#   journaled/undoable; defaults writes are not.

# exec: run a user script (the escape hatch)
[[step]]
exec    = "assets/setup.sh"  # runs via sh AS YOU (not root); cwd = temper-home; env TEMPER_HOME/MACHINE/OS
check   = "assets/check.sh"  # optional drift-hook: exit 0 = in sync; gates re-run
sudo    = false              # "this script escalates internally" — it still runs AS YOU
                             #   (escalate per-command inside it: `sudo cp …`). The flag
                             #   only tells temper to include the step in the ONE up-front
                             #   password ask, so the script never stops mid-run to prompt
                             #   (a prompt buried in a list of results is easy to miss, and
                             #   the keyboard may not be there in 20 minutes). Set it on any
                             #   script that calls sudo/pkexec. `sysfile` steps are included
                             #   automatically — temper escalates for those itself.
                             #   LIMIT: this can only save a prompt where sudo caches
                             #   credentials per terminal (`timestamp_type=tty`) or
                             #   globally. Where they are cached per PARENT PROCESS
                             #   (`ppid` — the effective default on some Fedora builds,
                             #   whatever the man page says), a script's own `sudo` has a
                             #   different parent and authenticates again no matter what
                             #   temper did; temper detects that and says so instead of
                             #   promising otherwise. `sysfile` is unaffected — temper is
                             #   the parent there. Also asked only when root is REALLY
                             #   needed: an in-sync `sysfile`, or an `exec` whose `check`
                             #   passes, costs no prompt.
secrets = ["ACOUSTID_KEY"]   # env vars passed through to the script. A live apply
                             # errors if a declared secret is missing; a read-only
                             # `drift`/`install --dry-run` DEGRADES that step to
                             # status-only ("unavailable — secret …") — never aborts.
# exec is NOT journaled (not reversible by undo).
# exec output is QUIET by default (like `brew upgrade --quiet`): its stdout/stderr
# is captured and stays hidden on success, surfaced only if the script fails (or
# always under `--verbose`). So an idempotent script's chatter never masquerades
# as temper's own reporting. To keep an idempotent `exec` from re-running (and
# re-printing) every `update`, give it a `check` — a passing check skips the run
# entirely.

# profile: install a macOS .mobileconfig (apply opens System Settings to approve).
# Installing needs the GUI; READING what is installed needs neither MDM nor root, so
# drift is checked: temper matches the file's top-level PayloadIdentifier against
# `system_profiler SPConfigurationProfileDataType`, across BOTH the user and device
# scopes.
#   missing      → the identifier isn't installed. `temper install` opens it.
#   drifted      → installed, but the source file changed since temper applied it,
#                  so the installed copy is stale (tracked by a content stamp).
#   in sync      → installed, and either unchanged since temper applied it or never
#                  applied by temper at all — a hand-installed profile is present,
#                  and temper won't call it stale on no evidence.
#   unavailable  → can't be evaluated: not a Mac, or a SIGNED/encrypted profile,
#                  which is CMS-wrapped rather than a readable plist. Degrades to
#                  status-only; apply then falls back to the content stamp alone.
# `update` skips profiles entirely (see the `run` default above), so a profile you
# decline is reported by drift, never re-offered on the routine upgrade path.
[[step]]
profile = "assets/x.mobileconfig"

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

# Any assertion may add:
#   severity = "notice"   # default "drift". A NOTICE reports a STATE, not a
#                         #   defect: it shows as a cyan ℹ line, stays OUT of the
#                         #   out-of-sync count, and gets no remediation.
#   message  = "…"        # human text shown instead of the generic result — say
#                         #   what to do, not what the predicate was.
#
# Use it when a failing check means "something is pending", not "something is
# wrong". A staged ostree deployment is the archetype: the machine matches the
# spec, an update is simply waiting for a reboot, and no verb can clear it — so a
# red ✗ that survives every `install` is both wrong and unactionable:
#
#   [[assert]]
#   absent   = "/run/ostree/staged-deployment"
#   severity = "notice"
#   message  = "a system update is staged — reboot to apply it"
[[assert]] not_member = { group = "onepassword" }          # user NOT in group
[[assert]] shell = "/bin/zsh"                              # login shell name (matches by basename: /usr/bin/zsh ok)
[[assert]] json_semantic = { file = "~/deployed.json", against = "reference.json" }  # against: relative to the temper-home
# each also accepts os = "mac"|"linux" and role = "desktop"|"server" (skip on mismatch)
```

## Not in the schema (rejected by `deny_unknown_fields`)

Unknown fields are a parse error. `when` / `needs` (step presence-gating) and
`owner` / `group` (on a `sysfile` step) **are** valid — they're documented above.
A few names from older design notes are **not** fields and will error: `dict_add`
/ `domain` on `setkey`, `mode_lifecycle`, and `owner` as an *assert* check (owner
is a `sysfile` field, not an assertion). When in doubt, the parser is the
authority — an unknown field names itself in the error.
