#!/usr/bin/env bash
# Prepares an Ubuntu / Debian machine to run Gagent.
#
#   scripts/install-ubuntu.sh                 install the libraries the window needs (and git, python3)
#   scripts/install-ubuntu.sh --build         also install Rust, build Gagent and install it for the current user
#   scripts/install-ubuntu.sh --with-docker   also install Docker (optional: only for real isolation)
#
# Needs sudo for apt. Safe to run more than once. Nothing else is installed.
set -euo pipefail

BUILD=0
DOCKER=0
for arg in "$@"; do
  case "$arg" in
    --build) BUILD=1 ;;
    --with-docker) DOCKER=1 ;;
    -h|--help) sed -n '2,8p' "$0"; exit 0 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

if ! command -v apt-get >/dev/null 2>&1; then
  echo "This script supports Ubuntu / Debian (apt). On other systems install Docker, OpenGL and xkbcommon yourself." >&2
  exit 1
fi

SUDO=""
if [ "$(id -u)" -ne 0 ]; then SUDO="sudo"; fi
export DEBIAN_FRONTEND=noninteractive

echo "==> Installing runtime libraries"
$SUDO apt-get update -qq
# libgl1/libegl1: OpenGL for the window; libxkbcommon-x11-0: keyboard; xdg-utils: opens links.
$SUDO apt-get install -y --no-install-recommends \
  ca-certificates curl xdg-utils libgl1 libegl1 libxkbcommon-x11-0 libxkbcommon0 libx11-6 libwayland-client0
$SUDO apt-get install -y --no-install-recommends xdg-desktop-portal-gtk zenity 2>/dev/null || true

# The agent runs its commands with this computer's bash; git and python3 are what most projects need.
$SUDO apt-get install -y --no-install-recommends git python3 python3-pip python3-venv

if [ "$DOCKER" -eq 1 ]; then
  echo "==> Docker (optional)"
  if command -v docker >/dev/null 2>&1; then
    echo "docker is already installed: $(docker --version)"
  else
    $SUDO apt-get install -y docker.io
    $SUDO systemctl enable --now docker 2>/dev/null || true
  fi
  ME="$(id -un)"
  if [ "$ME" != "root" ] && ! id -nG "$ME" | tr ' ' '\n' | grep -qx docker; then
    $SUDO usermod -aG docker "$ME"
    echo "NOTE: you were added to the 'docker' group. Log out and back in (or run 'newgrp docker') before using Docker with Gagent."
  fi
fi

if [ "$BUILD" -eq 1 ]; then
  echo "==> Build tools"
  $SUDO apt-get install -y --no-install-recommends build-essential pkg-config git
  if ! command -v cargo >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
  fi
  rustup update stable >/dev/null 2>&1 || true
  ROOT="$(cd "$(dirname "$0")/.." && pwd)"
  echo "==> Building Gagent (a few minutes)"
  (cd "$ROOT" && cargo build --release)
  mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications"
  install -m 755 "$ROOT/target/release/gagent" "$ROOT/target/release/gagent-cli" "$HOME/.local/bin/"
  cat > "$HOME/.local/share/applications/gagent.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Gagent
Comment=Coding agent for Gemini, NVIDIA and OpenAI-compatible models
Exec=$HOME/.local/bin/gagent
Terminal=false
Categories=Development;
EOF
  echo "Installed to $HOME/.local/bin (make sure it is in your PATH)."
fi

echo "==> Done. Check the setup with: gagent-cli doctor"
