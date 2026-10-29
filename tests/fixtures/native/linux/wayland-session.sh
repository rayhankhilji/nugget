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
#                   the one backend on Ubuntu 24.04 that implements GlobalShortcuts (GNOME's
#                   portal gains it only in GNOME 48; Ubuntu 24.04 ships 46).
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
cleanup() {
    for ((i = ${#pids[@]} - 1; i >= 0; i--)); do kill "${pids[i]}" 2>/dev/null; done
    [ -n "${bus_pid:-}" ] && kill "$bus_pid" 2>/dev/null
    sleep 0.3
    rm -rf "$runtime"
}
trap cleanup EXIT

dump_logs() {
    for f in "$logs"/*.log; do
        [ -f "$f" ] || continue
        echo "----- $f (last 60 lines)"
        tail -n 60 "$f"
    done
}

bus_out="$(dbus-daemon --session --fork --print-address=1 --print-pid=1)" || {
    echo "wayland-session: cannot start dbus-daemon" >&2
    exit 1
}
DBUS_SESSION_BUS_ADDRESS="$(echo "$bus_out" | sed -n 1p)"
bus_pid="$(echo "$bus_out" | sed -n 2p)"
export DBUS_SESSION_BUS_ADDRESS

case "$compositor" in
sway)
    export XDG_CURRENT_DESKTOP=sway
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
    export XDG_CURRENT_DESKTOP=KDE KDE_FULL_SESSION=true KDE_SESSION_VERSION=5
    # kwin_wayland carries a file capability (cap_sys_nice), which a no_new_privs container
    # refuses to exec ("Operation not permitted"); a plain copy has none and runs anywhere.
    cp "$(command -v kwin_wayland)" "$runtime/kwin_wayland"
    # No GPU: composite with QPainter rather than through EGL.
    KWIN_COMPOSE=Q "$runtime/kwin_wayland" --virtual --no-lockscreen --width 1280 --height 800 \
        --socket wayland-1 >"$logs/kwin.log" 2>&1 &
    pids+=($!)
    # Qt clients started later (the KDE portal backend) connect to it.
    export QT_QPA_PLATFORM=wayland
    ;;
*)
    echo "wayland-session: unknown compositor $compositor" >&2
    exit 2
    ;;
esac

for _ in $(seq 1 100); do
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

# D-Bus activated services (portals, the accessibility bus) inherit this environment.
dbus-update-activation-environment WAYLAND_DISPLAY XDG_RUNTIME_DIR XDG_SESSION_TYPE \
    XDG_CURRENT_DESKTOP DBUS_SESSION_BUS_ADDRESS ${KDE_FULL_SESSION:+KDE_FULL_SESSION} \
    ${QT_QPA_PLATFORM:+QT_QPA_PLATFORM} >/dev/null 2>&1 || true

if [ "$compositor" = sway ]; then
    # The headless backend has no input devices, and a seat without a keyboard never gives
    # keyboard focus: GTK then reports no `active` window and no `focused` widget over AT-SPI.
    # A virtual keyboard (zwp_virtual_keyboard_v1, held open by a sleeping wtype) gives seat0
    # one, as a real session has. It presses nothing.
    wtype -s 86400000 >"$logs/wtype.log" 2>&1 &
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

"$@"
status=$?
if [ $status -ne 0 ]; then
    dump_logs
fi
exit $status
