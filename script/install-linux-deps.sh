#!/usr/bin/env bash
# The system packages agentdeck builds, tests and runs with on Ubuntu 24.04.
#
# Only what the dependency tree needs: the GUI links libxcb and libxkbcommon(-x11), and
# loads fontconfig, Wayland and Vulkan at runtime. gpui-component's own install script also
# pulls libwebkit2gtk-4.1-dev (for its webview), ALSA, OpenSSL and zstd headers and the Vulkan
# validation layers; agentdeck uses none of them, and webkit alone drags in GStreamer and Mesa,
# ~65 MB that made CI time out on a slow mirror.
#
# In CI (GITHUB_ACTIONS=true) it also:
# - pins the runner's apt mirror list to Azure's Ubuntu mirror, with archive.ubuntu.com behind
#   it. Each runner image picks its own mirror; one pointed at us-east-2.ec2.archive.ubuntu.com
#   crawled at ~50 KB/s while Azure's gave 10 MB/s.
# - when APT_DEB_CACHE names a directory, installs the .debs cached there instead of
#   downloading them, and copies what it downloaded back for actions/cache to save.
set -euo pipefail

# Retry and time out instead of hanging when the Ubuntu mirrors don't resolve.
apt_opts=(
  -o Acquire::Retries=5 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30
)

packages=(
  gcc g++ clang pkg-config
  libfontconfig-dev libwayland-dev libxkbcommon-x11-dev libx11-xcb-dev libvulkan1
  # Not GPUI's: agentdeck-notify's notification contract test starts a private session bus,
  # and agentdeckd's keyring contract test runs gnome-keyring-daemon on one.
  dbus-daemon gnome-keyring
)

archives=/var/cache/apt/archives

if [[ "${GITHUB_ACTIONS:-}" == true && -f /etc/apt/apt-mirrors.txt ]]; then
  printf '%s\tpriority:%s\n' \
    http://azure.archive.ubuntu.com/ubuntu/ 1 \
    http://archive.ubuntu.com/ubuntu/ 2 |
    sudo tee /etc/apt/apt-mirrors.txt >/dev/null
fi

cache="${APT_DEB_CACHE:-}"
if [[ -n "$cache" ]]; then
  mkdir -p "$cache"
  if compgen -G "$cache/*.deb" >/dev/null; then
    sudo cp "$cache"/*.deb "$archives"/
  fi
fi

sudo apt-get "${apt_opts[@]}" update
# Download first, then fill the cache, then install: some images (Docker's ubuntu) delete the
# downloaded .debs as soon as dpkg has run.
sudo apt-get "${apt_opts[@]}" install -y --no-install-recommends --download-only "${packages[@]}"
if [[ -n "$cache" ]]; then
  # Drop the cached versions the mirror has replaced, so the cache doesn't grow week on week.
  sudo apt-get autoclean -qq
  rm -f "$cache"/*.deb
  if compgen -G "$archives/*.deb" >/dev/null; then
    cp "$archives"/*.deb "$cache"/
  fi
fi
sudo apt-get "${apt_opts[@]}" install -y --no-install-recommends "${packages[@]}"
