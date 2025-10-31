#!/usr/bin/env python
import requests
import argparse

if __name__ == "__main__":
    all_args = argparse.ArgumentParser()
    all_args.add_argument(
        "-n", "--namespace", required=True, help="Kubernetes namespace"
    )
    args = vars(all_args.parse_args())

    namespace = args["namespace"]
    metrics_url = f"https://test-opa-server-default-metrics.{namespace}.svc.cluster.local:8443/metrics"

    # Use the CA certificate for verification
    response = requests.get(metrics_url, verify="/tls/ca.crt")

    assert response.status_code == 200, "Metrics endpoint must return a 200 status code"
    assert "bundle_loaded_counter" in response.text, (
        f"Metric bundle_loaded_counter should exist in {metrics_url}"
    )
    print("Metrics test successful!")
