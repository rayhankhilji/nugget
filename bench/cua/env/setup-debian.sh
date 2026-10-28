#!/usr/bin/env bash
# Install what the `cua` xvfb environment uses on Debian or Ubuntu (CI runners included).
# Xvfb and Chromium are required; the rest is optional and recorded in the report when present.
#
#   sudo bench/cua/env/setup-debian.sh
set -euo pipefail

packages=(
  xvfb                # virtual X server (required)
  openbox             # window manager (optional; windows still open without one)
  dbus dbus-x11       # session bus for AT-SPI (optional)
  at-spi2-core        # AT-SPI bus launcher and registry (optional)
  python3-pyatspi     # inspect the accessibility tree by hand (optional)
  xdotool x11vnc      # input injection for wave-2 drivers; watching a run (optional)
)

apt-get update
DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "${packages[@]}"

# Chromium: the Debian package, or point NUGGET_CUA_CHROMIUM at any Chrome/Chromium build
# (for example Playwright's: `npx playwright install chromium`).
if ! command -v chromium >/dev/null && ! command -v chromium-browser >/dev/null \
  && ! command -v google-chrome >/dev/null; then
  DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends chromium \
    || echo "note: no chromium package; set NUGGET_CUA_CHROMIUM to a Chrome binary" >&2
fi
