#!/bin/sh
# A real vendor rpm repo on a clean dnf box: one install run must resolve `gh`
# from a repo written in that same run.
export TEMPER_DIR=$HOME/spec
temper install --yes >/tmp/i.out 2>&1; install=$?
rpm -q gh >/dev/null 2>&1 && gh=yes || gh=no
from=$(dnf -q repoquery --installed --qf '%{from_repo}\n' gh 2>/dev/null | head -1)
[ -z "$from" ] && from=$(dnf -q repoquery --installed --qf '%{reponame}\n' gh 2>/dev/null | head -1)
temper --json drift > /tmp/d.json 2>/dev/null; out=$(grep -o '"ok":false' /tmp/d.json | wc -l)
echo "install=$install gh=$gh from=$from drift_out=$out"
[ "$gh" = yes ] || tail -15 /tmp/i.out
[ "$install" = 0 ] && [ "$gh" = yes ] && [ "$out" = 0 ]
