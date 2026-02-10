---
name: release
description: Create a release PR with version bump and changelog update
---

Create a release PR that bumps the version and updates the changelog. After the PR is merged, a GitHub workflow will automatically tag the release, create a GitHub release, and bump to the next development version.

## Arguments

The skill accepts a version level argument:
- `patch` - 5.5.0 -> 5.5.1
- `minor` - 5.5.0 -> 5.6.0
- `major` - 5.5.0 -> 6.0.0
- Or an explicit version like `5.6.0`

Example: `/release minor`

## Steps

1. **Verify prerequisites**:
   - Must be on `main` branch
   - Working directory must be clean
   - Must be up to date with origin/main

   ```bash
   git fetch origin
   if [ "$(git branch --show-current)" != "main" ]; then
     echo "Error: Must be on main branch"
     exit 1
   fi
   if [ -n "$(git status --porcelain)" ]; then
     echo "Error: Working directory not clean"
     exit 1
   fi
   if [ "$(git rev-parse HEAD)" != "$(git rev-parse origin/main)" ]; then
     echo "Error: Not up to date with origin/main"
     exit 1
   fi
   ```

2. **Run local checks**:
   ```bash
   cargo clippy --all-targets -- -D warnings
   cargo test
   ```
   If checks fail, stop and report the errors.

3. **Determine the new version**:
   ```bash
   # Get current version from Cargo.toml (strip -alpha.N suffix if present)
   CURRENT=$(grep '^version = ' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
   echo "Current version: $CURRENT"

   # Use cargo-release to calculate new version (handles alpha suffix removal)
   cargo release version <LEVEL> --dry-run 2>&1 | grep -o '[0-9]\+\.[0-9]\+\.[0-9]\+' | head -1
   ```
   Note: cargo-release will strip the `-alpha.N` suffix when bumping to a release version.

4. **Create release branch**:
   ```bash
   NEW_VERSION="X.Y.Z"  # from step 3
   git checkout -b release/v${NEW_VERSION}
   ```

5. **Bump version using cargo-release**:
   ```bash
   cargo release version <LEVEL> --execute --no-confirm
   ```

6. **Update CHANGELOG.md**:
   - Rename "Unreleased" section to new version with today's date
   - Create new empty "Unreleased" section at the top
   - Update the comparison links at the bottom of the file:
     - Add a new `[unreleased]` link pointing to `compare/vX.Y.Z...HEAD`
     - Add a new `[X.Y.Z]` link comparing to the previous version

   The changelog follows Keep a Changelog format. Ask the user if they want to review/edit the changelog before proceeding.

7. **Commit changes**:
   ```bash
   git add -A
   git commit -m "release: prepare v${NEW_VERSION}"
   ```

8. **Push and create PR**:
   ```bash
   git push -u origin release/v${NEW_VERSION}

   gh pr create \
     --repo iopsystems/rezolus \
     --title "release: v${NEW_VERSION}" \
     --body "$(cat <<'EOF'
   ## Release v${NEW_VERSION}

   This PR prepares the release of v${NEW_VERSION}.

   ### Changes
   - Version bump in Cargo.toml
   - Changelog update

   ### After Merge
   The release workflows will automatically:
   1. Create git tag `v${NEW_VERSION}` and bump to next dev version
   2. Build and publish packages (deb, rpm, Homebrew)
   3. Create GitHub release with all artifacts
   4. Publish to crates.io

   ---
   See CHANGELOG.md for details.
   EOF
   )"
   ```

9. **Report the PR URL** to the user.

## After PR Merge

When the PR is merged to main, the following workflow chain runs automatically:

1. **`tag-release.yml`** (triggered by push to main with release commit message):
   - Detects the version from Cargo.toml
   - Creates and pushes the git tag `vX.Y.Z`
   - Bumps to next dev version (e.g., `5.5.1-alpha.0`) and pushes to main

2. **`release.yml`** (triggered by the tag push):
   - Builds .deb packages for all supported distros and architectures
   - Builds .rpm packages for Rocky Linux and Amazon Linux
   - Uploads packages to GCP Artifact Registry
   - Creates GitHub release with all package artifacts
   - Publishes to crates.io (stable releases only)
   - Updates Homebrew formula (stable releases only)

## Troubleshooting

- **cargo-release not installed**: `cargo install cargo-release`
- **gh CLI not installed**: `brew install gh` or see https://cli.github.com/
- **Not authenticated with gh**: `gh auth login`
