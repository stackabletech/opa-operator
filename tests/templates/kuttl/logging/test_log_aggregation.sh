#!/usr/bin/env bash

DECISION_LOGS=$(kubectl logs service/test-opa-server -c opa | grep "decision_id");

if [ -n "$DECISION_LOGS" ];
then
    echo "Error: Decision logs printed to console";
    exit 1;
fi

echo "Test successful!";
