#!/usr/bin/env python
import requests
import argparse
import json

# todo: make the test more comprehensive to check customAttributes
users_and_groups = {
    "alice": ["/superset-admin"],
    "bob": [],
}


def assertions(
    username, response, opa_attribute, expected_groups, expected_attributes={}
):
    assert "result" in response
    result = response["result"]
    assert opa_attribute in result, f"expected {opa_attribute} in {result}"

    # repeated the right hand side for better output on error
    assert "customAttributes" in result[opa_attribute]
    assert "groups" in result[opa_attribute]
    assert "id" in result[opa_attribute]
    assert "username" in result[opa_attribute]

    # todo: split out group assertions
    print(f"Testing for {username} in groups {expected_groups}")
    groups = sorted(result[opa_attribute]["groups"])
    expected_groups = sorted(expected_groups)
    assert groups == expected_groups, f"got {groups}, expected: {expected_groups}"

    # todo: split out customAttribute assertions
    print(f"Testing for {username} with customAttributes {expected_attributes}")
    custom_attributes = result[opa_attribute]["customAttributes"]
    assert (
        custom_attributes == expected_attributes
    ), f"got {custom_attributes}, expected: {expected_attributes}"


if __name__ == "__main__":
    all_args = argparse.ArgumentParser()
    all_args.add_argument("-u", "--url", required=True, help="OPA service url")
    args = vars(all_args.parse_args())
    params = {"strict-builtin-errors": "true"}

    def make_request(payload):
        response = requests.post(args["url"], data=json.dumps(payload), params=params)
        expected_status_code = 200
        assert (
            response.status_code == expected_status_code
        ), f"got {response.status_code}, expected: {expected_status_code}"
        return response.json()

    for username, groups in users_and_groups.items():
        try:
            # todo: try this out locally until it works
            # url = 'http://test-opa-svc:8081/v1/data'
            payload = {"input": {"username": username}}
            response = make_request(payload)
            assertions(username, response, "currentUserInfoByUsername", groups, {})

            # do the reverse lookup
            user_id = response["result"]["currentUserInfoByUsername"]["id"]
            payload = {"input": {"id": user_id}}
            response = make_request(payload)
            assertions(username, response, "currentUserInfoById", groups, {})
        except Exception as e:
            print(f"exception: {e}")
            if response is not None:
                print(f"request  body: {payload}")
                print(f"response body: {response}")
            raise e

    print("Test successful!")
