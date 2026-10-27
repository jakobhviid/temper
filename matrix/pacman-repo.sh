#!/bin/bash
# A signed third-party pacman repo, built here, on a clean Arch box: one run
# trusts its key, reads it and installs from it, fetching no other database;
# dropping the declaration removes the package, the repo file and the key temper
# imported, and pacman still reads its configuration. A key the machine had
# before temper survives a prune.
set -u
export TEMPER_DIR=$HOME/spec
q() { "$@" >/dev/null 2>&1; }
trusted() { gpg --homedir /etc/pacman.d/gnupg --no-auto-check-trustdb --with-colons --list-sigs "$1" 2>/dev/null | awk -F: '$1=="sig" && $11 ~ /l$/ {f=1} END {exit !f}'; }
present() { gpg --homedir /etc/pacman.d/gnupg --no-auto-check-trustdb --list-keys "$1" >/dev/null 2>&1; }
vendor() { printf 'os = "linux"\npacman = ["temper-hello"]\npacman_repos = [{ repo = "assets/pacman/temperrepo.conf", key = "assets/pacman/temperrepo.asc" }]\n' > "$TEMPER_DIR/apps/vendor.toml"; }
dropped() { printf 'os = "linux"\npacman = ["tree"]\n' > "$TEMPER_DIR/apps/vendor.toml"; }

# The vendor: a key, a package signed with it, a repo serving it.
export GNUPGHOME=$(mktemp -d)
q gpg --batch --passphrase '' --quick-gen-key 'temper matrix <matrix@example.com>' ed25519 sign never
FPR=$(gpg --with-colons --list-keys | awk -F: '/^fpr/{print $10; exit}')
gpg --export --armor "$FPR" > "$TEMPER_DIR/assets/pacman/temperrepo.asc"
(cd "$TEMPER_DIR/pkg" && makepkg --nodeps --noconfirm --sign --key "$FPR" >/tmp/makepkg.out 2>&1) || { echo "makepkg=failed"; tail -5 /tmp/makepkg.out; exit 1; }
sudo mkdir -p /srv/temperrepo && sudo cp "$TEMPER_DIR"/pkg/temper-hello-*.pkg.tar.zst* /srv/temperrepo/
q sudo repo-add /srv/temperrepo/temperrepo.db.tar.gz /srv/temperrepo/temper-hello-1-1-any.pkg.tar.zst
q sudo pacman-key --init; q sudo pacman-key --populate archlinux
extra_before=$(stat -c %Y /var/lib/pacman/sync/extra.db 2>/dev/null || echo none)

temper install --yes >/tmp/i.out 2>&1; install=$?
pacman -Q temper-hello >/dev/null 2>&1 && pkg=yes || pkg=no
[ -f /etc/pacman.d/temper/temperrepo.conf ] && [ -f /etc/pacman.d/temper/00-temper.conf ] \
  && grep -qx 'Include = /etc/pacman.d/temper/\*.conf' /etc/pacman.conf && files=yes || files=no
trusted "$FPR" && key=trusted || key=no
[ "$extra_before" = "$(stat -c %Y /var/lib/pacman/sync/extra.db 2>/dev/null || echo none)" ] && others=untouched || others=synced
temper --json drift >/tmp/d.json 2>/dev/null; drift=$(grep -o '"ok":false' /tmp/d.json | wc -l)

# Drop the repo and its package; a converge between the drop and the prune.
dropped; temper install --yes >/tmp/i2.out 2>&1
temper --json prune --yes >/tmp/p.json 2>/tmp/p.err; prune=$?
pacman -Q temper-hello >/dev/null 2>&1 && pkg_gone=no || pkg_gone=yes
[ -e /etc/pacman.d/temper/temperrepo.conf ] && repo_gone=no || repo_gone=yes
present "$FPR" && key_gone=no || key_gone=yes
pacman-conf >/dev/null 2>&1 && conf=reads || conf=broken

# A key imported by hand before temper is the machine's, not temper's.
q sudo pacman-key --add "$TEMPER_DIR/assets/pacman/temperrepo.asc"; q sudo pacman-key --lsign-key "$FPR"
vendor; temper install --yes >/tmp/i3.out 2>&1
dropped; temper install --yes >/tmp/i4.out 2>&1; temper prune --yes >/tmp/p2.out 2>&1
present "$FPR" && kept=yes || kept=no

echo "install=$install pkg=$pkg files=$files key=$key other_dbs=$others drift_out=$drift prune=$prune pkg_removed=$pkg_gone repo_removed=$repo_gone key_removed=$key_gone pacman_conf=$conf own_key_kept=$kept"
[ "$install" = 0 ] && [ "$pkg" = yes ] && [ "$files" = yes ] && [ "$key" = trusted ] && [ "$others" = untouched ] \
  && [ "$drift" = 0 ] && [ "$prune" = 0 ] && [ "$pkg_gone" = yes ] && [ "$repo_gone" = yes ] && [ "$key_gone" = yes ] \
  && [ "$conf" = reads ] && [ "$kept" = yes ]
