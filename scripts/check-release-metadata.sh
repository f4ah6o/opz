#!/usr/bin/env bash
set -euo pipefail

name=$(sed -nE 's/^name = "([^"]+)"/\1/p' Cargo.toml | head -n1)
version=$(sed -nE 's/^version = "([^"]+)"/\1/p' Cargo.toml | head -n1)

if [[ "$name" != "opz" ]]; then
    echo "unexpected package name: $name" >&2
    exit 1
fi
if [[ ! "$version" =~ ^[0-9]{4}\.(1[0-2]|[1-9])\.[0-9]+$ ]]; then
    echo "version must use CalVer YYYY.M.PATCH: $version" >&2
    exit 1
fi
if [[ ${GITHUB_REF_TYPE:-} == "tag" && ${GITHUB_REF_NAME:-} != "v$version" ]]; then
    echo "tag ${GITHUB_REF_NAME:-<unset>} does not match Cargo.toml version v$version" >&2
    exit 1
fi

echo "$name version $version is release-compatible"
