#!/usr/bin/env bash
set -euo pipefail

manifest=${1:-}
if [[ -z "$manifest" || ! -f "$manifest" ]]; then
    echo "usage: $0 <dist-plan.json>" >&2
    exit 2
fi

require_line() {
    local file=$1
    local expected=$2
    if ! grep -Fqx "$expected" "$file"; then
        echo "$file: expected exact line: $expected" >&2
        exit 1
    fi
}

require_line Cargo.toml 'pkg-url = "{ repo }/releases/download/v{ version }/{ name }-{ target }{ archive-suffix }"'
require_line Cargo.toml 'bin-dir = "{ name }-{ target }/{ bin }{ binary-ext }"'
require_line Cargo.toml 'pkg-fmt = "txz"'
require_line Cargo.toml '[package.metadata.binstall.overrides.x86_64-pc-windows-msvc]'
require_line Cargo.toml 'bin-dir = "{ bin }{ binary-ext }"'

version=$(sed -nE 's/^version = "([^"]+)"/\1/p' Cargo.toml | head -n1)
if [[ ! "$version" =~ ^[0-9]{4}\.(1[0-2]|[1-9])\.[0-9]+$ ]]; then
    echo "Cargo.toml version is not CalVer YYYY.M.PATCH: $version" >&2
    exit 1
fi

for target in \
    x86_64-unknown-linux-gnu \
    x86_64-apple-darwin \
    x86_64-pc-windows-msvc
do
    if [[ "$target" == *windows* ]]; then
        archive="opz-$target.zip"
        binary="opz.exe"
    else
        archive="opz-$target.tar.xz"
        binary="opz"
    fi

    jq -e --arg archive "$archive" --arg target "$target" --arg binary "$binary" '
        .artifacts[$archive].kind == "executable-zip"
        and (.artifacts[$archive].target_triples | index($target) != null)
        and any(.artifacts[$archive].assets[]; .name == "opz" and .path == $binary)
        and any(.releases[].artifacts[]; . == $archive)
    ' "$manifest" >/dev/null || {
        echo "dist plan does not match cargo-binstall archive contract for $target" >&2
        exit 1
    }
done

if jq -e 'any(.artifacts[]?.assets[]?; .name == "opz-test-tool")' "$manifest" >/dev/null; then
    echo "test-support binary must not be included in release archives" >&2
    exit 1
fi

echo "cargo-binstall metadata matches cargo-dist release assets"
