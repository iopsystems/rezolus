---
name: prerelease
description: Tag and push a prerelease from the current Cargo.toml version. Use when the user wants to cut an alpha/beta/rc prerelease. Aborts on stable versions (no `-` in version string) and if the tag already exists.
user-invocable: true
allowed-tools: Bash
---

Tag and push a prerelease to upstream (iopsystems/rezolus).

The `release.yml` CI workflow triggers on any `v*` tag push and automatically
marks the GitHub release as a pre-release when the tag contains `-` (e.g.
`-alpha.8`). Package registries and Homebrew are only updated for stable
releases.

## Steps

1. **Verify prerequisites**:
   - Must be on `main` branch
   - Working directory must be clean

   ```bash
   if [ "$(git branch --show-current)" != "main" ]; then
     echo "Error: Must be on main branch"
     exit 1
   fi
   if [ -n "$(git status --porcelain)" ]; then
     echo "Error: Working directory not clean"
     exit 1
   fi
   ```

2. **Read version and validate it is a prerelease**:

   ```bash
   VERSION=$(grep '^version = ' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
   echo "Current version: $VERSION"

   if [[ "$VERSION" != *"-"* ]]; then
     echo "Error: $VERSION looks like a stable release (no prerelease suffix)."
     echo "Stable releases are handled by the tag-release CI workflow."
     echo "Commit with message 'release: prepare v$VERSION' on main instead."
     exit 1
   fi
   ```

3. **Check the tag does not already exist on upstream**:

   ```bash
   TAG="v$VERSION"
   if git ls-remote --tags upstream "$TAG" | grep -q "$TAG"; then
     echo "Error: Tag $TAG already exists on upstream. Nothing to do."
     exit 1
   fi
   ```

4. **Create annotated tag and push to upstream**:

   ```bash
   echo "Creating prerelease tag $TAG and pushing to upstream."
   echo "CI will build packages and create a GitHub pre-release."
   git tag -a "$TAG" -m "Prerelease $TAG"
   git push upstream "$TAG"
   ```

5. **Report the GitHub release URL**:

   ```
   https://github.com/iopsystems/rezolus/releases/tag/$TAG
   ```
