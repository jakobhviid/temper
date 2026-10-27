#!/bin/sh
# A vendor apt source on a clean box: one run installs from it, and dropping
# the declaration removes the package and the files temper wrote.
export TEMPER_DIR=$HOME/spec
has() { dpkg-query -W -f='${db:Status-Abbrev}' "$1" 2>/dev/null | grep -q '^ii'; }
temper install --yes >/tmp/i.out 2>&1; install=$?
has gh && gh=yes || gh=no
[ -f /etc/apt/sources.list.d/github-cli.sources ] && [ -f /etc/apt/keyrings/githubcli-archive-keyring.gpg ] && files=yes || files=no
temper --json drift > /tmp/d.json 2>/dev/null
drift=$(grep -c '"ok":false' /tmp/d.json)
# Undeclare gh and its source, as a spec edit would; tree keeps apt opted in.
printf 'os = "linux"\napt = ["tree"]\n' > "$TEMPER_DIR/apps/gh.toml"
# A converge between the drop and the prune — the order a fleet machine sees.
temper install --yes >/tmp/i2.out 2>&1
temper --json prune --yes > /tmp/p.json 2>/tmp/p.err; prune=$?
has gh && gone=no || gone=yes
[ -e /etc/apt/sources.list.d/github-cli.sources ] || [ -e /etc/apt/keyrings/githubcli-archive-keyring.gpg ] && fgone=no || fgone=yes
apt-get -s install gh >/dev/null 2>&1 && still_resolves=yes || still_resolves=no
echo "install=$install gh=$gh files=$files drift_out=$drift prune=$prune gh_removed=$gone files_removed=$fgone"
[ "$install" = 0 ] && [ "$gh" = yes ] && [ "$files" = yes ] && [ "$drift" = 0 ] && [ "$gone" = yes ] && [ "$fgone" = yes ]
