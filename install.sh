#!/usr/bin/env bash
# ClipOn One-Line Installer
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/2423gen-stack/clipon/main/install.sh | bash

set -euo pipefail

REPO="2423gen-stack/clipon"
INSTALL_DIR="${HOME}/.local/bin"
DESKTOP_DIR="${HOME}/.local/share/applications"
ICON_DIR="${HOME}/.local/share/icons"

COLOR_GREEN="\033[1;32m"
COLOR_BLUE="\033[1;34m"
COLOR_YELLOW="\033[1;33m"
COLOR_RED="\033[1;31m"
COLOR_RESET="\033[0m"

log_info() {
    echo -e "${COLOR_BLUE}==>${COLOR_RESET} $1"
}

log_success() {
    echo -e "${COLOR_GREEN}==>${COLOR_RESET} $1"
}

log_warn() {
    echo -e "${COLOR_YELLOW}WARN:${COLOR_RESET} $1"
}

log_err() {
    echo -e "${COLOR_RED}ERROR:${COLOR_RESET} $1" >&2
}

# 1. Check OS and Arch
OS="$(uname -s)"
ARCH="$(uname -m)"

if [ "$OS" != "Linux" ]; then
    log_err "ClipOn is designed for Linux (X11 / Wayland). Detected: $OS"
    exit 1
fi

if [ "$ARCH" != "x86_64" ]; then
    log_err "Prebuilt binaries currently support x86_64 architecture only. Detected: $ARCH"
    echo "Please build from source using 'cargo build --release'."
    exit 1
fi

# 2. Check for required downloader
if command -v curl >/dev/null 2>&1; then
    DOWNLOAD_CMD="curl -fsSL"
elif command -v wget >/dev/null 2>&1; then
    DOWNLOAD_CMD="wget -qO-"
else
    log_err "Neither curl nor wget found. Please install one of them."
    exit 1
fi

# 3. Detect latest release tag
log_info "Fetching latest ClipOn release information from GitHub..."
LATEST_TAG=$(curl -s "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/' || true)

if [ -z "$LATEST_TAG" ]; then
    # Fallback to Cargo.toml or fixed tag if release api fails/empty
    LATEST_TAG="v1.2.0"
fi

log_info "Target release: ${COLOR_GREEN}${LATEST_TAG}${COLOR_RESET}"

TAR_NAME="clipon-${LATEST_TAG}-x86_64-unknown-linux-gnu.tar.gz"
DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${LATEST_TAG}/${TAR_NAME}"

TMP_DIR="$(mktemp -d -t clipon-install-XXXXXX)"
cleanup() {
    rm -rf "$TMP_DIR"
}
trap cleanup EXIT

# 4. Download and extract
log_info "Downloading ${DOWNLOAD_URL}..."
if curl -fsSL -o "${TMP_DIR}/${TAR_NAME}" "$DOWNLOAD_URL" 2>/dev/null; then
    tar -xzf "${TMP_DIR}/${TAR_NAME}" -C "$TMP_DIR"
    EXTRACTED_DIR="${TMP_DIR}/clipon-${LATEST_TAG}-x86_64-unknown-linux-gnu"
else
    log_warn "Prebuilt release tarball not found for ${LATEST_TAG}."
    echo "Falling back to downloading repository assets and checking local build or cargo..."
    if command -v cargo >/dev/null 2>&1; then
        log_info "Rust toolchain detected! Building from source via 'cargo install --git'..."
        cargo install --git "https://github.com/${REPO}.git" --tag "${LATEST_TAG}" --root "${HOME}/.local"
        log_success "Build and install completed!"
        exit 0
    else
        log_err "Prebuilt binary unavailable and 'cargo' is not installed."
        log_err "Please install Rust (https://rustup.rs) or check GitHub Releases."
        exit 1
    fi
fi

# 5. Install binary and desktop integration
log_info "Installing ClipOn binary to ${INSTALL_DIR}..."
mkdir -p "$INSTALL_DIR" "$DESKTOP_DIR" "$ICON_DIR"

install -m 755 "${EXTRACTED_DIR}/clipon" "${INSTALL_DIR}/clipon"

# Icons & Desktop
if [ -d "${EXTRACTED_DIR}/assets" ]; then
    if [ -f "${EXTRACTED_DIR}/assets/clipon.png" ]; then
        cp -f "${EXTRACTED_DIR}/assets/clipon.png" "${ICON_DIR}/clipon.png"
    fi
    if [ -f "${EXTRACTED_DIR}/assets/clipon.svg" ]; then
        cp -f "${EXTRACTED_DIR}/assets/clipon.svg" "${ICON_DIR}/clipon.svg"
    fi
    if [ -f "${EXTRACTED_DIR}/assets/clipon.desktop" ]; then
        # Replace hardcoded home path if any, or generate dynamic
        sed "s|Exec=.*clipon|Exec=${INSTALL_DIR}/clipon|g; s|Icon=.*clipon.png|Icon=${ICON_DIR}/clipon.png|g" \
            "${EXTRACTED_DIR}/assets/clipon.desktop" > "${DESKTOP_DIR}/clipon.desktop"
        chmod 644 "${DESKTOP_DIR}/clipon.desktop"
    fi
fi

update-desktop-database "$DESKTOP_DIR" 2>/dev/null || true

# 6. Check PATH
case ":$PATH:" in
    *:"$INSTALL_DIR":*) ;;
    *)
        log_warn "${INSTALL_DIR} is not in your PATH."
        echo "Add the following line to your ~/.bashrc or ~/.zshrc:"
        echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
        ;;
esac

# 7. Check /dev/uinput and /dev/input permissions
log_info "Verifying device permissions for global keys and auto-paste..."
HAS_INPUT_ACCESS=false
if [ -r /dev/input/event0 ] 2>/dev/null || groups | grep -qw "input"; then
    HAS_INPUT_ACCESS=true
fi

if [ "$HAS_INPUT_ACCESS" = false ]; then
    log_warn "Current user may not have read access to /dev/input/event*."
    echo "To enable instant Shift-double-tap detection across all windows, run:"
    echo "  sudo usermod -aG input \$USER"
fi

echo ""
log_success "🎉 ClipOn ${LATEST_TAG} has been successfully installed to ${INSTALL_DIR}/clipon!"
echo ""
echo "To start ClipOn right now:"
echo "  ${INSTALL_DIR}/clipon &"
echo ""
echo "Double-tap 'Shift' anytime to summon your clipboard history!"
