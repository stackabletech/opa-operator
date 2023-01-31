#!/usr/bin/env bash
set -e

# Install and run beku, the tool to expand the test templates
# (https://github.com/stackabletech/beku.py)
pip install beku-stackabletech
beku

# Run tests, pass the params
pushd tests/_work
kubectl kuttl test "$@"
popd
