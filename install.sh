#!/bin/sh
# Install a trellis release binary from GitHub Releases.
#
#   curl -fsSL https://raw.githubusercontent.com/tylerbutler/trellis/main/install.sh | sh
#
# Environment:
#   TRELLIS_VERSION      version to install, e.g. "0.1.0" or "v0.1.0" (default: latest)
#   TRELLIS_INSTALL_DIR  directory to install into (default: ~/.local/bin)
#
# Inside GitHub Actions the install directory is appended to $GITHUB_PATH, so
# a workflow needs exactly one step:
#
#   - run: curl -fsSL https://raw.githubusercontent.com/tylerbutler/trellis/main/install.sh | TRELLIS_VERSION=0.1.0 sh

set -eu

repo="tylerbutler/trellis"
version="${TRELLIS_VERSION:-latest}"
install_dir="${TRELLIS_INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -s)" in
    Linux) os="unknown-linux-musl" ;;
    Darwin) os="apple-darwin" ;;
    *)
        echo "error: unsupported OS $(uname -s) — download a binary from https://github.com/$repo/releases" >&2
        exit 1
        ;;
esac
case "$(uname -m)" in
    x86_64 | amd64) arch="x86_64" ;;
    aarch64 | arm64) arch="aarch64" ;;
    *)
        echo "error: unsupported architecture $(uname -m) — download a binary from https://github.com/$repo/releases" >&2
        exit 1
        ;;
esac
target="$arch-$os"
asset="trellis-$target.tar.gz"

if [ "$version" = "latest" ]; then
    url="https://github.com/$repo/releases/latest/download/$asset"
else
    url="https://github.com/$repo/releases/download/v${version#v}/$asset"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "downloading $url"
curl -fsSL --retry 3 "$url" -o "$tmp/$asset"

# Verify the checksum when a sha256 tool is available (best effort: the
# .sha256 asset is published alongside every archive).
if command -v sha256sum >/dev/null 2>&1; then
    sha_tool="sha256sum"
elif command -v shasum >/dev/null 2>&1; then
    sha_tool="shasum -a 256"
else
    sha_tool=""
fi
if [ -n "$sha_tool" ]; then
    curl -fsSL --retry 3 "$url.sha256" -o "$tmp/$asset.sha256"
    expected=$(awk '{print $1}' "$tmp/$asset.sha256")
    actual=$($sha_tool "$tmp/$asset" | awk '{print $1}')
    if [ "$expected" != "$actual" ]; then
        echo "error: checksum mismatch for $asset (expected $expected, got $actual)" >&2
        exit 1
    fi
fi

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$install_dir"
install -m 755 "$tmp/trellis" "$install_dir/trellis"

if [ -n "${GITHUB_PATH:-}" ]; then
    echo "$install_dir" >>"$GITHUB_PATH"
fi

echo "installed $("$install_dir/trellis" --version) to $install_dir/trellis"
case ":$PATH:" in
    *":$install_dir:"*) ;;
    *) echo "note: $install_dir is not on your PATH" ;;
esac
