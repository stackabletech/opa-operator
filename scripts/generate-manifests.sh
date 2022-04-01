#!/usr/bin/env bash
# This script reads a Helm chart from deploy/helm/opa-operator and
# generates manifest files into deploy/manifestss
set -e

tmp=$(mktemp -d ./manifests-XXXXX)

helm template --output-dir "$tmp" \
              --include-crds \
              --name-template opa-operator \
              deploy/helm/opa-operator

for file in $tmp/opa-operator/*/*; do
  yq eval -i 'del(.. | select(has("app.kubernetes.io/managed-by")) | ."app.kubernetes.io/managed-by")' /dev/stdin < "$file"
  yq eval -i 'del(.. | select(has("helm.sh/chart")) | ."helm.sh/chart")' /dev/stdin < "$file"
  sed -i '/# Source: .*/d' "$file"
done

cp -r "$tmp"/opa-operator/*/* deploy/manifests/

rm -rf "$tmp"
