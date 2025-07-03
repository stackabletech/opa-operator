#!/usr/bin/env python
import argparse
import json
from dataclasses import dataclass, field

import requests


@dataclass
class Fixture:
    expected_username: str
    expected_groups: list[str] = field(default_factory=list)
    expected_custom_attributes: dict[str, str] = field(default_factory=dict)


fixtures = {
    "alice@sble.test": Fixture(
        expected_username="alice@sble.test",
        expected_groups=[
            "CN=Superset Admins,CN=Users,DC=sble,DC=test",
            "CN=Domain Users,CN=Users,DC=sble,DC=test",
            "CN=Users,CN=Builtin,DC=sble,DC=test",
        ],
    ),
    "alice": Fixture(
        expected_username="alice@sble.test",
        expected_groups=[
            "CN=Superset Admins,CN=Users,DC=sble,DC=test",
            "CN=Domain Users,CN=Users,DC=sble,DC=test",
            "CN=Users,CN=Builtin,DC=sble,DC=test",
        ],
    ),
    "sam-alice": Fixture(
        expected_username="alice@sble.test",
        expected_groups=[
            "CN=Superset Admins,CN=Users,DC=sble,DC=test",
            "CN=Domain Users,CN=Users,DC=sble,DC=test",
            "CN=Users,CN=Builtin,DC=sble,DC=test",
        ],
    ),
    "bob@sble.test": Fixture(
        expected_username="bob@SBLE.TEST",
        expected_groups=[
            "CN=Domain Users,CN=Users,DC=sble,DC=test",
            "CN=Users,CN=Builtin,DC=sble,DC=test",
        ],
    ),
    "charlie@CUSTOM.TEST": Fixture(
        expected_username="charlie@custom.test",
        expected_groups=[
            "CN=Domain Users,CN=Users,DC=sble,DC=test",
            "CN=Users,CN=Builtin,DC=sble,DC=test",
        ],
    ),
}


def assertions(username, response, opa_attribute, fixture):
    assert "result" in response
    result = response["result"]
    # print(result)
    assert opa_attribute in result, f"expected {opa_attribute} in {result}"

    # repeated the right hand side for better output on error
    assert "customAttributes" in result[opa_attribute]
    assert "groups" in result[opa_attribute]
    assert "id" in result[opa_attribute]
    assert "username" in result[opa_attribute]

    assert result[opa_attribute]["username"] == fixture.expected_username, (
        f"for {username=} got user name {result[opa_attribute]['username']}, expected: {fixture.expected_username}"
    )

    # todo: split out group assertions
    groups = sorted(result[opa_attribute]["groups"])
    expected_groups = sorted(fixture.expected_groups)
    assert groups == expected_groups, (
        f"for {username=} got {groups=}, expected: {expected_groups=}"
    )

    # todo: split out customAttribute assertions
    custom_attributes = result[opa_attribute]["customAttributes"]
    assert custom_attributes == fixture.expected_custom_attributes, (
        f"for {username=} got {custom_attributes=}, expected: {fixture.expected_custom_attributes}"
    )


if __name__ == "__main__":
    all_args = argparse.ArgumentParser()
    all_args.add_argument("-u", "--url", required=True, help="OPA service url")
    args = vars(all_args.parse_args())
    params = {"strict-builtin-errors": "true"}

    def make_request(payload):
        response = requests.post(args["url"], data=json.dumps(payload), params=params)
        expected_status_code = 200
        assert response.status_code == expected_status_code, (
            f"got {response.status_code}, expected: {expected_status_code}"
        )
        return response.json()

    for username, fixture in fixtures.items():
        try:
            # todo: try this out locally until it works
            # url = 'http://test-opa-svc:8081/v1/data'
            payload = {"input": {"username": username}}
            response = make_request(payload)
            assertions(username, response, "currentUserInfoByUsername", fixture)

            # do the reverse lookup
            user_id = response["result"]["currentUserInfoByUsername"]["id"]
            payload = {"input": {"id": user_id}}
            response = make_request(payload)
            assertions(username, response, "currentUserInfoById", fixture)
        except Exception as e:
            print(f"exception: {e}")
            if response is not None:
                print(f"request  body: {payload}")
                print(f"response body: {response}")
            raise e

    print("Test successful!")
