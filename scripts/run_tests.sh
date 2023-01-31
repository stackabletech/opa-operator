#!/usr/bin/env bash

# Check if the test expansion tool beku is installed
set +e
which beku 2>&1 >/dev/null
beku_installed=$?
set -e
if [ $beku_installed -ne 0 ]; then
  echo "Please install beku.py to run the tests, see https://github.com/stackabletech/beku.py"
  exit 1
fi

# Expand the tests
beku

# Run tests, pass the params
pushd tests/_work
kubectl kuttl test "$@"
popd
