# ADOPT: first-class rpm-ostree repositories

**Delete this file once it is integrated or rejected.** It is a proposal with the
context needed to judge it, not documentation. If adopted, the durable parts
belong in `SPEC.md`, `ARCHITECTURE.md`'s provider matrix and `ROADMAP.md`; if
rejected, a line in `ROADMAP.md` recording *why* is worth more than this file is.

Written from a real migration, not from reading the code: moving a three-machine
Bazzite fleet off a custom OS image and onto declared channels, which is the
first time temper's `rpm_ostree` provider has been used for anything but a single
package from a repo that happened to already exist.

## The problem in one sentence

An `rpm_ostree` package needs a `.repo` file on the host before it can be
resolved, temper has no way to declare one, and the workaround — a `sysfile`
step — lands **after** the package that needs it.

## Why that is worse than it sounds

`temper install` is documented as "add packages + apply all config", and it means
it: the package providers converge, then the `[[step]]` list runs. A `.repo` file
can only be expressed as config, so on a machine that does not already have the
repo, the order is exactly backwards:

1. `rpm-ostree install brave-origin ghostty …` — no repo on disk, nothing
   resolves, every package reported as failed
2. the same run then writes the `.repo` files
3. a second `temper install` succeeds

That is not a cosmetic wart. Three consequences:

**A clean machine does not converge in one pass.** Reproducing a machine from
scratch is the thing the spec exists for, and the answer "run it twice" is a
worse answer than it looks: the first run is not a no-op that gets retried, it is
a run that reports a wall of failures. Anyone converging an unfamiliar spec for
the first time reads that as "this is broken", and they are not wrong to.

**It hides real failures behind expected ones.** If five packages always fail the
first pass, nobody can see the one that failed for a genuine reason — a renamed
package, a dead repo, a dependency conflict. The signal is gone.

**A repo is not drift-checked as a repo.** As a `sysfile` it is an opaque blob:
temper compares bytes, mode and owner, and can say nothing about whether the
repo id is actually configured, whether it resolves, or whether a layered package
has any provider at all. The most useful thing temper could tell you here — "this
package is declared and nothing on this machine can supply it" — is precisely
what it cannot.

## What actually happened

On the first machine (`chronos-redux`, Bazzite → stock `bazzite-gnome`), the
spec declared six layered packages from three repos. On the first `temper
install` after the rebase:

- `ghostty` failed. Its COPR file was image content in `/etc`, so the rebase's
  `/etc` merge dropped it, and steel's replacement had not been written yet.
- `brave-origin` succeeded **by luck**: a stale `brave-browser.repo` from an
  earlier era happened to have survived in `/etc`.

Both outcomes are wrong for the same reason, and the lucky one is the more
dangerous: it made the spec look correct on a machine with history, and it will
not repeat on a fresh install. On a genuinely clean machine every one of those
packages fails the first pass.

The spec now carries a paragraph in the relevant bundle explaining the two-pass
requirement, and the downstream shareable spec says in its README quickstart to
run `temper install` twice. Both of those are documentation standing in for a
missing feature, which is the reason for this file.

## Why the obvious fixes are wrong

**Flip the order — config before packages.** No. Config routinely *needs*
packages: `when = { binary = … }` and `when = { rpm = … }` probes gate steps on
an app being installed, and `setkey` targets files that only exist once the app
does. Flipping the phases trades this bug for its mirror image.

**Add `--config-only` as a mirror of `--packages-only`.** Cheap, and it does make
the two-pass flow explicit and scriptable instead of folklore. But it still
leaves a clean machine needing two passes, still cannot tell a real failure from
an expected one, and still gives no repo-level drift. Worth having as a
convenience; not an answer.

## The precedent is already in temper

temper solves this exact shape for flatpak and gets it right.

`flatpak_remotes` is a first-class list at **both** bundle and machine scope —
"remotes this bundle's apps come from" — converged by its own provider before the
flatpaks that need it. Where a flatpak comes from is *declared*, not left to a
config step that has to land in time.

There is no equivalent for rpm-ostree. The schema has no rpm-repo concept at all.
That asymmetry is the whole bug: one package provider knows about its
prerequisite, the other does not.

## Recommendation

Add `rpm_repos` as a first-class category at bundle and machine scope, converged
by a provider that runs **before** `rpm_ostree`, mirroring
`flatpak_remotes` → `flatpak`.

