# WORKFLOWS.md — how you operate temper

The day-to-day loops, not the schema. `SPEC.md` is *what you write*; this is
*what you run* and when. Compiled into `--llm` so an agent can operate a temper
folder, not just author one.

> **Status: confirmed against Jakob's habits** (cadence, absorb-direction, fleet
> model, restore) and grounded in ReinstallScripts. Each loop maps a RIS recipe
> onto temper's verbs.

The mental model: a machine has a **declared spec** (the folder) and a **live
state**. Every loop either converges the machine toward the spec
(**machine→spec**) or absorbs the machine's reality into the spec
(**spec←machine**). `drift` shows the gap and names both directions.

**How Jakob works it (the short version):** `drift` is the hub — run it, read the
**Next steps**, run the command it names. Absorbing an ad-hoc change goes through
`reconcile` (per-item), or `reconcile --csw` when the machine is simply right.
The fleet is **authored in one
place** (this folder, in git) and **run per-machine** (locally, or over ssh — see
§Fleet). `restore-dconf` is used both when bringing a desktop back up *and* mid-life to
snap GNOME/Ptyxis back to the known-good snapshot.

Throughout, every verb runs against **this machine** (resolved by hostname); a
machine name is **optional** — pass one only to target or preview another (a
live `install <name>` then asks to confirm). The examples below omit it, matching
what `drift`'s Next steps print.

---

## 0. Put a machine in the folder for the first time

**When:** the box exists and is set up the way you like, but the folder has never
heard of it. (If the machine is *already* declared, skip to §1.)

```sh
temper init            # name inferred from hostname; --role to skip the ask
```

`init` adds a `[[machine]]` block (creating `temper.toml` if the folder has
none), wires up `brewfiles/<name>`, and then seeds it from the machine's live
state — it is `reconcile --current-state-wins --include-trust` under the hood, so
you get dependency-aware brew extras, `[ignore]`
respected, canonical ordering, a preview, and an undo. It refuses to touch a
machine that's already declared, pointing you at `reconcile` instead.

Seeding is the one place the probe opt-in is lifted. Everywhere else a manager
is only probed once you declare one of its packages, which is what stops a spec
that declares nothing from reporting the whole machine; `init` is the verb whose
job is to *find* what is here, so it enumerates every manager whose tool is
present. **VS Code is the exception even here** — Settings Sync stays the sole
registrar of your extensions, and adopting them wholesale is an ownership temper
does not want.

`init` and `setup` are different jobs: **`setup` = which folder do I use**
(records a pointer), **`init` = put this machine in the folder**.

## 1. Bring up a machine (fresh install / reinstall)

**When:** a new box, or a reinstalled one, that already has brew + temper
(bootstrap is out of scope — a separate script).

```sh
temper setup ~/my-spec # once: record where your folder is (optional)
temper install       # full converge: packages + all config + one-time setup
temper restore-dconf # Linux desktop only: load the dconf snapshot back
```

`install` adds missing packages (brew/flatpak/mas/gext/rpm), applies every
config step, and runs one-time setup. `manual` steps (e.g. `speaker-eq`) are
skipped — run them by hand. On a fresh desktop, `restore-dconf` reloads GNOME/Ptyxis
state from the snapshot (it's a separate, confirm-gated verb because it clobbers
live tweaks). *(RIS: `bootstrap.sh` → `just install` → `just gnome-restore`.)*

`install`/`update` are **quiet by default** — the underlying tools are hushed
(`brew bundle`/`brew upgrade` `--quiet`, mas's Spotlight-reindex noise muted, and
already-installed App Store apps skipped) so you see installs, changes, warnings,
and errors, not a wall of "already OK". Quiet is not silent: the package phase
shows a **spinner naming the package being installed right now**
(`⠹ Installing llvm`, and `⠹ Installing Xcode 3/49` for the one-at-a-time App
Store apps), so a multi-GB download reads as progress instead of a hang. The
tool's own output is captured and **replayed in full if the converge fails**;
Homebrew's warnings (with their bodies, which carry the remedy) print even on
success. Your own **`exec` scripts are hushed the same way**: their output is
captured and stays hidden on success (so an idempotent script's "nothing to do"
chatter can't masquerade as temper's own verdict), and is surfaced only if the
script fails. While a script runs, the progress region **steps aside**: an `exec`
is arbitrary code, and `sudo`/polkit/PAM write their prompts straight to the
terminal where a redrawing spinner would fuse itself onto them ("Place your finger
on the fingerprint reader" landing mid-progress-line, then erased by the next
tick). So the region clears for the script's duration — a prompt gets a clean line
of its own and stays there. A script still running after a few seconds names itself
in the phase's own shape, so a long one isn't a silent terminal either and reads as
one item finishing:

```text
  ⋯ 1password  exec  assets/scripts/1password-setup.sh
  ✓ 1password  exec  assets/scripts/1password-setup.sh
```

