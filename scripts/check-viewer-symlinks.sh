#!/usr/bin/env bash
# Guard: every shared module under src/viewer/assets/lib/ must RESOLVE at the
# same relative path under site/viewer/lib/, or the deployed static (WASM)
# viewer 404s on the ES-module import and fails to load (see the charts/boxplot.js
# fix). Coverage may be a per-file symlink OR a parent-directory symlink (e.g.
# site/viewer/lib/embed -> ../../../src/viewer/assets/lib/embed), so we test
# resolution (-e), not symlink presence. Only script.js and viewer_api.js are
# standalone (site keeps its own real copy).
#
# Runs in CI so the guard is enforced on every PR — the Claude Code pre-commit
# hook runs the same check but only fires for local Claude Code users.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/src/viewer/assets/lib"; SITE="$ROOT/site/viewer/lib"
missing=(); n=0
while IFS= read -r src; do
    rel="${src#"$SRC"/}"
    case "$rel" in script.js|viewer_api.js) continue ;; esac
    n=$((n+1))
    [ -e "$SITE/$rel" ] || missing+=("$rel")
done < <(find "$SRC" -type f \( -name '*.js' -o -name '*.css' \))
if ((${#missing[@]})); then
    echo "::error::shared viewer modules that do not resolve under site/viewer/lib/ — the deployed static viewer will 404 on these imports and fail to load:"
    printf '  src/viewer/assets/lib/%s\n' "${missing[@]}"
    echo "  fix: add the per-file symlink under site/viewer/lib/ (or ensure a parent-dir symlink covers it); the sync-viewer-symlinks skill does this."
    exit 1
fi
echo "OK: $n shared viewer modules all resolve under site/viewer/lib/"
