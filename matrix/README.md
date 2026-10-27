# The distro matrix

temper's own test suite fakes the package managers, so it can say what temper
*asks* for. It cannot say whether a real dnf, apt or pacman agrees. These scripts
can: each runs the real `temper` binary on a real distribution, as an unprivileged
user, and checks every outcome with the distribution's own tools (`rpm -q`,
`dpkg-query`, `pacman -Q`, `apt-mark`, `dnf repoquery`, `dnf repolist`) — never
with temper's report of itself.

| script | fixture | what it proves |
|---|---|---|
| `host-packages.sh` | `fixtures/host-packages` | the whole dnf / apt / pacman loop: install, the baseline, `undo` taking the dependencies only its packages needed, a hand install reported, `prune` removing it with its own dependencies and keeping (as a dependency) a library something needs, an orphan from before left alone, `reconcile`, a declared dependency marked as yours and `undo` marking it back, and an `rpm_repos` file that dnf sees. On a distribution with none of the three, that nothing is installed or written. |
| `apt-source.sh` | `fixtures/apt-source` | a vendor apt source (GitHub CLI's): one run installs from it; dropping it, running `install`, then `prune` removes the package, the source and its key |
| `rpm-repo.sh` | `fixtures/rpm-repo` | a vendor rpm repo (GitHub CLI's): one run installs from it with dnf |

## The distributions

| package manager | distributions | scripts |
|---|---|---|
| dnf 5 | Fedora 44 | host-packages, rpm-repo |
| dnf 4 | AlmaLinux 8 and 9, Rocky Linux 10, CentOS Stream 9 | host-packages, rpm-repo |
| apt | Debian 12 and 13, Ubuntu 22.04 and 24.04 | host-packages, apt-source |
| pacman | Arch Linux | host-packages |
| zypper, apk (negative controls) | openSUSE Tumbleweed, Alpine 3.22 | host-packages |

As system containers, plus Ubuntu 24.04, Fedora 44 and Debian 13 as virtual
machines — real systemd, and Ubuntu's background updater holding apt's lock at
boot.

## What a harness has to provide

Any container or VM runner works. Each script expects:

- `temper` on `PATH` — the static `x86_64-unknown-linux-musl` release binary runs
  on every distribution above;
- the fixture copied to `~/spec`;
- an unprivileged user with passwordless `sudo`, running the script;
- `EREBUS_PM` set to the distribution's package manager: `dnf`, `apt-get`,
  `pacman`, `zypper` or `apk`.

The last line the script prints is the verdict (`install=0 pkgs=yes …`), and its
exit status is pass or fail. They are written for the Erebus lab's
`scripts/matrix.sh`, which provides exactly that and runs a list of distributions
in parallel:

```sh
scripts/matrix.sh -u -t matrix/host-packages.sh \
  -p target/x86_64-unknown-linux-musl/release/temper:/usr/local/bin/temper \
  -p matrix/fixtures/host-packages:/home/tester/spec \
  fedora/44 almalinux/9 almalinux/8 rockylinux/10 centos/9-Stream \
  debian/13 debian/12 ubuntu/noble ubuntu/jammy archlinux \
  opensuse/tumbleweed alpine/3.22
```

Add `--vm` for the virtual-machine rows. The apt-source and rpm-repo scripts
reach the internet (GitHub's package servers); the keys and repo files in their
fixtures are GitHub's published, public ones.
