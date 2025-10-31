#!/usr/bin/env python
import requests
import argparse


if __name__ == "__main__":
    all_args = argparse.ArgumentParser()
    all_args.add_argument("-u", "--url", required=True, help="OPA service url")
    args = vars(all_args.parse_args())

    # rego rule to check (compare: 01-install-opa.yaml)
    # ---
    # package test
    #
    # hello {
    #     true
    # }
    #
    # world {
    #     false
    # }
    # ---
    # We need to query: https://<host>:<port>/v1/data/<package>/(<rule>)+
    # In our case https://<host>:8443/v1/data/test
    # --> {'result': {'hello': True}}
    # or https://<host>:8443/v1/data/test/hello
    # --> {'hello': True}
    # For HTTP: http://<host>:8081/v1/data/test

    # Determine verification setting based on whether TLS is used
    if args["url"].startswith("http://"):
        verify = False
        protocol = "HTTP"
    else:
        verify = "/tls/ca.crt"
        protocol = "HTTPS"

    response = requests.post(args["url"], json={"input": {}}, verify=verify).json()

    if (
        "result" in response
        and "hello" in response["result"]
        and response["result"]["hello"]
    ):
        print(f"Regorule test ({protocol}) successful!")
        exit(0)
    else:
        print(
            f"Error ({protocol}): received "
            + str(response)
            + " - expected: {'result': {'hello': True}}"
        )
        exit(-1)
