#!/bin/sh
# temper's host-package-manager loop on a real distro, as `tester`. Every check
# goes through the distro's own tools, never temper's report.
export TEMPER_DIR=$HOME/spec
T=temper
case $EREBUS_PM in
  dnf)     PM=dnf;    lib=oniguruma
           has() { rpm -q "$1" >/dev/null 2>&1; }
           mine() { dnf -q repoquery --userinstalled --disablerepo='*' --qf '%{name}\n' 2>/dev/null | grep -qx "$1"; }
           cleanable() { sudo dnf autoremove --assumeno 2>&1 | grep -q "$1"; }
           hand() { sudo dnf -y -q install "$1" >/dev/null 2>&1; }
           mark() { sudo dnf -y -q mark user "$1" >/dev/null 2>&1 || sudo dnf -y -q mark install "$1" >/dev/null 2>&1; }
           unmark() { sudo dnf -y -q mark dependency "$1" >/dev/null 2>&1 || sudo dnf -y -q mark remove "$1" >/dev/null 2>&1; } ;;
  apt-get) PM=apt;    lib=libonig5
           has() { dpkg-query -W -f='${db:Status-Abbrev}' "$1" 2>/dev/null | grep -q '^ii'; }
           mine() { apt-mark showmanual | grep -qx "$1"; }
           cleanable() { apt-get -s autoremove 2>/dev/null | grep -q "^Remv $1 "; }
           hand() { sudo env DEBIAN_FRONTEND=noninteractive apt-get -y -qq install "$1" >/dev/null 2>&1; }
           mark() { sudo apt-mark manual "$1" >/dev/null 2>&1; }
           unmark() { sudo apt-mark auto "$1" >/dev/null 2>&1; } ;;
  pacman)  PM=pacman; lib=oniguruma
           has() { pacman -Q "$1" >/dev/null 2>&1; }
           mine() { pacman -Qqe | grep -qx "$1"; }
           cleanable() { pacman -Qdtq | grep -qx "$1"; }
           hand() { sudo pacman -S --needed --noconfirm "$1" >/dev/null 2>&1; }
           mark() { sudo pacman -D --asexplicit "$1" >/dev/null 2>&1; }
           unmark() { sudo pacman -D --asdeps "$1" >/dev/null 2>&1; } ;;
  *)       PM=none ;;
esac

if [ "$PM" = none ]; then
  $T install --yes >/tmp/i.out 2>&1; install=$?
  $T --json drift >/tmp/d.json 2>/dev/null; drift=$?
  [ -f /etc/yum.repos.d/vendor.repo ] && repo=written || repo=absent
  grep -q '"kind":"rpm-repo","ok":true,"status":"unavailable"' /tmp/d.json && repostat=unavailable || repostat=other
  echo "pm=$EREBUS_PM install=$install drift=$drift repo=$repo repo_drift=$repostat"
  [ "$install" = 0 ] && [ "$repo" = absent ]
  exit
fi
extras() { jq -r --arg k "$PM-extra" '[.items[] | select(.kind==$k and .ok==false) | .target] | sort | join(",")' /tmp/d.json; }
free() { for c in bc zip lsof patch cpio; do has "$c" || { echo "$c"; return; }; done; }

# 0. an orphan from before temper: installed as nobody's dependency. A plain
#    autoremove would take it; nothing temper does may.
stale=$(free); hand "$stale"; unmark "$stale"

# 1. install jq + tree; the baseline is recorded first.
$T install --yes >/tmp/i1.out 2>&1; install=$?
has jq && has tree && pkgs=yes || pkgs=no
grep -q '"jq"' "$HOME/.local/state/temper/baseline/m.json" 2>/dev/null && base=has_jq || base=ok
[ -f "$HOME/.local/state/temper/baseline/m.json" ] || base=missing

# 2. undo removes what it named and the dependencies only that needed — and not
#    the orphan from before.
$T undo </dev/null >/tmp/u.out 2>&1
has jq && undone=no || undone=yes
has "$lib" && deps=LEFT || deps=freed
has "$stale" && stale1=kept || stale1=GONE
$T install --yes >/tmp/i2.out 2>&1; has jq && reinstalled=yes || reinstalled=no

