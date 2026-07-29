#!/usr/bin/env bash
set -euo pipefail

listing=$(mktemp)
trap 'rm -f "$listing"' EXIT

cargo package --locked --list --allow-dirty > "$listing"

for required in \
    Cargo.lock \
    Cargo.toml \
    LICENSE \
    README.md \
    README.ja.md \
    build.rs \
    .agents/skills/opz/SKILL.md \
    src/lib.rs \
    src/main.rs
do
    if ! grep -Fqx "$required" "$listing"; then
        echo "package is missing required file: $required" >&2
        exit 1
    fi
done

if grep -Eq '(^|/)(\.env|target)(/|$)' "$listing"; then
    echo "package contains a generated or secret-bearing path" >&2
    exit 1
fi

echo "package contents are release-compatible"
