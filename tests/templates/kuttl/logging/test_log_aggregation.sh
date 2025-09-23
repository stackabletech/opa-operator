#!/usr/bin/env bash

# we should distinguish between cases where the variable is not set
# if the service or container is not found (which we don't want), and where
# grep does not find anything (which is what we *do* want).

set -Eeuo pipefail

# do not mask kubectl errors, but the right-hand group must always be true
# so the test does not fail on the grep
DECISION_LOGS=$(kubectl logs service/test-opa-server -c opa | { grep "decision_id" || true; })

if [ -n "$DECISION_LOGS" ];
then
    echo "Error: Decision logs printed to console";
    exit 1;
fi

echo "Test successful!";
