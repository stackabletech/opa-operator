#! /usr/bin/env bash
set -euo pipefail

# This script contains all the code snippets from the guide, as well as some assert tests
# to test if the instructions in the guide work. The user *could* use it, but it is intended
# for testing only.
# The script will install the operator, create an OPA instance and briefly open a port
# forward and query the OPA.
# No running processes are left behind (i.e. the port-forwarding is closed at the end)

if [ $# -eq 0 ]
then
  echo "Installation method argument ('helm' or 'stackablectl') required."
  exit 1
fi

case "$1" in
"helm")
echo "Adding 'stackable-dev' Helm Chart repository"
# tag::helm-add-repo[]
helm repo add stackable-dev https://repo.stackable.tech/repository/helm-dev/
# end::helm-add-repo[]
echo "Updating Helm repo"
helm repo update

echo "Installing Operators with Helm"
# tag::helm-install-operators[]
helm install --wait opa-operator stackable-dev/opa-operator --version 0.0.0-dev
# end::helm-install-operators[]
;;
"stackablectl")
echo "installing Operators with stackablectl"
# tag::stackablectl-install-operators[]
stackablectl operator install opa=0.0.0-dev
# end::stackablectl-install-operators[]
;;
*)
echo "Need to provide 'helm' or 'stackablectl' as an argument for which installation method to use!"
exit 1
;;
esac

echo "Creating OPA cluster"
# tag::apply-opa-cluster[]
kubectl apply -f opa.yaml
# end::apply-opa-cluster[]

sleep 15

echo "Waiting on rollout ..."
kubectl rollout status --watch --timeout=5m daemonset/simple-opa-server-default

echo "Applying the rule file"
# tag::apply-rule-file[]
kubectl apply -f simple-rule.yaml
# end::apply-rule-file[]

# The bundle builder will update the bundle almost immediately, but OPA can take up to
# max_delay_seconds: 20 (see ConfigMap)
# to poll the bundle
sleep 21

echo "Starting port-forwarding of port 8081"
# tag::port-forwarding[]
kubectl port-forward svc/simple-opa 8081 > /dev/null 2>&1 &
# end::port-forwarding[]
PORT_FORWARD_PID=$!
trap "kill $PORT_FORWARD_PID" EXIT
sleep 5

request_hello() {
# tag::request-hello[]
curl -s http://localhost:8081/v1/data/test/hello
# end::request-hello[]
}

echo "Checking policy decision for 'hello' rule ..."
test_hello=$(request_hello)
if [ "$test_hello" == "$(cat expected_response_hello.json)" ]; then
  echo "The 'hello' rule returned the correct response!"
else
  echo "The 'hello' rule returned an incorrect response."
  echo "Received: $test_hello"
  echo "Expected: $(cat expected_response_hello.json)"
  exit 1
fi

request_world() {
# tag::request-world[]
curl -s http://localhost:8081/v1/data/test/world
# end::request-world[]
}

echo "Checking policy decision for 'world' rule ..."
test_world=$(request_world)
if [ "$test_world" == "$(cat expected_response_world.json)" ]; then
  echo "The 'world' rule returned the correct response!"
else
  echo "The 'world' rule returned an incorrect response."
  echo "Received: $test_world"
  echo "Expected: $(cat expected_response_world.json)"
  exit 1
fi
