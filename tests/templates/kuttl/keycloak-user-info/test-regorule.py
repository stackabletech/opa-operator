#!/usr/bin/env python
import requests
import argparse
import json

# test table for users
# alice and bob by name first, then by id


if __name__ == "__main__":
    all_args = argparse.ArgumentParser()
    all_args.add_argument("-u", "--url", required=True, help="OPA service url")
    args = vars(all_args.parse_args())

    # todo: we removed currentUserInfo frin the path
    # todo: try this out locally until it works
    # url = 'http://test-opa-svc:8081/v1/data/currentUserInfo'
    params = {'strict-builtin-errors': 'true'}
    payload = {'input': {'username': 'alice'}}
    response = requests.post(args['url'], data=json.dumps(payload), params=params).json()

    expected_response = {
        'customAttributes': {},
        'groups': ["/superset-admin"],
    }

    # Sort lists since their order is not well-defined..
    response.get("result", {}).get("groups", []).sort(key=lambda group: group.get("name", ""))

    if "result" in response and response["result"] == expected_response:
        print("Test successful!")
        exit(0)
    else:
        print(f"Error: received {response} - expected: {({'result': expected_response})}")
        exit(-1)


received: {'result': {'currentUserInfoByUsername': {'customAttributes': {}, 'groups': ['/superset-admin'], 'id': '1680586a-f61d-4624-acf0-d32f3adb4427', 'username': 'alice'}}}
expected: {'result': {'customAttributes': {}, 'groups': ['/g1']}}