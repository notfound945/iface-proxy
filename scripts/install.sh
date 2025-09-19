#!/usr/bin/env sh
set -e

# Install iface-proxy to /usr/local/bin/iface-proxy on macOS
# Usage examples:
#   curl -fsSL https://raw.githubusercontent.com/${REPO:-notfound945/iface-proxy}/master/scripts/install.sh | sh
#   wget -qO- https://raw.githubusercontent.com/${REPO:-notfound945/iface-proxy}/master/scripts/install.sh | sh

REPO="${REPO:-notfound945/iface-proxy}"
BINARY_NAME="iface-proxy"
DEST_DIR="/usr/local/bin"
OS_NAME="$(uname -s)"

if [ "$OS_NAME" != "Darwin" ]; then
  echo "Error: Only macOS is supported by this installer." >&2
  exit 1
fi

have_cmd() { command -v "$1" >/dev/null 2>&1; }

download() {
  url="$1"
  out="$2"
  if have_cmd curl; then
    curl -fL "$url" -o "$out"
  elif have_cmd wget; then
    wget -qO "$out" "$url"
  else
    echo "Error: Need curl or wget to download files." >&2
    exit 1
  fi
}

get_latest_tag() {
  api_url="https://api.github.com/repos/$REPO/releases/latest"
  if have_cmd curl; then
    curl -fsSL "$api_url" | sed -n 's/.*"tag_name"\s*:\s*"\([^"]*\)".*/\1/p' | head -n1
  elif have_cmd wget; then
    wget -qO- "$api_url" | sed -n 's/.*"tag_name"\s*:\s*"\([^"]*\)".*/\1/p' | head -n1
  else
    echo "" # Should not happen; guarded by download()
  fi
}

VERSION="${VERSION:-}"
if [ -z "$VERSION" ]; then
  VERSION="$(get_latest_tag)"
  echo "Latest release: $VERSION"
fi

if [ -z "$VERSION" ]; then
  echo "Error: Unable to determine release version. Set VERSION environment variable and retry." >&2
  exit 1
fi

ASSET_NAME="${BINARY_NAME}-${VERSION}-macos.tar.gz"
DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${VERSION}/${ASSET_NAME}"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT INT TERM

echo "Downloading ${ASSET_NAME} ..."
download "$DOWNLOAD_URL" "$TMP_DIR/$ASSET_NAME"

echo "Extracting binary ..."
tar -C "$TMP_DIR" -xzf "$TMP_DIR/$ASSET_NAME"

BIN_PATH_SRC="$TMP_DIR/$BINARY_NAME"
if [ ! -f "$BIN_PATH_SRC" ]; then
  echo "Error: Extracted binary not found at $BIN_PATH_SRC" >&2
  exit 1
fi

chmod +x "$BIN_PATH_SRC"
DEST_PATH="$DEST_DIR/$BINARY_NAME"

mkdir_cmd="mkdir -p \"$DEST_DIR\""
install_cmd="install -m 0755 \"$BIN_PATH_SRC\" \"$DEST_PATH\""

if [ -w "$DEST_DIR" ]; then
  sh -c "$mkdir_cmd"
  sh -c "$install_cmd"
else
  echo "Using sudo to install to $DEST_DIR ..."
  if have_cmd sudo; then
    sudo sh -c "$mkdir_cmd"
    sudo sh -c "$install_cmd"
  else
    echo "Error: $DEST_DIR is not writable and sudo is not available." >&2
    exit 1
  fi
fi

echo "Installed: $DEST_PATH"
echo "Done. You can run '$BINARY_NAME --help' to get started."