### Shape — point at the asset, do not re-model dnf

```toml
rpm_repos = [
    { repo = "assets/rpm-repos/brave-browser.repo",
      key  = "assets/rpm-repos/RPM-GPG-KEY-brave-browser" },
    { repo = "assets/rpm-repos/ghostty-copr.repo" },
]
```

temper installs `key` to `/etc/pki/rpm-gpg/<basename>` and `repo` to
`/etc/yum.repos.d/<basename>`, root-owned `0644`, using the same escalation path
`sysfile` already has — so *N* repos still cost one password prompt. Identity is
the repo id parsed out of the file.

The reason to take a file rather than fields (`{ id, baseurl, gpgkey, gpgcheck,
… }`) is fidelity. A vendor's `.repo` file often has to be byte-faithful:
Vivaldi's `%post` rewrites its own repo file unconditionally, so a spec that
wants to bootstrap it has to write exactly what the package would write, or the
two fight forever. Rendering a repo from fields means temper owns a model of
dnf's format and has to keep up with it, for no gain over shipping the file the
vendor publishes.

The key must travel with the repo, not stay a separate `sysfile`. A
`gpgkey=file:///etc/pki/rpm-gpg/…` reference is useless if the key lands in the
config phase after the repo has already been consulted — the same ordering bug,
one level down.

### What this buys beyond ordering

- **Real drift.** "repo `brave-browser` is not configured", "its baseurl differs
  from the spec", and — the one that matters — "`ghostty` is declared but no
  configured repo provides it", which is currently unknowable.
- **A `prune` story.** An undeclared repo in `/etc/yum.repos.d` is exactly the
  kind of residue temper reports for every other category, and `[ignore]` gets an
  `rpm_repo` list like the others for the base image's own repos.
- **`reconcile` in both directions**, since a machine-scope list means a repo
  found on a box can be absorbed into the spec rather than hand-copied.

### One extra worth considering

A repo sometimes needs to exist **disabled**. Concretely: `brave-origin`'s
`%post` writes `/etc/yum.repos.d/brave-origin.repo` pointing at a
`dl.google.com` URL that 404s — Chrome packaging boilerplate Brave never
adapted — with `enabled=1`, which breaks the package re-resolve that every
`rpm-ostree upgrade` performs. The spec currently owns that path with a disabled
stub, and it is the one place it deliberately owns a file a package also writes.
If `rpm_repos` grew a `disabled = true` (or the id-level equivalent), that hack
becomes a declaration.

### Generalises beyond rpm-ostree

If a `dnf` or `apt` provider is ever added, it has the same hole. "Where do this
provider's packages come from" is a per-provider prerequisite, and flatpak is
currently the only one that models it. Worth deciding whether `rpm_repos` is the
second instance of a pattern or the second half of a general one.

## Implementation notes

`PATTERNS.md`'s "Adding a provider" section is the checklist, and it warns about
the parts that get missed — declaring at both scopes, three-valued observation,
converging in one call, registering both directions in `plan::KIND_ANSWERS`,
wiring `ReconcilePlan`/`PrunePlan`, the `ProviderSpec` row in `interface.rs`, and
the `ARCHITECTURE.md` matrix. Its closing advice applies unusually directly here:
*"when you add a check, ask which sibling field and which other scope have the
same shape"* — `flatpak_remotes` is that sibling, and reading how it is threaded
through is most of the work.

`flatpak_remotes` currently appears in `crates/temper-core/src/`
`manifest.rs`, `machine.rs`, `plan.rs`, `providers.rs`, `reconcile.rs`, in
`crates/temper/src/main.rs`, and in the `prune_actually_removes` and
`residue_and_retire` tests. That set is a good approximation of the blast radius.

The ordering guarantee is the part to get right and the part a test should pin:
a spec declaring an `rpm_ostree` package whose only provider is a declared
`rpm_repos` entry must converge **in one pass** on a machine that starts with
neither.

## If this is rejected

The fallback the caller already uses is: express repos as `sysfile` steps, document
the two-pass requirement in the bundle, and tell first-time users to run `temper
install` twice. That works and is in production. What it cannot do is converge a
clean machine in one pass or distinguish an expected first-pass failure from a
real one — so if the answer is no, `--config-only` is worth adding anyway, and
`ROADMAP.md` should record that a repo remains an opaque file by choice.
