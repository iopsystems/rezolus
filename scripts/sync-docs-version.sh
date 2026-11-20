#!/usr/bin/env bash
# Raise the version label in site/docs/*.html to the newest stable
# local v* tag; never lower it. Run on every PR, after
# `git fetch --tags <remote>` (CLAUDE.md, Git Conventions). Pre-release
# tags (alpha/beta/rc) are ignored: docs reflect what users can install.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

VERSION=$(git tag --list 'v[0-9]*' | grep -vE -- '-(alpha|beta|rc)' | sort -V | tail -1)
if [[ -z "$VERSION" ]]; then
    echo "no stable version tag found (looked for v[0-9]*, excluding alpha/beta/rc)" >&2
    exit 1
fi

# Refuse to lower the label. A clone that has not fetched the newest
# release tag resolves an older one, and rewriting would lower the label
# the pages already show. Pre-release labels are ignored, as tags are.
CURRENT=$(perl -ne 'print "$1\n" if m|<div class="font-mono text-\[10px\] uppercase tracking-widest text-slate-400">(v[0-9][^<]*)</div>|' site/docs/*.html \
    | { grep -vE -- '-(alpha|beta|rc)' || true; } | sort -V | tail -1)
if [[ -n "$CURRENT" && "$CURRENT" != "$VERSION" ]] \
    && [[ $(printf '%s\n%s\n' "$CURRENT" "$VERSION" | sort -V | tail -1) == "$CURRENT" ]]; then
    echo "site/docs/*.html shows $CURRENT, newer than the newest local stable tag $VERSION; leaving it unchanged (run git fetch --tags first)" >&2
    exit 0
fi

# Rewrite the sidebar version div in each docs page. The Tailwind
# class string is specific enough that only the version label matches.
VERSION="$VERSION" perl -pi -e \
    's|(<div class="font-mono text-\[10px\] uppercase tracking-widest text-slate-400">)v[0-9][^<]*(</div>)|$1$ENV{VERSION}$2|' \
    site/docs/*.html

echo "Synced site/docs/*.html version label to $VERSION"
