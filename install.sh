#!/usr/bin/env bash
set -euo pipefail

REPO="tiagoyuzoyoshida/violin"
INSTALL_DIR="${VIOLIN_INSTALL_DIR:-$HOME/.local/bin}"

OS="$(uname -s)"
if [ "$OS" != "Linux" ]; then
  echo "Error: violin prebuilt binaries only support Linux (detected: $OS)." >&2
  exit 1
fi

ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64)
    TARGET="x86_64-unknown-linux-musl"
    ;;
  aarch64|arm64)
    TARGET="aarch64-unknown-linux-musl"
    ;;
  *)
    echo "Error: unsupported architecture '$ARCH'. Supported: x86_64, aarch64." >&2
    exit 1
    ;;
esac

for cmd in curl tar; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "Error: required command '$cmd' not found in PATH." >&2
    exit 1
  fi
done

if [ -n "${VIOLIN_VERSION:-}" ]; then
  case "$VIOLIN_VERSION" in
    v*) TAG="$VIOLIN_VERSION" ;;
    *) TAG="v${VIOLIN_VERSION}" ;;
  esac
else
  API_URL="https://api.github.com/repos/${REPO}/releases/latest"
  if ! RELEASE_JSON="$(curl -fsSL "$API_URL" 2>/dev/null)"; then
    echo "Error: could not fetch latest release from GitHub." >&2
    echo "If no release has been published yet, build from source instead:" >&2
    echo "  cargo install --git https://github.com/${REPO}" >&2
    exit 1
  fi
  TAG="$(printf '%s\n' "$RELEASE_JSON" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)"
  if [ -z "$TAG" ]; then
    echo "Error: no release tag found for ${REPO}." >&2
    echo "Build from source instead:" >&2
    echo "  cargo install --git https://github.com/${REPO}" >&2
    exit 1
  fi
fi

ARCHIVE="violin-${TAG}-${TARGET}.tar.gz"
BASE_URL="https://github.com/${REPO}/releases/download/${TAG}"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

echo "Downloading violin ${TAG} (${TARGET})..."
curl -fsSL "${BASE_URL}/${ARCHIVE}" -o "${TMP_DIR}/${ARCHIVE}"

if command -v sha256sum >/dev/null 2>&1; then
  curl -fsSL "${BASE_URL}/SHA256SUMS.txt" -o "${TMP_DIR}/SHA256SUMS.txt"
  (cd "$TMP_DIR" && sha256sum -c --ignore-missing SHA256SUMS.txt)
fi

tar -xzf "${TMP_DIR}/${ARCHIVE}" -C "$TMP_DIR" violin
mkdir -p "$INSTALL_DIR"
install -m 0755 "${TMP_DIR}/violin" "${INSTALL_DIR}/violin"

echo "Installed violin ${TAG} to ${INSTALL_DIR}/violin"

case ":$PATH:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    echo ""
    echo "Note: ${INSTALL_DIR} is not in your PATH."
    echo "Add it by appending this line to your shell profile (~/.bashrc or ~/.zshrc):"
    echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
    ;;
esac
