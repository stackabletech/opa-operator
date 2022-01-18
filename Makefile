# =============
# This file is automatically generated from the templates in stackabletech/operator-templating
# DON'T MANUALLY EDIT THIS FILE
# =============

# This script requires https://github.com/mikefarah/yq (not to be confused with https://github.com/kislyuk/yq)
# It is available from Nixpkgs as `yq-go` (`nix shell nixpkgs#yq-go`)

.PHONY: docker chart-lint compile-chart

TAG    := $(shell git rev-parse --short HEAD)

VERSION := $(shell cargo metadata --format-version 1 | jq '.packages[] | select(.name=="stackable-opa-operator") | .version')

## Docker related targets
docker-build:
	docker build --force-rm -t "docker.stackable.tech/stackable/opa-operator:${VERSION}" -f docker/Dockerfile .

docker-build-latest: docker-build
	docker tag "docker.stackable.tech/stackable/opa-operator:${VERSION}" \
	           "docker.stackable.tech/stackable/opa-operator:latest"

docker-publish:
	echo "${NEXUS_PASSWORD}" | docker login --username github --password-stdin docker.stackable.tech
	docker push --all-tags docker.stackable.tech/stackable/opa-operator

docker: docker-build docker-publish

docker-release: docker-build-latest docker-publish

## Chart related targets
compile-chart: version crds config 

chart-clean:
	rm -rf deploy/helm/opa-operator/configs
	rm -rf deploy/helm/opa-operator/crds
	rm -rf deploy/helm/opa-operator/templates/crds.yaml

version:
	yq eval -i '.version = ${VERSION} | .appVersion = ${VERSION}' deploy/helm/opa-operator/Chart.yaml

config: deploy/helm/opa-operator/configs

deploy/helm/opa-operator/configs:
	cp -r deploy/config-spec deploy/helm/opa-operator/configs

crds: deploy/helm/opa-operator/crds/crds.yaml

deploy/helm/opa-operator/crds/crds.yaml:
	mkdir -p deploy/helm/opa-operator/crds
	cat deploy/crd/*.yaml | yq eval '.metadata.annotations["helm.sh/resource-policy"]="keep"' - > ${@}

chart-lint: compile-chart
	docker run -it -v $(shell pwd):/build/helm-charts -w /build/helm-charts quay.io/helmpack/chart-testing:v3.5.0  ct lint --config deploy/helm/ct.yaml

## Manifest related targets
clean-manifests:
	mkdir -p deploy/manifests
	rm -rf $$(find deploy/manifests -maxdepth 1 -mindepth 1 -not -name Kustomization)

generate-manifests: clean-manifests compile-chart
	./scripts/generate-manifests.sh
