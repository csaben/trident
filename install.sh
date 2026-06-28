#!/usr/bin/env sh
# trident installer - downloads the prebuilt binary for your OS/arch and puts it
# on your PATH. After this, use the binary itself to set things up:
#
#   curl -fsSL https://raw.githubusercontent.com/csaben/trident/main/install.sh | sh
#   trident host                 # this machine runs the hub + a session
#   trident join http://IP:8790  # other machines point at the hub
#
# Env overrides:
#   TRIDENT_BIN_DIR   install location (default: ~/.local/bin)
#   TRIDENT_VERSION   release tag to install (default: latest)
set -eu

REPO="csaben/trident"
DEST="${TRIDENT_BIN_DIR:-$HOME/.local/bin}"
VERSION="${TRIDENT_VERSION:-latest}"

say() { printf '\033[36m▸\033[0m %s\n' "$*"; }
die() { printf '\033[31m✗\033[0m %s\n' "$*" >&2; exit 1; }

# --- detect platform -------------------------------------------------------
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Linux)                  plat=linux ;;
  Darwin)                 plat=mac ;;
  MINGW*|MSYS*|CYGWIN*)   plat=windows ;;
  *) die "unsupported OS: $os" ;;
esac
case "$arch" in
  x86_64|amd64)   a=x86_64 ;;
  arm64|aarch64)  a=aarch64 ;;
  *) die "unsupported architecture: $arch" ;;
esac

case "$plat-$a" in
  linux-x86_64)   target=x86_64-unknown-linux-musl; ext=tar.gz ;;
  mac-x86_64)     target=x86_64-apple-darwin;       ext=tar.gz ;;
  mac-aarch64)    target=aarch64-apple-darwin;      ext=tar.gz ;;
  windows-x86_64) target=x86_64-pc-windows-msvc;    ext=zip ;;
  *) die "no prebuilt binary for $plat-$a yet. Build from source: cargo install --git https://github.com/$REPO" ;;
esac

asset="trident-$target.$ext"
if [ "$VERSION" = "latest" ]; then
  url="https://github.com/$REPO/releases/latest/download/$asset"
else
  url="https://github.com/$REPO/releases/download/$VERSION/$asset"
fi

command -v curl >/dev/null 2>&1 || die "curl is required."

# --- download + extract ----------------------------------------------------
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
say "Downloading $asset"
curl -fsSL "$url" -o "$tmp/$asset" || die "download failed: $url (has a release been published yet?)"

say "Extracting"
if [ "$ext" = "tar.gz" ]; then
  tar -xzf "$tmp/$asset" -C "$tmp"
  binfile=trident
else
  command -v unzip >/dev/null 2>&1 || die "unzip is required to install on Windows shells."
  unzip -q "$tmp/$asset" -d "$tmp"
  binfile=trident.exe
fi

# --- install ---------------------------------------------------------------
mkdir -p "$DEST"
cp "$tmp/$binfile" "$DEST/$binfile"
chmod +x "$DEST/$binfile" 2>/dev/null || true

printf '\n\033[32m✓ trident installed to %s/%s\033[0m\n\n' "$DEST" "$binfile"

# PATH hint
case ":$PATH:" in
  *":$DEST:"*) : ;;
  *)
    printf '\033[33m!\033[0m %s is not on your PATH. Add it:\n' "$DEST"
    printf '    echo '\''export PATH="%s:$PATH"'\'' >> ~/.profile && . ~/.profile\n\n' "$DEST"
    ;;
esac

cat <<'NEXT'
Next steps:
  trident host                  # this machine runs the hub + launches a session
  trident join http://IP:8790   # other machines: point at the hub's tailnet IP

First run prompts to register the channel for all Claude Code sessions and to
choose your skip-permissions default. Change anything later with `trident config`.
NEXT
