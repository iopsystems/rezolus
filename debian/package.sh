#!/bin/bash

set -euo pipefail

PROGRAM="$(basename "$0")"

VERBOSE=false

REZOLUS=/mnt/rezolus
OUTPUT=/mnt/output
RELEASE=0
CHOWN=

help() {
    cat <<EOF
$PROGRAM - Build rezolus debian packages.

USAGE:
    $PROGRAM <FLAGS>

OPTIONS:
    -h|--help       Show this help text.
    -v|--verbose    Display the commands run by this script.

    --release       The release number to use for the package. [default: $RELEASE]
    --rezolus-dir   The directory that the rezolus source is stored in. [default: $REZOLUS]
    --output-dir    The directory to place the output artifacts in. [default: $OUTPUT]
    --chown         Change the ownership of the resulting package files.

USE WITH DOCKER:
    This script is intended to be run within a debian-based docker container.
    As an example, consider building for ubuntu focal:

    docker run -it --rm \\
        -v \$(pwd):/mnt/rezolus \\
        -v \$(pwd)/target/debian:/mnt/output \\
        ubuntu:focal /mnt/rezolus/debian/package.sh --release 0 --chown \$(id -u) --verbose

    You should be able to swap out the docker container in order to build for different
    distros, provided that rezolus can be built on each distro. Note that you may have to
    clean out the debian/cargo_home and debian/cargo_target directories when switching distros.
EOF
}

error() {
    1>&2 echo "error: $1"
    1>&2 echo "Try '$PROGRAM --help' for more information."
}

while [ $# -gt 0 ]; do
    opt="$1"
    shift

    case "$opt" in
        -h|--help)
            help
            exit 0
            ;;
        -v|--verbose)   VERBOSE=true            ;;

        --release)      RELEASE="$1";   shift   ;;
        --rezolus-dir)  REZOLUS="$1";   shift   ;;
        --output-dir)   OUTPUT="$1";    shift   ;;
        --chown)        CHOWN="$1";     shift   ;;

        *)
            error "unexpected option '$opt'"
            exit 1
            ;;
    esac
done

if $VERBOSE; then
    set -x
fi

if [ "$(id -u)" -ne 0 ]; then
    error "package script must be run as root"
fi

shopt -s nullglob globstar

cd "$REZOLUS"

# Install required dependencies

# Disable tzdata requests or other things that may require user interaction
export DEBIAN_FRONTEND=noninteractive

apt-get -q update
apt-get -q install -y build-essential curl jq lsb-release unzip gpg

# Install rust
curl -sSf https://sh.rustup.rs | sh /dev/stdin -y
export PATH="$HOME/.cargo/env:$PATH"

# Build source package
dpkg-source --build .

# Generate the changelog file
export RELEASE="$RELEASE"
cp -p debian/changelog /tmp/changelog
trap 'cp -fp /tmp/changelog debian/changelog' EXIT
./debian/gen-changelog.sh > debian/changelog

# Install build dependencies
apt-get -q build-dep -y ../rezolus*.dsc

# Build the package
dpkg-buildpackage -b -us -uc

# Change ownership of the deb files, if requested
if [ -n "$CHOWN" ]; then
    chown "$CHOWN" ../*.deb ../*.ddeb
fi

# Copy the debs to the output directory
cp ../*.deb ../*.ddeb "$OUTPUT"
