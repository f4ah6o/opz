#!/usr/bin/env bash
set -euo pipefail

status=0
while IFS= read -r workflow; do
    while IFS= read -r entry; do
        line=${entry%%:*}
        use=${entry#*:}
        use=${use#"${use%%[![:space:]]*}"}
        use=${use#uses:}
        use=${use#"${use%%[![:space:]]*}"}

        reference=${use%%[[:space:]#]*}
        case "$reference" in
            ./*)
                continue
                ;;
        esac

        ref=${reference##*@}
        if [[ ! "$ref" =~ ^[0-9a-f]{40}$ ]]; then
            echo "$workflow:$line: action is not pinned to a full commit SHA: $reference" >&2
            status=1
        fi
        if [[ ! "$use" =~ \#[[:space:]]*(v[0-9]|stable|[0-9]{4}\.[0-9]{1,2}\.[0-9]+) ]]; then
            echo "$workflow:$line: action pin needs a readable version comment: $use" >&2
            status=1
        fi
    done < <(grep -nE '^[[:space:]]*uses:' "$workflow" || true)
done < <(find .github/workflows -type f \( -name '*.yml' -o -name '*.yaml' \) | sort)

exit "$status"
