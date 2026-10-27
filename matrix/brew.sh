#!/bin/bash
# Homebrew (on Linux) as temper drives it, as `tester`. Checks go through brew's
# own records (`brew list`, `brew info --json`), never temper's report.
#
# The machine: `tree` declared; `jq` installed by hand (it pulls in `oniguruma`);
# `bc` an orphan from before (installed, then marked as nobody's request — `brew
# autoremove` would take it, and nothing temper does may).
set -u
export TEMPER_DIR=$HOME/spec
NONINTERACTIVE=1 /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)" >/tmp/brew-install.log 2>&1 \
  || { echo "brew-install=failed"; exit 1; }
eval "$(/home/linuxbrew/.linuxbrew/bin/brew shellenv)"; export HOMEBREW_NO_ENV_HINTS=1
q() { "$@" >/dev/null 2>&1; }
has() { brew list --formula -1 2>/dev/null | grep -qx "$1"; }
j() { python3 -c "import json,sys; d=json.load(sys.stdin); $1"; }

temper install --yes >/tmp/i.out 2>&1; install=$?
has tree && declared=yes || declared=no
q brew install jq; q brew install bc; q brew tab --no-installed-on-request bc

# drift, reconcile and init name what was asked for — never what it pulled in.
drift=$(temper --json drift | j 'print(",".join(i["target"] for i in d["items"] if i["kind"]=="brew-package-extra"))')
adds=$(temper --json reconcile | j 'print(",".join(d["adds"]))')
mkdir -p ~/fresh; (cd ~/fresh && TEMPER_DIR=~/fresh temper init --yes --role server >/tmp/init.out 2>&1)
seeded=$(cat ~/fresh/brewfiles/* 2>/dev/null | tr -d '"' | sed 's/^brew //' | sort | tr '\n' ,)

# prune: the extra and the dependency only it needed; the orphan stays.
plan=$(temper --json prune --dry-run | j 'print(",".join(e["name"] for e in d["extras"])+"|"+",".join(d["dependencies"])+"|"+",".join(d["autoremove"]))')
temper prune --yes >/tmp/p.out 2>&1; prune=$?
{ ! has jq && ! has oniguruma; } && pruned=yes || pruned=no
has bc && has tree && kept=yes || kept=no

# undo: declare jq, install it, undo — jq and oniguruma go, the orphan stays.
printf 'brew "tree"\nbrew "jq"\n' > "$TEMPER_DIR/brewfiles/m"
temper install --yes >/tmp/i2.out 2>&1; has jq && has oniguruma && reinst=yes || reinst=no
temper undo </dev/null >/tmp/u.out 2>&1
{ ! has jq && ! has oniguruma; } && undone=yes || undone=no
has bc && kept2=yes || kept2=no

echo "install=$install declared=$declared drift=[$drift] adds=[$adds] seeded=[$seeded] plan=[$plan] prune=$prune pruned=$pruned kept=$kept reinst=$reinst undone=$undone kept_after_undo=$kept2"
[ "$install" = 0 ] && [ "$declared" = yes ] && [ "$drift" = "brew jq" ] && [ "$adds" = 'brew "jq"' ] \
  && [ "$seeded" = "jq,tree," ] && [ "$plan" = "jq|brew oniguruma|" ] && [ "$prune" = 0 ] && [ "$pruned" = yes ] \
  && [ "$kept" = yes ] && [ "$reinst" = yes ] && [ "$undone" = yes ] && [ "$kept2" = yes ]
