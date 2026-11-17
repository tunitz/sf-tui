#!/bin/sh
# Install sftui into a user-writable directory and register it in the user PATH.
# No sudo, administrator rights or system directories are used.

set -eu

REPOSITORY="tunitz/sf-tui"
VERSION="${1:-}"
INSTALL_DIR="${SFTUI_INSTALL_DIR:-$HOME/.local/bin}"

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "sftui installer: '$1' is required" >&2
    exit 1
  }
}

need uname
need mktemp
need tar

case "$(uname -s)" in
  Linux) os=Linux ;;
  Darwin) os=Darwin ;;
  *) echo "sftui installer: unsupported operating system $(uname -s)" >&2; exit 1 ;;
esac

case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  arm64 | aarch64)
    if [ "$os" = "Darwin" ]; then arch=aarch64; else arch=aarch64; fi
    ;;
  *) echo "sftui installer: unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac

target="${arch}-unknown-linux-gnu"
if [ "$os" = "Darwin" ]; then
  target="${arch}-apple-darwin"
fi

if [ -z "$VERSION" ]; then
  need curl
  latest=$(curl -fsSL "https://api.github.com/repos/${REPOSITORY}/releases/latest")
  VERSION=$(printf '%s\n' "$latest" | grep '"tag_name"' | head -n 1 | sed -E 's/.*"v?([^"]+)".*/\1/')
fi

if [ -z "$VERSION" ]; then
  echo "sftui installer: could not determine the latest version" >&2
  exit 1
fi

archive="sftui-${VERSION}-${target}.tar.gz"
url="https://github.com/${REPOSITORY}/releases/download/v${VERSION}/${archive}"
tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT HUP INT TERM

echo "Downloading ${archive}…"
if command -v curl >/dev/null 2>&1; then
  curl -fsSL --retry 3 "$url" -o "$tmpdir/$archive"
elif command -v wget >/dev/null 2>&1; then
  wget -q -O "$tmpdir/$archive" "$url"
else
  echo "sftui installer: curl or wget is required" >&2
  exit 1
fi

sums_url="https://github.com/${REPOSITORY}/releases/download/v${VERSION}/SHA256SUMS"
if command -v curl >/dev/null 2>&1; then
  curl -fsSL "$sums_url" -o "$tmpdir/SHA256SUMS" 2>/dev/null || rm -f "$tmpdir/SHA256SUMS"
elif command -v wget >/dev/null 2>&1; then
  wget -q -O "$tmpdir/SHA256SUMS" "$sums_url" 2>/dev/null || rm -f "$tmpdir/SHA256SUMS"
fi
if [ -f "$tmpdir/SHA256SUMS" ]; then
  expected=$(grep -E "  ${archive}$" "$tmpdir/SHA256SUMS" | awk '{print $1}' | head -n 1)
  if [ -n "$expected" ]; then
    if command -v sha256sum >/dev/null 2>&1; then
      actual=$(sha256sum "$tmpdir/$archive" | awk '{print $1}')
    elif command -v shasum >/dev/null 2>&1; then
      actual=$(shasum -a 256 "$tmpdir/$archive" | awk '{print $1}')
    else
      echo "sftui installer: no SHA-256 tool found; skipping checksum verification" >&2
      actual=""
    fi
    if [ -n "$actual" ] && [ "$actual" != "$expected" ]; then
      echo "sftui installer: checksum verification failed" >&2
      exit 1
    fi
  fi
fi

tar -xzf "$tmpdir/$archive" -C "$tmpdir"
if [ ! -f "$tmpdir/sftui" ]; then
  echo "sftui installer: archive did not contain the sftui executable" >&2
  exit 1
fi

mkdir -p "$INSTALL_DIR"
chmod 755 "$tmpdir/sftui"
cp "$tmpdir/sftui" "$INSTALL_DIR/sftui"
chmod 755 "$INSTALL_DIR/sftui"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    shell_name=$(basename "${SHELL:-/bin/sh}")
    case "$shell_name" in
      zsh) rc="$HOME/.zshrc" ;;
      bash) rc="$HOME/.bashrc" ;;
      *) rc="$HOME/.profile" ;;
    esac
    line="export PATH=\"$INSTALL_DIR:\$PATH\""
    if [ -f "$rc" ] && grep -Fq "$line" "$rc"; then
      :
    elif [ -w "$rc" ] || [ ! -e "$rc" ]; then
      printf '\n# Added by the sftui installer\n%s\n' "$line" >> "$rc"
      echo "Added $INSTALL_DIR to PATH in $rc"
    else
      echo "Add this line to your shell configuration: $line"
    fi
    ;;
esac

echo "Installed sftui $VERSION to $INSTALL_DIR/sftui"
echo "Open a new terminal, then type sftui"
