#!/bin/bash
# Post-format reminder: after running cargo fmt / cargo xtask fmt, check
# whether dashboard JSON needs regenerating (formatting can change Rust
# source which may affect generated output).

set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"

# Check if any dashboard-related Rust files were modified (staged or unstaged)
modified=$(git diff --name-only 2>/dev/null || true)
need_check=false
while IFS= read -r f; do
    case "$f" in
        src/viewer/dashboard/*|src/viewer/plot.rs|src/viewer/mod.rs)
            need_check=true ;;
    esac
done <<< "$modified"

if $need_check; then
    if ! cargo xtask generate-dashboards --check >/dev/null 2>&1; then
        echo "NOTE: dashboard JSON may be out of date after formatting — run: cargo xtask generate-dashboards"
    fi
fi

exit 0
