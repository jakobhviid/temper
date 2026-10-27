#!/bin/sh
# A vendor flatpak remote, declared by its .flatpakrepo file: added to the
# system installation, where apps install; in sync after it, and still pointing
# at its repository after a second install; removed by prune once undeclared; a
# remote temper did not add (flathub, there before temper, as on an image) left
# alone.
export TEMPER_DIR=$HOME/spec
has() { flatpak remotes "$1" --columns=name 2>/dev/null | grep -qx "$2"; }
sudo flatpak remote-add --system --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo >/dev/null 2>&1
temper install --yes >/tmp/i.out 2>&1; install=$?
has --system flathub-beta && system=yes || system=no
has --user flathub-beta && user=yes || user=no
temper --json drift >/tmp/d.json 2>/dev/null; drift=$(grep -o '"ok":false' /tmp/d.json | wc -l)
temper install --yes >/tmp/i2.out 2>&1
case "$(flatpak remotes --system --columns=name,url | awk '$1=="flathub-beta"{print $2}')" in
  *.flatpakrepo) url=file ;; https://*) url=repo ;; *) url=gone ;; esac
# Undeclare it, as a spec edit would.
printf '[[machine]]\nname = "m"\nos = "linux"\nflatpak_remotes = []\n' > "$TEMPER_DIR/temper.toml"
temper --json prune --yes >/tmp/p.json 2>/tmp/p.err; prune=$?
has --system flathub-beta && removed=no || removed=yes
has --system flathub && flathub=kept || flathub=removed
echo "install=$install added_system=$system added_user=$user drift_out=$drift url_after_reinstall=$url prune=$prune removed=$removed flathub=$flathub"
[ "$install" = 0 ] && [ "$system" = yes ] && [ "$user" = no ] && [ "$drift" = 0 ] && [ "$url" = repo ] && [ "$prune" = 0 ] \
  && [ "$removed" = yes ] && [ "$flathub" = kept ]
