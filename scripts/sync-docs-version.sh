#!/usr/bin/env bash
# Update the version label in site/docs/*.html to the latest stable
# release tag. Run idempotently before opening a PR so deployed docs
# track the latest release. Pre-release tags (alpha/beta/rc) are
# ignored — docs reflect what users can actually install.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

VERSION=$(git tag --list 'v[0-9]*' | grep -vE -- '-(alpha|beta|rc)' | sort -V | tail -1)
if [[ -z "$VERSION" ]]; then
    echo "no stable version tag found (looked for v[0-9]*, excluding alpha/beta/rc)" >&2
    exit 1
fi

# Rewrite the sidebar version div in each docs page. The Tailwind
# class string is specific enough that only the version label matches.
VERSION="$VERSION" perl -pi -e \
    's|(<div class="font-mono text-\[10px\] uppercase tracking-widest text-slate-400">)v[0-9][^<]*(</div>)|$1$ENV{VERSION}$2|' \
    site/docs/*.html

echo "Synced site/docs/*.html version label to $VERSION"
