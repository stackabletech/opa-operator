#!/usr/bin/env python
import requests
import argparse
import json


def assertions(username, response, opa_attribute, expected_groups, expected_attributes={}):
    assert "result" in response
    assert opa_attribute in response["result"]

    # repeated the right hand side for better output on error
    assert "customAttributes" in response["result"][opa_attribute]
    assert "groups" in response["result"][opa_attribute]
    assert "id" in response["result"][opa_attribute]
    assert "username" in response["result"][opa_attribute]

    # todo: split out group assertions
    print(f"Testing for {username} in groups {expected_groups}")
    groups = sorted(response["result"][opa_attribute]["groups"])
    expected_groups = sorted(expected_groups)
    assert groups == expected_groups, f"got {groups}, expected: {expected_groups}"

    # todo: split out customAttribute assertions
    print(f"Testing for {username} with customAttributes {expected_attributes}")
    custom_attributes = response["result"][opa_attribute]["customAttributes"]
    assert custom_attributes == expected_attributes, f"got {custom_attributes}, expected: {expected_attributes}"


if __name__ == "__main__":
    all_args = argparse.ArgumentParser()
    all_args.add_argument("-u", "--url", required=True, help="OPA service url")
    args = vars(all_args.parse_args())
    params = {'strict-builtin-errors': 'true'}

    def make_request(payload):
        return requests.post(args['url'], data=json.dumps(payload), params=params).json()

    for subject_id in ["alice", "bob"]:
        try:
            # todo: try this out locally until it works
            # url = 'http://test-opa-svc:8081/v1/data'
            payload = {'input': {'id': subject_id}}
            response = make_request(payload)
            assertions(subject_id, response, "currentUserInfoById", [], {"e-mail": f"{subject_id}@example.com", "company": "openid"})
        except Exception as e:
            if response is not None:
                print(f"something went wrong. last response: {response}")
            raise e

    print("Test successful!")
