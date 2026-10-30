#!/usr/bin/env bash
# Run a command inside a private, headless Wayland session: a session bus, a headless compositor,
# the accessibility bus and the XDG desktop portals (all D-Bus activated on the private bus).
#
#   tests/fixtures/native/linux/wayland-session.sh <command> [args...]
#
# Compositor (NUGGET_WAYLAND_COMPOSITOR):
#   sway (default)  wlroots headless backend. Chosen because it starts reliably without a GPU,
#                   seat or logind on ubuntu-latest, and implements wlr-data-control, so the
#                   Wayland clipboard (wl-clipboard-rs) works. Its portal backend
#                   (xdg-desktop-portal-wlr) has no GlobalShortcuts, which is what Nugget's
#                   documented "portal unavailable" path is for.
#   kwin            KDE's kwin_wayland --virtual, with xdg-desktop-portal-kde and kglobalaccel:
#                   a backend that implements GlobalShortcuts and runs headless (Ubuntu 24.04:
#                   Plasma 5.27; ubuntu-latest is 26.04: Plasma 6.6, xdg-desktop-portal 1.21).
#                   The KDE portal runs on the Wayland QPA (needs qt6-wayland), so its consent
#                   dialog is a real window in KWin, exposed over AT-SPI.
#   gnome           gnome-shell --headless with a virtual monitor (Mutter), with
#                   xdg-desktop-portal-gnome and gnome-control-center's global shortcuts provider
#                   (GNOME 48+: GlobalShortcuts, with a GTK 4 consent dialog).
#
# In kwin and gnome sessions keys are pressed with virtual-keyboard.py (KWin fake input, Mutter
# RemoteDesktop) and consent dialogs accepted with accept-shortcut-dialog.py (AT-SPI), both next
# to this script.
#
# The portal names an unsandboxed app only from an installed desktop file whose Exec program
# exists (xdg-desktop-portal 1.19+). When Nugget's package is installed, its
# /usr/share/applications/dev.nugget.Nugget.desktop is used; otherwise the session provides a
# test one whose Exec is $NUGGET_WAYLAND_DESKTOP_EXEC (default /usr/bin/true).
#
# NUGGET_WAYLAND_CONSENT=preseed (kwin only) writes the consent a returning user has (the
# shortcut in kglobalshortcutsrc) before KWin starts, so no dialog is shown. By default the
# dialog is left to the test (CI accepts the real dialog; the pre-seeded path is not run there).
#
# Diagnostics, off by default: NUGGET_WAYLAND_DBUS_MONITOR=1 (dbus-monitor.log),
# NUGGET_WAYLAND_KWIN_LOGGING (QT_LOGGING_RULES for KWin), NUGGET_WAYLAND_MUTTER_DEBUG
# (MUTTER_DEBUG topics), NUGGET_WAYLAND_GNOME_PORTAL_DEBUG (G_MESSAGES_DEBUG for the GNOME portal).
#
# The command sees WAYLAND_DISPLAY, XDG_RUNTIME_DIR, DBUS_SESSION_BUS_ADDRESS,
# XDG_SESSION_TYPE=wayland and XDG_CURRENT_DESKTOP, and no DISPLAY. Logs go to
# $NUGGET_WAYLAND_LOGS (default: a temporary directory) and are printed on failure.
set -uo pipefail

compositor="${NUGGET_WAYLAND_COMPOSITOR:-sway}"
logs="${NUGGET_WAYLAND_LOGS:-$(mktemp -d)}"
mkdir -p "$logs"
# A short path: Unix socket paths are limited to 108 bytes.
runtime="$(mktemp -d /tmp/nwl.XXXXXX)"
chmod 700 "$runtime"
export XDG_RUNTIME_DIR="$runtime"
export XDG_SESSION_TYPE=wayland
unset DISPLAY WAYLAND_DISPLAY

