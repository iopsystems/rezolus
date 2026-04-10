#!/bin/bash
# Pre-commit validation for the Rezolus viewer.
# Checks:
#   1. site/viewer symlinks are in sync with src/viewer/assets
#   2. Generated dashboard JSON is up to date with Rust definitions
#
# Exit 0  = all good
# Exit 2  = blocking failure (outputs JSON to deny the commit)

set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
SRC="$ROOT/src/viewer/assets/lib"
SITE="$ROOT/site/viewer/lib"

# Files in site/viewer/lib/ that are standalone (not symlinked)
STANDALONE_TOP="data.js script.js dashboards.js viewer_api.js"

errors=()

# ── 1. Check symlinks ───────────────────────────────────────────────

check_symlinks() {
    # Walk every .js and .css file under src/viewer/assets/lib/
    while IFS= read -r src_file; do
        rel="${src_file#$SRC/}"          # e.g. "charts/metric_types.js" or "theme.js"
        base="$(basename "$rel")"
        dir="$(dirname "$rel")"          # "." for top-level

        # Skip top-level standalone files
        if [ "$dir" = "." ]; then
            skip=false
            for s in $STANDALONE_TOP; do
                [ "$base" = "$s" ] && skip=true && break
            done
            if $skip; then
                # Special case: data.js -> data_base.js
                if [ "$base" = "data.js" ]; then
                    link="$SITE/data_base.js"
                    if [ ! -L "$link" ]; then
                        errors+=("missing symlink: site/viewer/lib/data_base.js -> $src_file")
                    fi
                fi
                continue
            fi
        fi

        link="$SITE/$rel"
        if [ ! -L "$link" ]; then
            errors+=("missing symlink: site/viewer/lib/$rel")
        fi
    done < <(find "$SRC" -type f \( -name '*.js' -o -name '*.css' \))
}

# ── 2. Check dashboard JSON is up to date ────────────────────────────

check_dashboards() {
    # Only check if any dashboard-related Rust files are staged
    staged=$(git diff --cached --name-only 2>/dev/null || true)
    need_check=false
    while IFS= read -r f; do
        case "$f" in
            src/viewer/dashboard/*|src/viewer/plot.rs|src/viewer/mod.rs)
                need_check=true ;;
        esac
    done <<< "$staged"

    if $need_check; then
        if ! cargo xtask generate-dashboards --check >/dev/null 2>&1; then
            errors+=("dashboard JSON is out of date — run: cargo xtask generate-dashboards")
        fi
    fi
}

# ── Run checks ───────────────────────────────────────────────────────

check_symlinks
check_dashboards

if [ ${#errors[@]} -gt 0 ]; then
    msg=$(printf '%s\n' "${errors[@]}")
    # Output JSON to block the commit via Claude Code hook protocol
    cat <<EOJSON
{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"Pre-commit checks failed:\n$msg"}}
EOJSON
    exit 2
fi

exit 0
