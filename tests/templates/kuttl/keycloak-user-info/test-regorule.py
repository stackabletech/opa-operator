#!/usr/bin/env python
import requests
import argparse
import json


if __name__ == "__main__":
    all_args = argparse.ArgumentParser()
    all_args.add_argument("-u", "--url", required=True, help="OPA service url")
    args = vars(all_args.parse_args())

    # url = 'http://test-opa-svc:8081/v1/data/currentUserInfo'
    params = {'strict-builtin-errors': 'true'}
    payload = {'input': {'username': 'admin'}}
    response = requests.post(args['url'], data=json.dumps(payload), params=params).json()

    expected_response = {
        'customAttributes': {},
        'groups': [],
        'roles': [
            {'name': 'admin'},
            {'name': 'create-realm'},
            {'name': 'default-roles-master'},
            {'name': 'offline_access'},
            {'name': 'uma_authorization'},
        ],
    }

    # Sort lists since their order is not well-defined..
    response.get("result", {}).get("groups", []).sort(key=lambda group: group.get("name", ""))
    response.get("result", {}).get("roles", []).sort(key=lambda role: role.get("name", ""))

    if "result" in response and response["result"] == expected_response:
        print("Test successful!")
        exit(0)
    else:
        print(f"Error: received {response} - expected: {({'result': expected_response})}")
        exit(-1)