A script that finishes quickly prints only the `✓`. The waiting line is deliberately
**not** shaped like a row of the list: the leftmost glyph there is a status column
(`✓`/`⚠`/`✗`) that the eye scans for exceptions, so a progress marker sitting in it
would read as "this step has a problem". It is indented as a detail, dimmed, and says
what it is — and the `✓` that resolves it carries the elapsed time, so the pause you
just sat through is accounted for:

```text
  ✓ ptyxis             exec    assets/scripts/ptyxis-load.sh
      … still working: assets/scripts/1password-setup.sh
  ✓ 1password          exec    assets/scripts/1password-setup.sh    12s
```

Rows are **column-aligned** — app, kind, then target — so a run reads down the
page instead of zig-zagging with the app-name length:

```text
  ✓ shell              exec    assets/scripts/retire-sesh-tap.sh
  ✓ zsh                copy    ~/.zshrc
  ✓ desktop-overrides  copy    ~/Bibliotek/Programstøtte/overrides.conf
  ✓ opencode           setkey  ~/.config/opencode/opencode.jsonc:share
```

The widths come from the run's own plan (known before the first line prints), and
`drift` sizes its table the same way, so the two views line up with each other. On
a narrow terminal the target is elided from the left (`…/retire-sesh-tap.sh` still
tells you which step); redirected output is never shortened, because a log is
evidence.

