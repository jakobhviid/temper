#!/bin/bash
# Homebrew (on Linux) as temper drives it, as `tester`. Checks go through brew's
# own records (`brew list`, `brew info --json`, `brew autoremove --dry-run`),
# never temper's report.
#
# The machine: `tree` declared; `jq` installed by hand (it pulls in `oniguruma`);
# `bc` an orphan from before (installed, then marked as nobody's request — `brew
# autoremove` would take it, and nothing temper does may); `nvtop` retired and
# never installed.
set -u
export TEMPER_DIR=$HOME/spec
NONINTERACTIVE=1 /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)" >/tmp/brew-install.log 2>&1 \
  || { echo "brew-install=failed"; exit 1; }
eval "$(/home/linuxbrew/.linuxbrew/bin/brew shellenv)"; export HOMEBREW_NO_ENV_HINTS=1
q() { "$@" >/dev/null 2>&1; }
has() { brew list --formula -1 2>/dev/null | grep -qx "$1"; }
j() { python3 -c "import json,sys; d=json.load(sys.stdin); $1"; }
asked() { brew info --json=v2 "$1" | j 'print(str(d["formulae"][0]["installed"][0]["installed_on_request"]).lower())'; }
orphans() { brew autoremove --dry-run 2>/dev/null | grep -v '^==>' | tr '\n' ,; }
extras() { temper --json drift | j 'print(",".join(i["target"] for i in d["items"] if i["kind"]=="brew-package-extra"))'; }
brewfile() { printf 'brew "%s"\n' "$@" > "$TEMPER_DIR/brewfiles/m"; }

temper install --yes >/tmp/i.out 2>&1; install=$?
has tree && declared=yes || declared=no
q brew install jq; q brew install bc; q brew tab --no-installed-on-request bc

# drift, reconcile and init name what was asked for — never what it pulled in.
drift=$(extras)
adds=$(temper --json reconcile | j 'print(",".join(d["adds"]))')
mkdir -p ~/fresh; (cd ~/fresh && TEMPER_DIR=~/fresh temper init --yes --role server >/tmp/init.out 2>&1)
seeded=$(cat ~/fresh/brewfiles/* 2>/dev/null | tr -d '"' | sed 's/^brew //' | sort | tr '\n' ,)

# prune: the extra and the dependency only it needed; the orphan stays, still
# nobody's request; the retired formula that is not here is not planned.
plan=$(temper --json prune --dry-run | j 'print(",".join(e["name"] for e in d["extras"])+"|"+",".join(d["dependencies"])+"|"+",".join(d["autoremove"]))')
temper prune --yes >/tmp/p.out 2>&1; prune=$?
{ ! has jq && ! has oniguruma; } && pruned=yes || pruned=no
has bc && has tree && [ "$(asked bc)" = false ] && kept=yes || kept=no
after=$(extras)

# undo: declare jq, install it, undo — jq and oniguruma go, the orphan stays.
brewfile tree jq
temper install --yes >/tmp/i2.out 2>&1; has jq && has oniguruma && reinst=yes || reinst=no
temper undo </dev/null >/tmp/u.out 2>&1
{ ! has jq && ! has oniguruma; } && undone=yes || undone=no
has bc && kept2=yes || kept2=no

# A declared dependency, dropped from the Brewfile: brew still holds it as asked
# for. drift reports it, prune marks it as jq's dependency without removing it,
# and it leaves with jq.
brewfile tree jq oniguruma
temper install --yes >/tmp/i3.out 2>&1; [ "$(asked oniguruma)" = true ] && marked=yes || marked=no
brewfile tree jq
dropped=$(extras)
plan2=$(temper --json prune --dry-run | j 'print(",".join(e["name"] for e in d["extras"])+"|"+",".join(d["dependencies"])+"|"+",".join(d["demote"]))')
temper prune --yes >/tmp/p2.out 2>&1
has oniguruma && has jq && [ "$(asked oniguruma)" = false ] && demoted=yes || demoted=no
case ",$(orphans)" in *,oniguruma,*) held=no ;; *) held=yes ;; esac
after2=$(extras)
brewfile tree
temper prune --yes >/tmp/p3.out 2>&1
{ ! has jq && ! has oniguruma && has bc; } && freed=yes || freed=no

echo "install=$install declared=$declared drift=[$drift] adds=[$adds] seeded=[$seeded] plan=[$plan] prune=$prune pruned=$pruned kept=$kept after=[$after] reinst=$reinst undone=$undone kept_after_undo=$kept2 marked=$marked dropped=[$dropped] plan2=[$plan2] demoted=$demoted held=$held after2=[$after2] freed=$freed"
[ "$install" = 0 ] && [ "$declared" = yes ] && [ "$drift" = "brew jq" ] && [ "$adds" = 'brew "jq"' ] \
  && [ "$seeded" = "jq,tree," ] && [ "$plan" = "jq|brew oniguruma|" ] && [ "$prune" = 0 ] && [ "$pruned" = yes ] \
  && [ "$kept" = yes ] && [ -z "$after" ] && [ "$reinst" = yes ] && [ "$undone" = yes ] && [ "$kept2" = yes ] \
  && [ "$marked" = yes ] && [ "$dropped" = "brew oniguruma" ] && [ "$plan2" = "||brew oniguruma" ] \
  && [ "$demoted" = yes ] && [ "$held" = yes ] && [ -z "$after2" ] && [ "$freed" = yes ]
