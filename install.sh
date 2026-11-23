#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ARCH="$(uname -m)"

echo "==> Installing OmSquish for architecture: ${ARCH}"

# 1. Install required Arch Linux build and runtime dependencies
echo "==> Checking and installing Arch dependencies via pacman..."
sudo pacman -S --needed --noconfirm \
    base-devel \
    pkgconf \
    cmake \
    clang \
    rust \
    gtk3 \
    glib2 \
    gdk-pixbuf2 \
    pango \
    cairo \
    libxkbcommon \
    wayland \
    libglvnd \
    libwebp \
    libjpeg-turbo \
    oxipng \
    desktop-file-utils \
    hicolor-icon-theme

# 2. Build omsquish natively for the host CPU architecture
echo "==> Building OmSquish (release) for ${ARCH}..."
cd "${SCRIPT_DIR}"
cargo build --release --bin omsquish

# 3. Install binary into ~/.local/bin/omsquish
echo "==> Installing binary to ${HOME}/.local/bin/omsquish..."
mkdir -p "${HOME}/.local/bin"
install -Dm755 "${SCRIPT_DIR}/target/release/omsquish" "${HOME}/.local/bin/omsquish"

# 4. Install icons into ~/.local/share/icons/hicolor
echo "==> Installing application icons..."
for size in 32x32 48x48 64x64 128x128 256x256 512x512; do
    if [[ -f "${SCRIPT_DIR}/assets/icons/${size}/omsquish.png" ]]; then
        install -Dm644 "${SCRIPT_DIR}/assets/icons/${size}/omsquish.png" \
            "${HOME}/.local/share/icons/hicolor/${size}/apps/omsquish.png"
    fi
done
# Fallback 512x512 icon from assets/icon.png if needed
if [[ ! -f "${HOME}/.local/share/icons/hicolor/512x512/apps/omsquish.png" && -f "${SCRIPT_DIR}/assets/icon.png" ]]; then
    install -Dm644 "${SCRIPT_DIR}/assets/icon.png" \
        "${HOME}/.local/share/icons/hicolor/512x512/apps/omsquish.png"
fi

# 5. Install desktop entry (replacing __HOME__ template token dynamically with $HOME)
echo "==> Installing desktop entry to ${HOME}/.local/share/applications/omsquish.desktop..."
mkdir -p "${HOME}/.local/share/applications"
sed "s|__HOME__|${HOME}|g" "${SCRIPT_DIR}/omsquish.desktop" > "${HOME}/.local/share/applications/omsquish.desktop"
chmod 644 "${HOME}/.local/share/applications/omsquish.desktop"

# 6. Refresh desktop database and icon cache
echo "==> Updating desktop database..."
update-desktop-database "${HOME}/.local/share/applications" || true
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t "${HOME}/.local/share/icons/hicolor" 2>/dev/null || true
fi

echo "==> OmSquish installed successfully to ${HOME}/.local/bin/omsquish!"