Pass **`-v`/`--verbose`** (a global flag, like `--json`) to stream
every tool's — and every `exec`'s — full output instead of the spinner when
debugging. (An idempotent `exec` that re-runs each `update` should carry a `check`
hook so it's skipped, not just hushed, when already in sync.)

**One password per run.** Two things in a run can need root: some casks install
through a system installer (`mactex`, `zoom`, `dotnet-sdk`, …), and your own steps —
a `sysfile` (temper places it with `sudo install`) or an `exec` script that declares
`sudo = true` because it calls `sudo` internally. temper collects all of them before
anything starts and asks **once**, at the keyboard, naming what needs it. Declaring
`sudo = true` is what keeps a script from stopping in the middle of the run to
prompt — a password or fingerprint request buried in a list of results is easy to
miss, and the keyboard may not still be there twenty minutes in. temper asks only
for root it will **really** need: an in-sync `sysfile`, or an `exec` whose `check`
passes, costs no prompt at all, because that work won't happen.

> **Where one prompt isn't possible.** Reusing a credential across processes depends
> on how this machine's sudo caches it. With `timestamp_type=tty` (sudo's documented
> default) or global caching, temper's single up-front prompt covers everything. Where
> credentials are cached per **parent process** (`ppid` — the effective default on
> some Fedora builds regardless of what `man 5 sudoers` states), a script's own `sudo`
> has a different parent and must authenticate again; no amount of asking early can
> change that. temper measures this rather than assuming, and says so plainly instead
> of promising a quiet run it can't deliver — `Defaults timestamp_type=tty` in sudoers
> is the fix. Steps temper escalates *itself* (`sysfile`) are unaffected, since temper
> is the parent. Homebrew asks per cask, and
because sudo's timestamp expires (5 min by default) during the multi-GB downloads
in between, a big converge used to prompt over and over, hours apart. temper now
checks up front whether this run will touch any such package, names them, and
asks **once** before anything downloads — then holds the timestamp open for the
rest of the run. Nothing to ask for means no prompt at all: a converged machine,
a spec with no pkg-based casks, and every read-only verb stay password-free, and
so does `--dry-run`. Set `TEMPER_NO_SUDO_KEEPALIVE=1` to opt out (you'll get
Homebrew's per-cask prompts back). App Store prompts are Apple's own and can't be
cached this way — `mas` may still ask per app.

**Unattended runs say so instead of dying mysteriously.** Over ssh without a tty, from
cron, or with stdin piped, there is nowhere to type a password — so a run that needs
root warns up front, naming what needs it, rather than looking healthy until the
escalation itself fails with a bare `sudo install … failed`. Same if `sudo` isn't
installed at all. A run needing no root is unaffected and stays silent.

**How temper finds the folder (why you rarely have to tell it).** temper
resolves its home in this order, first hit wins: `$TEMPER_DIR` → walk up from the
cwd → a saved pointer (`temper setup <dir>`) → **auto-scan** a folder named
`steel`/`temper-home`/`.temper` (`steel` is the author's own fleet spec, the
folder temper was built for — kept as a scanned name; yours can use it, or one of
the other two, or any name plus `temper setup`) under `~`, a dev parent
(`~/Developer`, `~/developer`, `~/dev`, `~/src`, `~/code`, `~/projects`, `~/git`,
`~/repos`, …), or a cloud-sync root — the same set `dotsync` probes: each
`~/Library/CloudStorage/*` client, iCloud Drive, `~/Nextcloud`, `~/Dropbox`,
`~/OneDrive`, `~/ProtonDrive`, `~/Google Drive`, `~/Sync`, plus `/media` and
`/run/media/$USER`. So on a
fresh box you have three no-fuss options: clone/sync your folder to one of those
locations (e.g. `~/Developer/temper-home`) and it's found automatically; or run
`temper setup` — with no argument it lists the discovered libraries and lets you
pick one (or paste a path), saving the pointer; `temper setup <dir>` sets one
directly. Or export `TEMPER_DIR`. If several libraries are found and none is
pinned, temper refuses to guess and tells you to run `temper setup`. temper only
*locates* the folder — getting it onto the box (git clone, a synced cloud folder,
a USB copy) is not temper's job.

## 2. Stay current (routine maintenance)

**When:** periodically — the safe, boring upgrade.

```sh
temper update
```

Upgrades the declared package set (`brew upgrade` + `flatpak update`), re-applies
`always` config steps (so hand-drift is corrected), and backfills `ensure` steps
that are missing. It does **not** add newly-declared apps wholesale (that's an
`install`), and it never runs `restore-dconf` (reloading a snapshot would clobber live
tweaks). *(RIS: `just update`, which deliberately excludes gnome-restore.)*

The summary reports what the run **changed**, not what the machine declares —
`upgraded 6 packages` (measured by diffing installed versions across the upgrade,
so a failed upgrade can't claim credit) or `packages already current`.
The package managers' own output is captured: a converged machine is near-silent
except for a spinner, warnings and errors always print, and a tool's private
verdict ("Nothing to update.", about *its* remotes) is never shown as temper's.
`-v` streams everything the tools print.

> **Keep temper itself current across the fleet before using a new manifest
> field.** Because unknown fields are rejected (`deny_unknown_fields`), a machine
> on an older temper doesn't *skip* a field it doesn't know — it fails to parse
> the whole manifest, breaking that machine's entire converge, not just the step.
> So a new field (a new `setkey` backend, `template`, `append`, …) sets a version
> *floor*: bump temper everywhere first, and note the floor in the commit/recipe.
>
> temper helps you catch this: it stamps `temper_version` into `temper.toml` on
> every write, so a box running an older temper can tell a *version skew* (the
> folder came from a newer temper) from a genuine typo. On a skew it says so —
> and, on a Homebrew install (macOS or Linuxbrew), offers to run
> `brew upgrade temper` and re-run your command, instead of dumping a cryptic
> `unknown field` error. Tune this with
> `temper configure set update.mode <off|warn|prompt|auto>` (default `prompt`).
> `auto` upgrades unattended — **only temper**, never your other packages.
> (`temper upgrade` is an alias for `temper update`.)

## 3. The drift loop (the core habit)

**When:** anytime you want to know "is this machine what I said it should be?" —
read-only, so run it freely.

```sh
temper drift
```

The report groups findings by app, surfaces out-of-sync items in red, collapses
in-sync apps, and ends with **Next steps** — the exact command for each way out
of the drift. You pick a direction and run what it prints:

- **Config drifted** (a file/key/assert): `temper install` re-applies
  it, or `temper undo` reverts the last run.
- **Packages missing:** `temper install --packages-only` (add them, no
  config churn) — the additive "install-missing" flow.
> **"changed" is a measured claim.** An `exec` with no `check` hook has no drift
> story: temper ran it and genuinely cannot tell whether it did anything, so it
> reports `ran (no drift-check)` and stays out of the changed count. That is why a
> converged machine can report `0 changed` even with such steps — before, every
> run looked like it did work it may never have done. Give the step a `check`
> hook and it becomes measurable.

- **Packages installed but not declared (extras):** decide the direction —
  - converge machine→spec: `temper prune` (uninstall them, asks first);
  - absorb spec←machine: `temper reconcile` (interactively add/drop).
    or `temper reconcile --csw` to take the machine's state for every item
    at once (see §5);
  - or neither: `[ignore].<manager>` (fleet) / `[machine.ignore]` (this box).
    An ignored package is not just unreported — it is **protected**, and `prune`
    leaves it alone. That matters because `brew bundle cleanup` decides for
    itself what to remove, so temper has to name the ignored packages in the
    file it hands over rather than merely filtering them out of the report.
- **GNOME extensions installed but not declared:** three answers, like packages —
`temper reconcile` declares it for **this machine** (its own `gnome_extensions`
list), `temper prune` uninstalls it (asks first), or
`[ignore].gnome_extensions` silences it.
- **A GNOME extension is installed but switched off:** that is a state the
declaration carries, so it drifts. A bare uuid means *installed and enabled*, so
a switched-off one reports `gnome-extension-enable` and `install` turns it on.
If you switched it off on purpose, say so and the drift goes away without your
desktop changing:

```toml
gnome_extensions = [{ uuid = "CoverflowAltTab@palatis.blogspot.com", enabled = false }]
```

temper asserts only the uuids it declares, by `enable`/`disable`, and never
rewrites `enabled-extensions` wholesale — so extensions the image ships and the
spec says nothing about are left alone.
- **A machine-scope package declared but not installed:** `temper install
--packages-only` puts it back, or `temper reconcile` drops it — from the
machine's Brewfile *or* its loose `packages` list, whichever declared it. A
package a **bundle** declares is fleet scope: removing that one is a spec edit,
and then every machine's `prune` enacts it.
- **GNOME extensions declared but not installed:** two answers. `temper install
--packages-only` puts it back; `temper reconcile` offers to drop it from this
machine's own `gnome_extensions` list — the answer when you removed it on purpose and
every converge keeps reinstalling it. The prompt defaults to *keep*, so absence
alone never quietly un-declares anything.

Reconcile writes to the machine's own list, never a bundle's — a bundle's
`gnome_extensions` is shared by every machine composing it, so editing there would
change every machine off one machine's state. An extension a *bundle* declares
therefore stays a hand edit in either direction, and drift names the file.

**A failed `[[assert]]`** has no verb at all: it reports a condition you resolve
yourself — a group membership clears by logging out. `drift` says so instead of
naming a command that cannot work.

If the condition is *pending* rather than *wrong*, mark the assertion
`severity = "notice"` and give it a `message`. It then reports as a cyan ℹ line
and stays out of the out-of-sync count — because a staged system update means the
machine is fine and waiting for a reboot, and a red ✗ that no `install` can ever
clear is both wrong and unactionable.

**Tap-trust drifted** (`[brew].trust`): a declared tap that isn't trusted
  (brew silently skips its formulae) → `temper install`/`update` re-trusts it;
  a tap trusted on the machine but not declared → `temper reconcile` absorbs it
  into `[brew].trust` (or `[ignore].tap`), or `temper prune` `brew untrust`s it
  (the machine→spec mirror). `[ignore].tap` suppresses the extra either way.

  All of that needs the spec to declare **at least one** tap somewhere. A folder
  that never mentions taps has no opinion about them, so drift reports none and
  `prune` untrusts none — the same opt-in a manager gets by having none of its
  packages declared. Without it, `prune` on a tap-silent spec untrusts every tap
  the machine has, including the ones its own formulae come from.

This four-branch package fork is the heart of it (RIS emitted it at the moment of
detection; temper prints it under Next steps, and in `--json` as `remediation`).

## 4. Add something to the spec

**When:** you want a new app/package on a machine going forward.

1. Edit the folder: add the package to a bundle, the machine's loose `packages`,
   or its `brewfile`; add config steps to the app bundle. (The `[[step]]`
   primitives and their fields are in `SPEC.md` / `temper --llm` — e.g. `setkey`
   can resolve `{{ … }}` in a value at apply time with `template = true`, and its
   `json` backend edits JSONC comment-preservingly. `PATTERNS.md` shows how to
   *compose* the primitives for common problem shapes.)
2. Apply it:

```sh
temper install                 # add packages + apply the new config
# or, packages only, no config re-run:
temper install --packages-only
```

## 5. Absorb ad-hoc changes back into the spec

**When:** you installed/changed something directly on the machine and want the
spec to reflect it (spec←machine). **The default habit is `reconcile`** — it's
per-item and surgical, so nothing lands in the spec without you saying so.

```sh
temper reconcile   # the go-to: interactively add extras / drop missing entries /
                   #   route a flatpak extra to [ignore] / reconcile tap-trust /
                   #   absorb changed desktop keys, per section
temper adopt       # optional first look: just list the extras, mutate nothing
temper reconcile --current-state-wins   # take the machine's state for everything
temper reconcile --csw                  #   (same thing, shorter)
```

`reconcile` prompts per item (missing entries default to keep, extras default to
skip; flatpak extras also offer "ignore") and edits only the machine's **own**
Brewfile, never a shared bundle — so it's safe to run often. Absorbed entries are
written back in canonical order (taps → brews → casks → mas, alphabetical within
each group) so the Brewfile stays sorted instead of growing an unsorted tail.

`reconcile` also reconciles **tap-trust** (`[brew].trust`, fleet-level in
temper.toml) the same way: a tap trusted on the machine but not declared can be
absorbed into `[brew].trust` (or routed to `[ignore].tap`), and a declared tap
that isn't currently trusted can be dropped (default keep — `install`/`update`
would re-trust it). `adopt` is the read-only preview.

### `--current-state-wins` (`--csw`) — absorb everything, no prompts

When you know the machine is right and you'd rather review a diff than answer
forty questions: `--csw` answers every item with "take the machine". Extras get
added, declared-but-absent entries get dropped, changed desktop keys take the
live value. It still prints the full plan and **confirms once** — the per-item
prompts *were* the review step, so removing them without leaving anything would
land a bulk write in your spec unreviewed. `--yes` waives that last confirm.

Unlike the old wholesale dump, it makes the same surgical edits `reconcile`
always makes: only the machine's own Brewfile, `[ignore]` respected, canonically
sorted, comments intact, journaled and undoable. Taps ride along as ordinary
Brewfile entries — a `tap "user/repo"` present on the machine but undeclared is
absorbed (and sorted to the top), one declared but absent is dropped.

> **Converge before you absorb.** `--csw` reads "declared but not installed" as
> "not wanted", and on a machine that hasn't run `install` yet those are the same
> thing — so it would strip the spec down to whatever happens to be present. Run
> `temper drift` first: if it lists *missing* packages, converge (`temper install
> --packages-only`) before absorbing, or you'll adopt an incomplete machine as the
> spec. The preview and `temper undo` are your backstop either way.

> **`--csw` writes machine-scoped files only.** `[brew].trust` and `[ignore]`
> live in `temper.toml` at **fleet** scope — absorbing them from one machine
> would silently change every other machine. So `--csw` reports tap-trust drift
> and leaves it alone. An extra is therefore always *added*, never routed to
> `[ignore]` (ignoring is a judgement, not a state — that stays interactive).
>
> **`--include-trust` adds, and only adds.** It records taps *this* machine
> trusts that the fleet doesn't declare — real knowledge. It will **never remove**
> a declared tap: a declared-but-untrusted tap almost always means the machine
> hasn't run `install` yet (on a brand-new box it has trusted *nothing*), and
> deleting on that basis would break every other machine in the fleet. Removing a
> tap is a fleet decision, so it stays an interactive one — reported every time,
> never automatic.

It never goes quiet about what it skipped:

```
! 2 tap-trust difference(s) NOT absorbed (fleet-scope — affects every machine):
    ublue-os/tap                 trusted here, not in [brew].trust
    jakobhviid/tap               declared, not trusted here
  → temper reconcile                          decide each interactively
  → temper reconcile --csw --include-trust    take the machine's state too
```

## 6. Capture / restore desktop (GNOME + Ptyxis) state

**When:** you tuned the desktop and want it in the spec, or you're resetting a
machine to the snapshot — both on a fresh bring-up **and** mid-life when live
GNOME/Ptyxis state has drifted and you want it back to known-good.

Desktop state is **first-class drift**: `temper drift` compares each declared
`[[machine.dconf]]` subtree against a live dump and reports it key by key, so
you find out the desktop moved without having to capture-and-diff blind.

```sh
temper drift       # per-key: missing / extra / changed, grouped per snapshot
temper reconcile   # absorb per section (= per extension) — the surgical default
temper snapshot-dconf # capture whole subtree(s) into the spec (spec←machine)
temper restore-dconf  # load the snapshot(s) back into live dconf (spec→machine)
```

Both captures run through the `strip` filter (bookkeeping + per-monitor keys
that would corrupt a round-trip), and so does *each side* of a drift compare —
a stripped key never reads as drift.

> **A `missing` desktop key means "at the schema default".** dconf only stores
> non-default values, so absorbing one *removes* it from the snapshot — right
> after you reset something deliberately and re-tuned a few keys, wrong on a box
> where `restore-dconf` has never run. temper can't tell those apart, so `--csw` groups
> removals by section in the preview (`extensions/just-perfection — 31 removed`)
> rather than burying them one per line, and interactive `reconcile` defaults to
> keeping them.

`reconcile` prompts **per section**, which is the unit dconf itself defines: for
a snapshot rooted at `/org/gnome/shell/extensions/` that means one ask per
extension. A key holding a list (`enabled-extensions`, `favorite-apps`) is one
key, so it is one ask, shown as a member-level `+2 −1` delta rather than two
walls of GVariant. `snapshot-dconf` is the wholesale sibling when you'd rather
diff-then-trim in git than answer prompts.

`restore-dconf` is confirm-gated and never part of `update` (so a routine `update`
never clobbers live tweaks) — it's a deliberate, on-demand reset. It is
**journaled**: `temper undo` resets the subtree and reloads your prior state,
guarded so it skips rather than clobbers if the desktop moved since.
`temper restore-dconf --dry-run` previews which snapshots would load, touching nothing. *(RIS: `gnome-backup`/`gnome-restore`,
`ptyxis-backup`/`-restore`.)*

## 7. Undo a run

**When:** a run did something you didn't want.

```sh
temper undo --list           # revertible runs, newest first
temper undo                  # revert the most recent
temper undo <run-id>         # revert a specific one
temper undo --dry-run        # show what would revert, touch nothing
```

A drifted `setkey` names **both values** — `want 0.99, have 0.47` — under the
finding, and carries them in `--json` as `detail`. A bare "drifted" can't tell a
real difference from a formatting artefact, which is what once made a dconf
double-comparison bug take a hand-audit to find.

Reverts file writes (`copy`/`block`/`setkey` json/toml/ini), `setkey(dconf)`
values, and a whole-subtree `restore-dconf` (undo resets the subtree and reloads your
prior dump — a bare reload would leave behind every key the restore introduced).
`setkey(defaults)`, `sysfile`, and `exec` aren't journaled — undo skips them.
You find that out **before** the run rather than after: `temper install
--dry-run` lists each step it would change that `undo` could not revert, with the
reason (`assets/setup.sh — an \`exec\` runs arbitrary code; temper cannot know
what to undo`), so "can I take this back?" is answered while the run is still a
forecast.
Every revert is guarded: if the target changed since, it's skipped, not
clobbered. For a subtree the guard is the **strip-filtered** dump, so ordinary
desktop churn in stripped keys doesn't quietly disqualify the undo.

A run whose changes were *all* unrevertible is still recorded, and `undo` lands
on it and says so. It has to: with nothing recorded there would be no run, and
`temper undo` would reach past it to the previous one and revert that instead —
which is the opposite of what you asked for.

A run written by a *newer* temper than the one reverting keeps its
still-understood entries revertible: an unrecognized op is skipped and counted,
not treated as a corrupt journal.

Each item is named as it goes — `✓ <path>` for a revert, and a skip says **why**
(`changed since temper wrote it`, `gone since temper wrote it`), because that is
the thing you need before deciding what to do next. `--dry-run` lists the same
items as `· would revert <path>` and touches nothing. `install --dry-run` names
the steps behind its count the same way (`· would apply zsh  copy ~/.zshrc`),
and lists any change `undo` would not be able to revert.

## 8. Retire something across the fleet

**When:** a config file, or a package, must be *gone* — not merely undeclared.

Not declaring something and declaring you don't want it are different. An
undeclared package is an **extra**: silent until `prune`, and re-absorbable by
`reconcile --csw`. A retired one is **drift**, every run, until it is gone.

```toml
[[machine]]                       # …or a bundle, for a group
retire = ["~/.config/old-app"]    # paths that must not exist
retire_packages = ['brew "old"']  # packages that must not be installed
```

`drift` reports each as `retired-present` / `retired-package`, and `temper prune`
enacts them with the confirm every destructive thing gets — a directory as
readily as a file, escalating only if the unprivileged removal is refused.
`[ignore]` does **not** silence a retirement: ignoring is "don't tell me",
retiring is "get rid of it".

```sh
temper retired      # every tombstone, and whether it is still doing work
```

Nothing expires on a date. A date would mean two machines on the same commit
behaving differently, and a box offline past it skipping the retirement
silently — so tombstones are **reviewed** instead, oldest first, with `temper
retired`. Delete the entry once the fleet is clean.

Files temper itself deployed usually need no tombstone at all: it records what
it wrote, so dropping a `copy`/`sysfile`/`block` step makes the file *residue*,
which `drift` reports and `prune` removes — provided it is still byte-identical
to what temper left. Edit it and temper reports it instead, and never deletes
your work. `retire` covers what that structurally cannot: files deployed before
the ledger existed, and things temper never put there.

## 9. Declare where flatpaks come from

**When:** an app comes from a remote the machine may not have.

```toml
[[machine]]
flatpak_remotes = ["vendor https://example.com/vendor.flatpakrepo"]
```

The **name** is the identity and the url is what drifts, so a remote configured
under a different url is reported and re-pointed rather than reported forever.
Remotes are added *before* the packages that come from them, for the same reason
tap-trust runs before brew. Remotes are read from **both** installations — one
the image provides already satisfies a declaration — and written to, and removed
from, your **user** one, which is the only one temper may touch.

## 10. Per-machine versions of the fleet lists

**When:** you want something on one box only.

Every category exists at both scopes, because "on this machine only" is an
ordinary thing to want. Declared in a bundle, a thing belongs to the group and
`reconcile` will never edit it from one machine; declared on the machine, that
machine's `reconcile` owns both directions.

```toml
[[machine]]
brew_trust  = ["me/personal-tap"]   # unioned with the fleet [brew].trust
rpm_ostree  = ["proton-vpn"]        # unioned with its bundles' lists
[machine.ignore]
flatpak = ["org.example.JustHere"]  # unioned with the fleet [ignore]
```

The fleet forms stay what they are: a group decision, changed by editing the
shared spec and committing — after which every machine's `prune` enacts it. That
is why `reconcile --csw` refuses to absorb them from one box, and reports what it
skipped instead of quietly widening its remit.

## 11. Pull calibrated speaker profiles

**When:** the upstream speaker-EQ repo has new profiles (needs `[eq_import]`).

```sh
temper eq-import             # clone the repo, land <x>.calibrated.conf → <x>.conf
```

Then apply via the `speaker-eq` step (which is `manual` — run it explicitly).
This is folder-authoring: it writes *into* the folder, then you review + commit.
*(RIS: `just eq-import`.)*

---

## Save spec changes to git (so the folder doesn't drift)

> **Used to run `temper backup`?** It was split: its dconf half is now
> `temper snapshot-dconf` (§6), and its package half is `temper reconcile` — per item,
> or `--csw` for all of them (§5). A machine that isn't in the folder at all
> starts with `temper init` (§0). See the README's "If you used `temper backup`".

`init`, `reconcile`, `snapshot-dconf`, and `eq-import` — and any hand edit — change the
temper-home *folder*, not a machine. If that folder is a git repo, temper helps
you persist those changes so it doesn't silently drift:

- Whenever the folder is left dirty, **any** command hints — the spec-writing
  verbs above *and* the read/apply ones (`drift`, `install`, `update`, `prune`,
  `adopt`, `restore-dconf`), so a stray hand edit surfaces whatever you run next:
  `ⓘ my-spec has uncommitted spec changes — temper save …` (it names your folder).
- **`temper save`** = `pull → add -A → commit → push`, with an
  auto-generated message (`reconcile chronos-redux: +2 -1 ~0`) unless you pass
  `-m "…"`. `--no-push` to hold. Works after hand edits too (message from the
  changed paths).
- **`temper refresh`** (alias `pull`) = `git pull` in the home — the pull-side
  counterpart to `save`. Run it from **anywhere**: temper resolves the folder, so
  you never have to find it or `cd` in just to grab a fleet change. Explicit, so
  it pulls even when `auto_pull` is off; `--rebase` (or `[git].auto_rebase`)
  rebases instead of fast-forward. A non-git home just says so.
- Prefer hands-off? Turn on the `[git]` toggles so temper auto-commits (and
  optionally pushes, and pulls before a run):
  **`temper configure set git.auto_commit true`** (then `git.auto_push`,
  `git.auto_pull`, and `git.auto_rebase` — the last makes `auto_pull` use
  `--rebase` instead of `--ff-only`, so a pull still lands when the box has
  un-pushed local commits). `temper configure list` shows the current values;
  `temper configure unset git.auto_commit` reverts a toggle to its default.
- On a **non-git** folder (Nextcloud / USB / plain dir) all of this is a silent
  no-op — syncing that folder is git/Nextcloud's job, not temper's (Principle #9).

`auto_pull` (and `save`'s pre-push pull) keep you from committing onto a stale
spec; if it can't pull (offline, diverged) it warns and continues — never blocks.
With `auto_rebase` a diverged local is replayed on top instead of warned past.

While it runs you see what it is doing (`⠹ pulling ~/Developer/my-spec`), and it
reports the **effect**: `✓ spec updated (2 commits)` when work landed — which is
also the explanation for a plan that differs from last time — and nothing at all
when the spec was already current. Inside `install`/`update` the pull is a
precondition, not the point, so "already current" is not news; in `temper refresh`,
where the pull *is* the deliverable, it says so explicitly.

**Per-run override.** `auto_pull` is the persistent default; two global flags
override it for a single run on **any** verb: **`--pull`** forces a pull even
when `auto_pull` is off (handy after a known fleet change), and **`--no-pull`**
skips it even when on (handy offline). **`temper status`** shows the home's
state + settings in one view: path, git state, resolved machine, `[git]` toggles,
and the `[update]` self-update mode.

The whole git surface, then, is symmetric: **pull** = `auto_pull` (default) ·
`--pull`/`--no-pull` (per-run) · `temper refresh` (explicit); **push** =
`auto_commit`/`auto_push` (default) · `temper save` (explicit); **status** =
`temper status`.

## Fleet: author once, run per-machine

temper acts on the **machine it runs on**. The machine *argument* selects which
**spec** to read; execution is always local. So the fleet model is:

- **Author centrally.** Edit this one folder; it travels to each machine by
  git/Nextcloud/USB. On each box, `temper setup <dir>` records where it lives.
- **Run per-machine, no argument.** On a machine, `temper install` / `update` /
  `drift` with **no machine name** resolves *this* machine by hostname. Drive
  remotes over ssh — `ssh atlas 'cd ~/my-spec && temper drift'` — so atlas resolves
  and converges *itself*.
- **The machine argument selects a spec, not a remote target.** `temper drift
  atlas` on another box checks *this* box's live state against *atlas's* spec
  (useful while authoring). A live `temper install atlas` from a different host
  **asks you to confirm** — because it would apply atlas's spec to the box you're
  on, not to a remote atlas (`--yes` skips the prompt; `--json` refuses without
  it; a differing OS is still hard-refused). To converge atlas for real, ssh in
  and run `temper install` there — with no name it resolves *itself* by hostname,
  no prompt. Installing by an explicit name is for the deliberate case (e.g.
  imaging a box whose hostname isn't set yet).

## Presence-gating in practice (why config "just works" per machine)

Steps can carry `when = { <probe> }` (skip unless present) or `needs =
{ <probe> }` (error unless present). So composing a desktop bundle on a server,
or an app whose binary is image-baked vs brew-installed, behaves correctly under
one rule — `install`/`update` print `⚠ skipped: <probe> absent`, and `drift`
reports the gated-out step status-only. You rarely think about it; it's why a
machine's app list can be generous without config landing where it shouldn't.

## Which verb touches which kind of state

The single most common surprise: running one verb and finding drift left over.
temper manages several *kinds* of state, and they do not all have the same verbs
— desktop dconf is the only one with a wholesale verb in **both** directions,
which is why it has verbs of its own.

| kind of state | onto the machine | into the spec |
|---|---|---|
| app config (`copy`/`block`/`setkey`/`sysfile`) | `install` / `update` | you author it by hand |
| macOS profiles (`profile`) | `install` only — its apply is a System Settings dialog, so `update` skips it rather than re-asking every run | you author it by hand (the `.mobileconfig` too) |
| packages (brew, cask, tap, flatpak, mas, vscode, rpm) | `install` (remove: `prune`) | `reconcile`, or `reconcile --csw` |
| GNOME extensions | `install` (remove: `prune`) | `reconcile` — both directions, on this machine's own list — or `[ignore].gnome_extensions` |
| desktop dconf subtrees | `restore-dconf` | `snapshot-dconf`, or `reconcile` per key |
| flatpak remotes | `install` (remove: `prune`) | `reconcile` — machine scope |
| rpm-ostree layered packages | `install` (remove: `prune`, which stages a deployment) | `reconcile` — machine scope |
| brew tap-trust | `install` / `update` (remove: `prune`) | `reconcile` — the fleet list needs `--include-trust` |
| deployed-file residue | n/a — it is what a *dropped* step left | `prune` removes it if untouched, reports it if you edited it |
| retirements (`retire`, `retire_packages`) | `prune` enacts them | you author them by hand; `temper retired` reviews them |
| assertions (`[[assert]]`) | nothing — drift-only, you resolve the condition | n/a |

So `snapshot-dconf` captures the **dconf** row and nothing else; a leftover finding
from any other row is not that verb failing. A **profile** is the one row whose
converge is deliberately absent from `update`: `drift` tells you it is missing and
`install` re-offers it, because a dialog can't be re-applied silently the way a
file can. And a failed **assertion** is the one drift no verb converges — a staged
ostree deployment clears on reboot, a group membership by logging out. `drift` says
so rather than naming a command that cannot work.

## Two directions, one table

| You want… | Direction | Command |
|---|---|---|
| Machine to match the spec (packages) | machine→spec | `install --packages-only` / `prune` |
| Machine to match the spec (config) | machine→spec | `install` |
| Spec to match the machine (per item) | spec←machine | `reconcile` |
| Spec to match the machine (everything) | spec←machine | `reconcile --csw` |
| Desktop captured into the spec (wholesale) | spec←machine | `snapshot-dconf` |
| Desktop reset to the snapshot | spec→machine | `restore-dconf` |
| A file a dropped step left behind | machine→spec | `prune` |
| Something gone for good, everywhere | machine→spec | `retire` / `retire_packages`, then `prune` |
| A machine that isn't in the folder yet | spec←machine | `init` (once) |
| Undo the last change | — | `undo` |

## Shell completions

```sh
temper completions zsh  > "${fpath[1]}/_temper"   # or bash / fish / elvish / powershell
```

Prints a completion script for the named shell to stdout; where it belongs is
your shell's business, not temper's. Worth doing once — the verb list is long
enough that completion is how you'll discover `snapshot-dconf` rather than
guessing at `snapshot`.

## Status markers

temper draws `✓` / `✗` / `⚠` / `i` / `→` from a set that renders on any terminal.
If your terminals run a patched font, opt into Nerd glyphs:

```toml
[ui]
icons = "nerd"     # default "unicode"
```

`TEMPER_ICONS=nerd` (or `=unicode`) overrides it for one terminal — font coverage
is a property of the terminal, not of the spec, so the manifest sets the fleet's
norm and the environment handles the exception. The default stays `unicode`
because Nerd glyphs are Private Use Area: an empty box on an unpatched font,
which is a worse failure than a plain `✓`.

Two glyphs are deliberately absent from the Unicode set. `ℹ` is in Unicode's
emoji set, so a colour font renders it double-width — it swallows the following
space and shifts every aligned column after it. The circled `ⓘ` avoids that but
is illegible at terminal sizes. Hence the plain ASCII `i`.

## `--json` everywhere

Every verb takes `--json` (machine output on stdout, progress/errors on stderr),
so these loops script cleanly. `drift --json` includes a `remediation` array (the
Next-steps commands); `reconcile --json` previews the plan without prompting.
