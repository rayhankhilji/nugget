#!/usr/bin/env bash
# Install what wayland-session.sh needs for one compositor on Ubuntu (CI runners):
#
#   tests/fixtures/native/linux/install-wayland-session.sh sway|kwin|gnome
#
# Common: the session bus, AT-SPI (with the Python bindings the consent and GTK 4 fixtures use)
# and the portal front end. kwin adds qt6-wayland and the QML modules of xdg-desktop-portal-kde's
# consent dialog (it is a real window on the Wayland QPA); gnome adds GNOME Shell (Mutter),
# xdg-desktop-portal-gnome and gnome-control-center, which provides the global shortcuts
# consent dialog. Prints the versions that matter.
set -euo pipefail

compositor="${1:?usage: $0 sway|kwin|gnome}"
common="dbus dbus-bin libglib2.0-bin at-spi2-core xdg-desktop-portal python3-gi
  gir1.2-gtk-4.0 gir1.2-atspi-2.0 libgl1-mesa-dri libegl1"
case "$compositor" in
sway) extra="sway wtype xdg-desktop-portal-wlr" ;;
kwin)
    extra="kwin-wayland xdg-desktop-portal-kde qt6-wayland
      qml6-module-org-kde-kirigami qml6-module-org-kde-kcmutils qml6-module-org-kde-kquickcontrols
      qml6-module-org-kde-ki18n qml6-module-qtquick qml6-module-qtquick-controls
      qml6-module-qtquick-layouts qml6-module-qtquick-templates qml6-module-qtquick-window
      qml6-module-qtqml-workerscript qml6-module-qt5compat-graphicaleffects"
    ;;
gnome) extra="gnome-shell mutter xdg-desktop-portal-gnome gnome-control-center dconf-service" ;;
*)
    echo "install-wayland-session: unknown compositor $compositor" >&2
    exit 2
    ;;
esac
sudo apt-get update
# shellcheck disable=SC2086
sudo apt-get install -y --no-install-recommends $common $extra
dpkg-query -W xdg-desktop-portal xdg-desktop-portal-kde xdg-desktop-portal-gnome xdg-desktop-portal-wlr \
    sway kwin-wayland gnome-shell gnome-control-center 2>/dev/null || true
/usr/bin/python3 -c "import gi; gi.require_version('Gtk', '4.0'); from gi.repository import Gtk; print('GTK', Gtk.get_major_version(), Gtk.get_minor_version())"
