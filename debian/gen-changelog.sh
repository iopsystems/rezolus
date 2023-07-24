#!/usr/bin/env bash

VERSION=$(cargo metadata --format-version 1 --no-deps | jq -r '.packages[] | select(.name == "rezolus") | .version')
RELEASE=${RELEASE:-1}

cat <<EOM
rezolus ($VERSION-$RELEASE) $(lsb_release -sc); urgency=medium

  * Automated update package for rezolus $VERSION

 -- IOP Systems <sean@iop.systems>  $(date -R)
EOM
