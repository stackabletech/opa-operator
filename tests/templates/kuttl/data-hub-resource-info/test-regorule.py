#!/usr/bin/env python
import argparse
import json

import requests

# Every dataset in DataHub's built-in sample data (loaded in 04-load-sample-data) carries the
# same two owners under the "data owner" ownership type.
SAMPLE_OWNERS = ["urn:li:corpuser:datahub", "urn:li:corpuser:jdoe"]

# Resources looked up by their raw DataHub URN, with the metadata we expect back. Values were
# captured from a live fetcher response against the sample data.
BY_URN_CASES = [
    {
        "urn": "urn:li:dataset:(urn:li:dataPlatform:hive,SampleHiveDataset,PROD)",
        "tags": ["urn:li:tag:Legacy"],
        "owners": SAMPLE_OWNERS,
    },
    {
        "urn": "urn:li:dataset:(urn:li:dataPlatform:kafka,SampleKafkaDataset,PROD)",
        "tags": [],
        "owners": SAMPLE_OWNERS,
    },
    {
        "urn": "urn:li:dataset:(urn:li:dataPlatform:hdfs,SampleHdfsDataset,PROD)",
        "tags": [],
        "owners": SAMPLE_OWNERS,
    },
]


def tag_urns(resource):
    return sorted(tag["urn"] for tag in resource["tags"])


def owner_urns(resource):
    # `owners` is keyed by ownership type; collect the user and group urns underneath each.
    urns = []
    for ownership in resource["owners"].values():
        urns += [user["urn"] for user in ownership["users"]]
        urns += [group["urn"] for group in ownership["groups"]]
    return sorted(urns)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "-u", "--url", required=True, help="OPA data API base URL for the 'test' package"
    )
    args = parser.parse_args()

    def query(rule, opa_input):
        # strict-builtin-errors turns a failing resource-info-fetcher call (e.g. rejected GMS
        # authentication) into a 500 rather than a silently undefined result.
        response = requests.post(
            f"{args.url}/{rule}",
            params={"strict-builtin-errors": "true"},
            data=json.dumps({"input": opa_input}),
        )
        assert response.status_code == 200, (
            f"{rule}: expected 200 from OPA, got {response.status_code}: {response.text}"
        )
        body = response.json()
        # A missing 'result' means the rule was undefined - typically an auth/connection problem.
        assert "result" in body, f"{rule}: rule did not evaluate: {body}"
        return body["result"]

    def assert_resource(resource, urn, expected_tags, expected_owners):
        assert resource["urn"] == urn, f"expected urn {urn}, got {resource['urn']}"
        assert tag_urns(resource) == sorted(expected_tags), (
            f"{urn}: tags {tag_urns(resource)} != expected {sorted(expected_tags)}"
        )
        assert owner_urns(resource) == sorted(expected_owners), (
            f"{urn}: owners {owner_urns(resource)} != expected {sorted(expected_owners)}"
        )

    # 1) Resolve several resources by their raw DataHub URN.
    for case in BY_URN_CASES:
        print(f"Checking byUrn: {case['urn']}")
        resource = query("byUrn", {"urn": case["urn"]})
        assert_resource(resource, case["urn"], case["tags"], case["owners"])

    # 2) Exercise the Stackable KafkaTopic abstraction: the fetcher maps (stacklet, topic) to a
    #    DataHub URN and resolves it. It must return the same record as the raw-URN lookup above,
    #    which proves the mapping in resource_to_urn_mapping.rs is correct end-to-end.
    print("Checking kafkaTopic abstraction: kafka / SampleKafkaDataset")
    kafka_topic = query("kafkaTopic", {"stacklet": "kafka", "topic": "SampleKafkaDataset"})
    assert_resource(
        kafka_topic,
        "urn:li:dataset:(urn:li:dataPlatform:kafka,SampleKafkaDataset,PROD)",
        [],
        SAMPLE_OWNERS,
    )

    print("Test successful!")


if __name__ == "__main__":
    main()
