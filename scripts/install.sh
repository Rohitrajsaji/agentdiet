#!/bin/sh
# Install (or upgrade) AgentDiet from a GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/Rohitrajsaji/agentdiet/main/scripts/install.sh | sh
#
# It downloads the archive for this machine and SHA256SUMS, refuses to install unless the checksum
# matches, and puts `agentdiet` in $AGENTDIET_INSTALL_DIR (default ~/.local/bin), replacing an older
# copy atomically. It never edits shell rc files and never runs `agentdiet init` for you.
#
# Settings (environment):
#   AGENTDIET_VERSION      a version such as 0.2.0 (default: the latest release)
#   AGENTDIET_INSTALL_DIR  where to put the binary (default: $HOME/.local/bin)
#   AGENTDIET_BASE_URL     where the release assets are (default: the GitHub release of that version);
#                          any URL curl understands, file:// included
set -eu

repo=Rohitrajsaji/agentdiet
dir=${AGENTDIET_INSTALL_DIR:-$HOME/.local/bin}

say() { printf 'agentdiet-install: %s\n' "$*"; }
die() { printf 'agentdiet-install: error: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" > /dev/null 2>&1 || die "this needs \`$1\`"; }
need curl
need tar
need uname

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  Linux-x86_64) target=x86_64-unknown-linux-musl ;;
  Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-musl ;;
  MINGW* | MSYS* | CYGWIN*) die "on Windows, download agentdiet-<version>-x86_64-pc-windows-msvc.zip from the release page and check it against SHA256SUMS (see README)" ;;
  *) die "no release binary for $(uname -s) $(uname -m); build from source: cargo install --path . --locked" ;;
esac

version=${AGENTDIET_VERSION:-}
if [ -z "$version" ]; then
  version=$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" \
    | sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' | head -n 1)
  [ -n "$version" ] || die "could not find the latest release (set AGENTDIET_VERSION)"
fi
base=${AGENTDIET_BASE_URL:-https://github.com/$repo/releases/download/v$version}
archive=agentdiet-$version-$target.tar.gz

tmp=$(mktemp -d "${TMPDIR:-/tmp}/agentdiet-install.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM
say "downloading $archive"
curl -fsSL "$base/$archive" -o "$tmp/$archive" || die "could not download $base/$archive"
curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS" || die "could not download $base/SHA256SUMS"

expected=$(awk -v f="$archive" '$2 == f || $2 == "*"f { print $1 }' "$tmp/SHA256SUMS")
[ -n "$expected" ] || die "$archive is not listed in SHA256SUMS"
if command -v sha256sum > /dev/null 2>&1; then
  actual=$(sha256sum "$tmp/$archive" | awk '{ print $1 }')
elif command -v shasum > /dev/null 2>&1; then
  actual=$(shasum -a 256 "$tmp/$archive" | awk '{ print $1 }')
else
  die "this needs sha256sum or shasum to verify the download"
fi
[ "$expected" = "$actual" ] || die "checksum mismatch for $archive (expected $expected, got $actual); nothing was installed"
say "checksum OK ($actual)"

tar -xzf "$tmp/$archive" -C "$tmp"
src=$tmp/agentdiet-$version-$target/agentdiet
[ -f "$src" ] || die "the archive has no agentdiet binary"
mkdir -p "$dir"
upgrade=
[ -e "$dir/agentdiet" ] && upgrade=1
# Same directory, then rename: an assistant running agentdiet never sees a half-written file.
cp "$src" "$dir/.agentdiet.new.$$"
chmod 755 "$dir/.agentdiet.new.$$"
mv -f "$dir/.agentdiet.new.$$" "$dir/agentdiet"
say "installed $("$dir/agentdiet" --version) at $dir/agentdiet"

case ":$PATH:" in
  *":$dir:"*) ;;
  *) say "add $dir to your PATH, e.g. in your shell rc: export PATH=\"$dir:\$PATH\"" ;;
esac
if [ -n "$upgrade" ]; then
  say "upgraded in place: assistants already point at this path; check with: agentdiet doctor"
else
  say "next: agentdiet init   (connects your assistants; then: agentdiet doctor)"
fi