# 3. drift is clean; nothing from the image is an extra.
$T --json drift >/tmp/d.json 2>/dev/null; clean="[$(extras)]"

# 4. a hand install, and the library jq needs marked as asked-for by hand.
cand=$(free); hand "$cand"; mark "$lib"
$T --json drift >/tmp/d.json 2>/dev/null; seen="[$(extras)]"

# 5. prune removes the hand install, and demotes the library jq needs.
$T --json prune --yes >/tmp/p.json 2>/tmp/p.err; prune=$?
has "$cand" && pruned=no || pruned=yes
{ has "$lib" && ! mine "$lib" && has jq; } && demoted=yes || demoted=no
has "$stale" && stale2=kept || stale2=GONE
ndeps=$(jq -r '.dependencies | length' /tmp/p.json)
depsgone=yes; for d in $(jq -r '.dependencies[]' /tmp/p.json | cut -d' ' -f2); do has "$d" && depsgone="NO:$d"; done
[ "$(jq -r '.failed | length' /tmp/p.json)" = 0 ] || depsgone="FAILED"
[ "$(jq -r '.demote | join(",")' /tmp/p.json)" = "$PM $lib" ] && previewed=yes || previewed=no

# 6. reconcile absorbs a later hand install into the machine's own list.
cand2=$(free); hand "$cand2"
$T reconcile --current-state-wins --yes >/tmp/r.out 2>&1
grep -q "\"$cand2\"" "$TEMPER_DIR/temper.toml" && absorbed=yes || absorbed=no

# 7. declare the library (held as a dependency): drift says so, install marks it
#    yours without installing anything, undo marks it back.
printf '%s = ["%s"]\n' "$PM" "$lib" > "$TEMPER_DIR/apps/lib.toml"
sed -i 's/apps = \["cli"\]/apps = ["cli", "lib"]/' "$TEMPER_DIR/temper.toml"
$T --json drift >/tmp/d2.json 2>/dev/null
jq -e --arg k "$PM" --arg t "$lib" '.items[] | select(.kind==$k and .target==$t and .status=="installed as a dependency")' /tmp/d2.json >/dev/null && dep_drift=yes || dep_drift=no
$T install --yes >/tmp/i3.out 2>&1
mine "$lib" && marked=yes || marked=no
$T undo </dev/null >/tmp/u2.out 2>&1
{ has "$lib" && ! mine "$lib"; } && unmarked=yes || unmarked=no

case $PM in dnf) dnf -q repolist --disabled 2>/dev/null | grep -q '^vendor' && repo=seen || repo=unseen ;; *) [ -f /etc/yum.repos.d/vendor.repo ] && repo=WRITTEN || repo=n/a ;; esac

echo "install=$install pkgs=$pkgs base=$base undo=$undone deps=$deps stale=$stale1/$stale2 prune_deps=$ndeps/$depsgone reinst=$reinstalled clean=$clean seen=$seen prune=$prune pruned=$pruned demoted=$demoted previewed=$previewed absorbed=$absorbed dep_drift=$dep_drift marked=$marked unmarked=$unmarked repo=$repo"
[ "$install" = 0 ] && [ "$pkgs" = yes ] && [ "$base" = ok ] && [ "$undone" = yes ] && [ "$deps" = freed ] && [ "$reinstalled" = yes ] \
  && [ "$stale1" = kept ] && [ "$stale2" = kept ] && [ "$depsgone" = yes ] \
  && [ "$clean" = "[]" ] && [ "$seen" = "[$(printf '%s\n%s\n' "$cand" "$lib" | sort | paste -sd,)]" ] && [ "$pruned" = yes ] \
  && [ "$demoted" = yes ] && [ "$previewed" = yes ] && [ "$absorbed" = yes ] && [ "$dep_drift" = yes ] && [ "$marked" = yes ] \
  && [ "$unmarked" = yes ] && [ "$repo" != unseen ] && [ "$repo" != WRITTEN ]
