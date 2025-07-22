#!/usr/bin/env python
import requests

# metrics_url = "http://test-opa-server-default:9504/metrics"
metrics_url = "http://test-opa-server-default-metrics:8081/metrics"
# FIXME: Ideally this would be exposed via a metrics service (as the other operators do)
response = requests.get(metrics_url)

assert response.status_code == 200, "Metrics endpoint must return a 200 status code"
assert "bundle_loaded_counter" in response.text, f"Metric bundle_loaded_counter should exist in {metrics_url}"

print("Metrics test successful!")
