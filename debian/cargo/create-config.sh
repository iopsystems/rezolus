#!/usr/bin/bash

set -eu

SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )
cd "$SCRIPT_DIR/.."


DEB_HOST_GNU_TYPE=${DEB_HOST_GNU_TYPE:-$(dpkg-architecture -qDEB_HOST_GNU_TYPE)}

RUSTFLAGS=("${RUSTFLAGS[@]}")
RUSTFLAGS+=(
    --cap-lints warn
    -Cdebuginfo=2
    "-Clinker=$DEB_HOST_GNU_TYPE-gcc"
)

LDFLAGS="${LDFLAGS:-}"
for ldflag in $LDFLAGS; do
    RUSTFLAGS+=("-Clink-arg=$ldflag")
done


echo "[build]"
echo "rustflags = ["

for flag in "${RUSTFLAGS[@]}"; do
    echo "  \"$flag\","
done

echo "]"