pids=()
as_root=()
cleanup() {
    for ((i = ${#pids[@]} - 1; i >= 0; i--)); do
        # The sudo wrapper forwards the signal to a compositor running as root.
        kill "${pids[i]}" 2>/dev/null
    done
    [ -n "${bus_pid:-}" ] && kill "$bus_pid" 2>/dev/null
    sleep 0.3
    if [ "${#as_root[@]}" -gt 0 ]; then
        "${as_root[@]}" rm -rf "$runtime"
    else
        rm -rf "$runtime"
    fi
}
trap cleanup EXIT

dump_logs() {
    for f in "$logs"/*.log; do
        [ -f "$f" ] || continue
        echo "----- $f (last 60 lines)"
        tail -n 60 "$f"
    done
}

# xdg-desktop-portal 1.19+ names an unsandboxed app only from a desktop file matching the app id
# it registers (nugget-hotkey registers dev.nugget.Nugget), and GLib loads that file only when
# its Exec program exists (on PATH, or an absolute path). Use the installed package's, or provide
# a test one. Older portals ignore it.
mkdir -p "$runtime/data/applications"
export XDG_DATA_DIRS="$runtime/data:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
installed_desktop=/usr/share/applications/dev.nugget.Nugget.desktop
if [ -f "$installed_desktop" ] && [ -z "${NUGGET_WAYLAND_DESKTOP_EXEC:-}" ]; then
    echo "wayland-session: desktop file: the installed $installed_desktop ($(grep -m1 '^Exec=' "$installed_desktop"))"
else
    exec_path="${NUGGET_WAYLAND_DESKTOP_EXEC:-/usr/bin/true}"
    printf '[Desktop Entry]\nType=Application\nName=Nugget\nExec=%s\nNoDisplay=true\n' "$exec_path" \
        >"$runtime/data/applications/dev.nugget.Nugget.desktop"
    echo "wayland-session: desktop file: a test dev.nugget.Nugget.desktop with Exec=$exec_path"
fi

# The desktop name decides which portal backends the front end uses (kde-portals.conf,
# gnome-portals.conf, sway-portals.conf). Set before the bus starts: KWin activates the portal
# front end itself.
case "$compositor" in
sway) export XDG_CURRENT_DESKTOP=sway ;;
kwin) export XDG_CURRENT_DESKTOP=KDE KDE_FULL_SESSION=true KDE_SESSION_VERSION=6 ;;
gnome) export XDG_CURRENT_DESKTOP=GNOME XDG_SESSION_DESKTOP=gnome ;;
esac
# Qt and GTK apps in the session expose their widgets over AT-SPI (the consent dialogs).
export QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1 GTK_A11Y=atspi

# KWin 5.27 only starts as root without a logind seat: as an ordinary user it crashes before it
# parses its arguments (a segfault on ubuntu-latest, a refused QPA plugin elsewhere). Where
# password-less sudo exists (CI runners) it runs as root; its socket is then opened to the
# session's user, and the session bus admits root (KWin hosts kglobalaccel on it).
if [ "$compositor" = kwin ] && [ "$(id -u)" != 0 ]; then
    if sudo -n true 2>/dev/null; then
        as_root=(sudo -n)
    else
        echo "wayland-session: kwin needs root here (sudo -n failed)" >&2
    fi
fi
cat >"$runtime/bus.conf" <<'EOF'
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <include>/usr/share/dbus-1/session.conf</include>
  <policy context="default"><allow user="root"/></policy>
</busconfig>
EOF
bus_conf=(--session)
[ "${#as_root[@]}" -gt 0 ] && bus_conf=(--config-file="$runtime/bus.conf")
bus_out="$(dbus-daemon "${bus_conf[@]}" --fork --print-address=1 --print-pid=1)" || {
    echo "wayland-session: cannot start dbus-daemon" >&2
    exit 1
}
DBUS_SESSION_BUS_ADDRESS="$(echo "$bus_out" | sed -n 1p)"
bus_pid="$(echo "$bus_out" | sed -n 2p)"
export DBUS_SESSION_BUS_ADDRESS

has_name() {
    gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
        --method org.freedesktop.DBus.NameHasOwner "$1" 2>/dev/null | grep -q true
}

case "$compositor" in
sway)
    cat >"$runtime/sway.conf" <<'EOF'
output HEADLESS-1 resolution 1280x800
xwayland disable
default_border none
focus_on_window_activation focus
EOF
    WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman \
        sway -c "$runtime/sway.conf" >"$logs/sway.log" 2>&1 &
    pids+=($!)
    ;;
kwin)
    # kwin_wayland carries a file capability (cap_sys_nice), which a no_new_privs container
    # refuses to exec ("Operation not permitted"); a plain copy has none and runs there.
    cp "$(command -v kwin_wayland)" "$runtime/kwin_wayland"
    # KWin hosts kglobalaccel, which reads $HOME/.config/kglobalshortcutsrc (HOME is $runtime).
    if [ "${NUGGET_WAYLAND_CONSENT:-dialog}" = preseed ]; then
        mkdir -p "$runtime/.config"
        printf '%s\n' '[dev.nugget.Nugget]' '_k_friendly_name=Nugget' \
            'double-tap=Ctrl+Alt+Space,Ctrl+Alt+Space,Nugget: draft a reply' \
            >"$runtime/.config/kglobalshortcutsrc"
        echo "wayland-session: PRE-SEEDED consent: dev.nugget.Nugget/double-tap in kglobalshortcutsrc (no dialog)"
    fi
    # No GPU: composite with QPainter rather than through EGL. KWin 6.6 gives its restricted
    # Wayland interfaces (org_kde_kwin_fake_input, which virtual-keyboard.py uses) only to a
    # program whose desktop file lists them in X-KDE-Wayland-Interfaces, as KDE Connect's does;
    # a python3 desktop file was not found by this root-run KWin's service cache, so this CI
    # compositor runs with KWin's own switch for that check off. Nothing else changes: the key
    # events still enter as an input device's, through KWin's shortcut handling.
    "${as_root[@]}" env XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" DBUS_SESSION_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS" \
        XDG_CURRENT_DESKTOP=KDE XDG_SESSION_TYPE=wayland HOME="$runtime" KWIN_COMPOSE=Q \
        XDG_DATA_DIRS="$XDG_DATA_DIRS" QT_LOGGING_RULES="${NUGGET_WAYLAND_KWIN_LOGGING:-}" \
        KWIN_WAYLAND_NO_PERMISSION_CHECKS=1 \
        "$runtime/kwin_wayland" --virtual --no-lockscreen \
        --width 1280 --height 800 --socket wayland-1 >"$logs/kwin.log" 2>&1 &
    pids+=($!)
    # Qt clients started later (the KDE portal backend) connect to it.
    export QT_QPA_PLATFORM=wayland
    ;;
gnome)
    # A fresh user's first-login extras would cover the desktop: the welcome tour, the idle
    # lock. (dconf is D-Bus activated on this bus.)
    for kv in "org.gnome.shell welcome-dialog-last-shown-version '9999'" \
        "org.gnome.desktop.session idle-delay 0" \
        "org.gnome.desktop.screensaver lock-enabled false"; do
        # shellcheck disable=SC2086
        eval gsettings set $kv >>"$logs/gsettings.log" 2>&1 || true
    done
    # Headless Mutter renders with llvmpipe through surfaceless EGL; the virtual monitor is the
    # session's only output. No logind seat is needed in headless mode.
    export GSK_RENDERER=cairo
    MUTTER_DEBUG="${NUGGET_WAYLAND_MUTTER_DEBUG:-}" \
        gnome-shell --headless --wayland --no-x11 --virtual-monitor 1280x800 \
        --wayland-display wayland-1 >"$logs/gnome-shell.log" 2>&1 &
    pids+=($!)
    ;;
*)
    echo "wayland-session: unknown compositor $compositor" >&2
    exit 2
    ;;
esac

for _ in $(seq 1 300); do
    sock="$(find "$runtime" -maxdepth 1 -name 'wayland-*' -type s | head -n 1)"
    [ -n "$sock" ] && break
    sleep 0.1
done
if [ -z "${sock:-}" ]; then
    echo "wayland-session: $compositor did not create a Wayland socket" >&2
    dump_logs >&2
    exit 1
fi
export WAYLAND_DISPLAY="$(basename "$sock")"
if [ "${#as_root[@]}" -gt 0 ]; then
    "${as_root[@]}" chmod 0777 "$sock"
fi

# D-Bus activated services (portals, the accessibility bus, the GNOME shortcuts provider)
# inherit this environment.
dbus-update-activation-environment WAYLAND_DISPLAY XDG_RUNTIME_DIR XDG_SESSION_TYPE \
    XDG_CURRENT_DESKTOP XDG_DATA_DIRS DBUS_SESSION_BUS_ADDRESS ${KDE_FULL_SESSION:+KDE_FULL_SESSION} \
    ${KDE_SESSION_VERSION:+KDE_SESSION_VERSION} ${QT_QPA_PLATFORM:+QT_QPA_PLATFORM} \
    ${GSK_RENDERER:+GSK_RENDERER} QT_LINUX_ACCESSIBILITY_ALWAYS_ON GTK_A11Y >/dev/null 2>&1 || true

if [ "$compositor" = kwin ]; then
    # Start the KDE portal backend ourselves (rather than by activation) so its log is kept,
    # and wait for it before the portal front end looks for backends.
    xdpk="$(ls /usr/lib/*/libexec/xdg-desktop-portal-kde /usr/libexec/xdg-desktop-portal-kde 2>/dev/null | head -n 1)"
    if [ -n "$xdpk" ]; then
        # On the Wayland QPA (qt6-wayland's xdg-shell integration), so the GlobalShortcuts
        # consent dialog is a real window in KWin; NUGGET_WAYLAND_KDE_QPA=offscreen hides it.
        env -u QT_PLUGIN_PATH -u QT_QPA_PLATFORM_PLUGIN_PATH \
            QT_QPA_PLATFORM="${NUGGET_WAYLAND_KDE_QPA:-wayland}" \
            QT_LOGGING_RULES="xdp-kde-globalshortcuts.debug=true" "$xdpk" \
            >"$logs/portal-kde.log" 2>&1 &
        pids+=($!)
        for _ in $(seq 1 100); do
            has_name org.freedesktop.impl.portal.desktop.kde && break
            sleep 0.1
        done
        has_name org.freedesktop.impl.portal.desktop.kde ||
            echo "wayland-session: xdg-desktop-portal-kde did not start (see portal-kde.log)" >&2
    fi
fi

if [ "$compositor" = gnome ]; then
    # GNOME Shell owns org.gnome.Shell once its JS is up. It starts in the overview, where app
    # accelerators (the portal's grab is ActionMode NORMAL) do not fire.
    for _ in $(seq 1 600); do
        has_name org.gnome.Shell && has_name org.gnome.Mutter.RemoteDesktop && break
        sleep 0.1
    done
    if ! has_name org.gnome.Shell; then
        echo "wayland-session: gnome-shell did not own org.gnome.Shell" >&2
        dump_logs >&2
        exit 1
    fi
    shell_prop() {
        gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell \
            --method org.freedesktop.DBus.Properties.Get org.gnome.Shell "$1" 2>/dev/null
    }
    # App accelerators work only once the shell's startup animation is complete (it sets the
    # action mode from NONE to NORMAL and logs "GNOME Shell started at"); hiding the overview
    # before that can leave the startup unfinished, with every accelerator ignored. Wait for it,
    # then leave the overview it starts in.
    for _ in $(seq 1 600); do
        grep -q "GNOME Shell started at" "$logs/gnome-shell.log" && break
        sleep 0.1
    done
    if ! grep -q "GNOME Shell started at" "$logs/gnome-shell.log"; then
        echo "wayland-session: gnome-shell did not complete its startup in 60 s" >&2
        dump_logs >&2
        exit 1
    fi
    hidden=0
    for _ in $(seq 1 60); do
        if shell_prop OverviewActive | grep -q false; then
            hidden=$((hidden + 1))
            [ "$hidden" -ge 4 ] && break
        else
            hidden=0
            gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell \
                --method org.freedesktop.DBus.Properties.Set org.gnome.Shell OverviewActive '<false>' \
                >/dev/null 2>&1
        fi
        sleep 0.5
    done
    echo "wayland-session: gnome-shell $(shell_prop ShellVersion) overview $(shell_prop OverviewActive)"
    # Start the GNOME portal backend ourselves (rather than by activation) so its log is kept,
    # once the shell it talks to is up.
    xdpg="$(ls /usr/libexec/xdg-desktop-portal-gnome /usr/lib/*/libexec/xdg-desktop-portal-gnome 2>/dev/null | head -n 1)"
    if [ -n "$xdpg" ]; then
        G_MESSAGES_DEBUG="${NUGGET_WAYLAND_GNOME_PORTAL_DEBUG:-}" "$xdpg" -r >"$logs/portal-gnome.log" 2>&1 &
        pids+=($!)
    fi
    for _ in $(seq 1 100); do
        has_name org.freedesktop.impl.portal.desktop.gnome && break
        sleep 0.1
    done
fi

if [ "$compositor" = sway ]; then
    # The headless backend has no input devices, and a seat without a keyboard never gives
    # keyboard focus: GTK then reports no `active` window and no `focused` widget over AT-SPI.
    # A virtual keyboard (zwp_virtual_keyboard_v1, held open by a sleeping wtype) gives seat0
    # one, as a real session has. It presses nothing.
    wtype -s 86400000 >"$logs/wtype.log" 2>&1 &
    pids+=($!)
fi

# Every D-Bus message on the session bus, for diagnosis (NUGGET_WAYLAND_DBUS_MONITOR=1).
if [ "${NUGGET_WAYLAND_DBUS_MONITOR:-0}" = 1 ]; then
    dbus-monitor --session >"$logs/dbus-monitor.log" 2>&1 &
    pids+=($!)
fi

# Activate the accessibility bus and the portal front end now, so the first test does not pay
# for it and the log says what the portal offers.
gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus \
    --method org.a11y.Bus.GetAddress >"$logs/a11y-bus.log" 2>&1 || true
portals="$(gdbus introspect --session --dest org.freedesktop.portal.Desktop \
    --object-path /org/freedesktop/portal/desktop 2>"$logs/portal-introspect.log" |
    grep -o 'interface org.freedesktop.portal.[A-Za-z]*' | sed 's/interface //' | sort -u | tr '\n' ' ')"
echo "wayland-session: compositor=$compositor WAYLAND_DISPLAY=$WAYLAND_DISPLAY portals: ${portals:-none}"
export NUGGET_WAYLAND_PORTALS="$portals"
# The helpers for tests in this session.
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export NUGGET_WAYLAND_KEYS="$here/virtual-keyboard.py"
export NUGGET_WAYLAND_ACCEPT_DIALOG="$here/accept-shortcut-dialog.py"

"$@"
status=$?
if [ $status -ne 0 ]; then
    dump_logs
fi
exit $status
