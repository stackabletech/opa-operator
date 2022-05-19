#!/bin/bash
git clone -b "$GIT_BRANCH" https://github.com/stackabletech/opa-operator.git
(cd opa-operator/ && ./scripts/run_tests.sh)
exit_code=$?
./operator-logs.sh opa > /target/opa-operator.log
./operator-logs.sh regorule > /target/regorule-operator.log
exit $exit_code
