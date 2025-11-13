#!/usr/bin/env python
import argparse
import json
from dataclasses import dataclass, field

import requests


@dataclass
class Fixture:
    expected_username: str
    expected_groups: list[str] = field(default_factory=list)
    expected_custom_attributes: dict[str, list[str]] = field(default_factory=dict)


groupofnames_fixtures = {
    "alice": Fixture(
        expected_username="alice",
        expected_groups=[
            "admins",
            "developers",
            "readers",
        ],
        expected_custom_attributes={
            "displayName": ["User1", "alice"],
            "hdir": ["/home/alice"],
            "surname": ["Bar1"],
        },
    ),
    "bob": Fixture(
        expected_username="bob",
        expected_groups=[
            "developers",
            "readers",
        ],
        expected_custom_attributes={
            "displayName": ["User2", "bob"],
            "hdir": ["/home/bob"],
            "surname": ["Bar2"],
        },
    ),
}

posixgroup_fixtures = {
    "alice": Fixture(
        expected_username="alice",
        expected_groups=[
            "posix-admins",
            "posix-developers",
        ],
        expected_custom_attributes={},
    ),
    "bob": Fixture(
        expected_username="bob",
        expected_groups=[
            "posix-developers",
        ],
        expected_custom_attributes={},
    ),
}


def assertions(
    username, response, opa_attribute, fixture, should_have_custom_attributes=False
):
    assert "result" in response
    result = response["result"]
    assert opa_attribute in result, f"expected {opa_attribute} in {result}"

    assert "customAttributes" in result[opa_attribute]
    assert "groups" in result[opa_attribute]
    assert "id" in result[opa_attribute]
    assert "username" in result[opa_attribute]

    assert result[opa_attribute]["username"] == fixture.expected_username, (
        f"for {username=} got user name {result[opa_attribute]['username']}, expected: {fixture.expected_username}"
    )

    groups = sorted(result[opa_attribute]["groups"])
    expected_groups = sorted(fixture.expected_groups)
    assert groups == expected_groups, (
        f"for {username=} got {groups=}, expected: {expected_groups=}"
    )

    custom_attributes = result[opa_attribute]["customAttributes"]
    if should_have_custom_attributes:
        assert custom_attributes == fixture.expected_custom_attributes, (
            f"for {username=} got {custom_attributes=}, expected: {fixture.expected_custom_attributes}"
        )
    else:
        # For clusters without custom attribute mappings, should be empty
        assert custom_attributes == {}, (
            f"for {username=} expected empty custom attributes but got {custom_attributes=}"
        )


def test_user_not_found(url):
    params = {"strict-builtin-errors": "true"}
    expected_status_code = 200

    payload = {"input": {"username": "nonexistent"}}
    response = requests.post(url, data=json.dumps(payload), params=params)
    assert response.status_code == expected_status_code, (
        f"got {response.status_code}, expected: {expected_status_code}"
    )
    response = response.json()
    assert "result" in response
    result = response["result"]
    assert "currentUserInfoByUsername" in result
    assert "error" in result["currentUserInfoByUsername"]
    error = result["currentUserInfoByUsername"]["error"]
    assert "message" in error
    assert error["message"] == "failed to get user information from OpenLDAP"
    assert "causes" in error
    assert error["causes"][0] == 'unable to find user with username "nonexistent"'

    payload = {"input": {"id": "00000000-0000-0000-0000-000000000000"}}
    response = requests.post(url, data=json.dumps(payload), params=params)
    assert response.status_code == expected_status_code, (
        f"got {response.status_code}, expected: {expected_status_code}"
    )
    response = response.json()
    assert "result" in response
    result = response["result"]
    assert "currentUserInfoById" in result
    assert "error" in result["currentUserInfoById"]
    error = result["currentUserInfoById"]["error"]
    assert "message" in error
    assert error["message"] == "failed to get user information from OpenLDAP"
    assert "causes" in error
    assert (
        error["causes"][0]
        == 'unable to find user with id "00000000-0000-0000-0000-000000000000"'
    )


if __name__ == "__main__":
    all_args = argparse.ArgumentParser()
    all_args.add_argument("-u", "--url", required=True, help="OPA service url")
    all_args.add_argument(
        "-t",
        "--test-type",
        required=True,
        choices=["groupofnames-tls", "groupofnames-notls", "posixgroup-tls"],
        help="Type of test to run",
    )
    args = vars(all_args.parse_args())
    params = {"strict-builtin-errors": "true"}

    # Select the appropriate fixtures based on test type
    if args["test_type"].startswith("groupofnames"):
        fixtures = groupofnames_fixtures
        # Only groupofnames-tls has custom attribute mappings configured
        has_custom_attributes = args["test_type"] == "groupofnames-tls"
    else:
        fixtures = posixgroup_fixtures
        has_custom_attributes = False

    def make_request(payload):
        response = requests.post(args["url"], data=json.dumps(payload), params=params)
        expected_status_code = 200
        assert response.status_code == expected_status_code, (
            f"got {response.status_code}, expected: {expected_status_code}"
        )
        return response.json()

    for username, fixture in fixtures.items():
        try:
            # Test by username
            payload = {"input": {"username": username}}
            response = make_request(payload)
            assertions(
                username,
                response,
                "currentUserInfoByUsername",
                fixture,
                has_custom_attributes,
            )

            # Test by ID (reverse lookup)
            user_id = response["result"]["currentUserInfoByUsername"]["id"]
            payload = {"input": {"id": user_id}}
            response = make_request(payload)
            assertions(
                username,
                response,
                "currentUserInfoById",
                fixture,
                has_custom_attributes,
            )
        except Exception as e:
            print(f"exception: {e}")
            if response is not None:
                print(f"request  body: {payload}")
                print(f"response body: {response}")
            raise e

    # Test user not found scenarios
    try:
        print(f"Testing user not found scenarios for {args['test_type']}...")
        test_user_not_found(args["url"])
        print("User not found tests passed!")
    except Exception as e:
        print(f"User not found test failed: {e}")
        raise e

    print(f"All tests passed for {args['test_type']}!")
