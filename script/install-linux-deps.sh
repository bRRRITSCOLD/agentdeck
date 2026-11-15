#!/usr/bin/env bash
# GPUI's Linux build and runtime packages, verbatim from gpui-component's
# script/install-linux.sh (tested on Ubuntu 24.04).
set -euo pipefail

# Retry and time out instead of hanging when the Ubuntu mirrors don't resolve.
apt_opts=(-o Acquire::Retries=5 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30)

sudo apt-get "${apt_opts[@]}" update
sudo apt-get "${apt_opts[@]}" install -y \
  gcc g++ clang libfontconfig-dev libwayland-dev \
  libwebkit2gtk-4.1-dev libxkbcommon-x11-dev libx11-xcb-dev \
  libssl-dev libzstd-dev libasound2-dev \
  vulkan-validationlayers libvulkan1

# Not GPUI's: agentdeck-notify's notification contract test starts a private session bus, and
# agentdeckd's keyring contract test runs gnome-keyring-daemon on one.
sudo apt-get "${apt_opts[@]}" install -y dbus-daemon gnome-keyring
