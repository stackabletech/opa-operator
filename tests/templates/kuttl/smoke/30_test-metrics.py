#!/usr/bin/env python
import requests
import argparse

if __name__ == "__main__":
    all_args = argparse.ArgumentParser()
    all_args.add_argument("-u", "--url", required=True, help="OPA metrics url")
    args = vars(all_args.parse_args())

    metrics_url = args["url"]

    # Determine verification setting based on whether TLS is used
    if metrics_url.startswith("http://"):
        verify = False
        protocol = "HTTP"
    else:
        verify = "/tls/ca.crt"
        protocol = "HTTPS"

    response = requests.get(metrics_url, verify=verify)

    assert response.status_code == 200, "Metrics endpoint must return a 200 status code"
    assert "bundle_loaded_counter" in response.text, (
        f"Metric bundle_loaded_counter should exist in {metrics_url}"
    )
    print(f"Metrics test ({protocol}) successful!")
