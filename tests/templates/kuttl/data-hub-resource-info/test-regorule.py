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


BUSINESS = "urn:li:ownershipType:__system__business_owner"
TECHNICAL = "urn:li:ownershipType:__system__technical_owner"

# Trino resources resolved via the Stackable abstraction (stacklet + coordinates -> URN). The
# metadata is created in 22-create-metadata. These exercise the full RIF response - tags, domain,
# data products and owners grouped by ownership type - across the catalog/schema/table levels, and
# prove the container-URN derivation in resource_to_urn_mapping.rs (catalog/schema URNs are opaque
# GUIDs, so a wrong derivation would resolve to a different - empty - entity).
TRINO_CASES = [
    {
        "rule": "trinoCatalog",
        "input": {"env": "PROD", "stacklet": "trino", "catalog": "tpch"},
        "urn": "urn:li:container:6a39142fc39af8ec4ec5340eb21c1dee",
        "tags": [],
        "domain": None,
        "data_products": [],
        "owners": {TECHNICAL: {"users": [], "groups": ["urn:li:corpGroup:data-platform"]}},
    },
    {
        "rule": "trinoSchema",
        "input": {"env": "PROD", "stacklet": "trino", "catalog": "tpch", "schema": "sf1"},
        "urn": "urn:li:container:727821ddae4cbef3856d53190f82489c",
        "tags": [],
        "domain": None,
        "data_products": [],
        # The schema deliberately mixes a group and a user owner (both technical).
        "owners": {
            TECHNICAL: {
                "users": ["urn:li:corpuser:erin.fischer"],
                "groups": ["urn:li:corpGroup:data-platform"],
            }
        },
    },
    {
        "rule": "trinoTable",
        "input": {"env": "PROD", "stacklet": "trino", "catalog": "tpch", "schema": "sf1", "table": "customer"},
        "urn": "urn:li:dataset:(urn:li:dataPlatform:trino,tpch.sf1.customer,PROD)",
        "tags": ["urn:li:tag:pii"],
        "domain": "urn:li:domain:sales",
        "data_products": ["urn:li:dataProduct:order-analytics"],
        "owners": {BUSINESS: {"users": [], "groups": ["urn:li:corpGroup:sales-analytics"]}},
    },
    {
        "rule": "trinoTable",
        "input": {"env": "PROD", "stacklet": "trino", "catalog": "tpch", "schema": "sf1", "table": "supplier"},
        "urn": "urn:li:dataset:(urn:li:dataPlatform:trino,tpch.sf1.supplier,PROD)",
        "tags": ["urn:li:tag:pii"],
        "domain": "urn:li:domain:supply-chain",
        "data_products": ["urn:li:dataProduct:supplier-360"],
        "owners": {BUSINESS: {"users": [], "groups": ["urn:li:corpGroup:procurement"]}},
    },
    {
        # A reference table: tagged public, no domain, in no data product.
        "rule": "trinoTable",
        "input": {"env": "PROD", "stacklet": "trino", "catalog": "tpch", "schema": "sf1", "table": "nation"},
        "urn": "urn:li:dataset:(urn:li:dataPlatform:trino,tpch.sf1.nation,PROD)",
        "tags": ["urn:li:tag:public"],
        "domain": None,
        "data_products": [],
        "owners": {TECHNICAL: {"users": [], "groups": ["urn:li:corpGroup:data-platform"]}},
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


def domain_urn(resource):
    # `domain` is optional (at most one per resource); None when unassigned.
    return resource["domain"]["urn"] if resource["domain"] else None


def data_product_urns(resource):
    return sorted(data_product["urn"] for data_product in resource["dataProducts"])


def owners_by_type(resource):
    # Reshape `owners` to {ownershipTypeUrn: {"users": [...], "groups": [...]}} for comparison.
    return {
        type_urn: {
            "users": sorted(user["urn"] for user in bucket["users"]),
            "groups": sorted(group["urn"] for group in bucket["groups"]),
        }
        for type_urn, bucket in resource["owners"].items()
    }


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
    kafka_topic = query("kafkaTopic", {"env": "PROD", "stacklet": "kafka", "topic": "SampleKafkaDataset"})
    assert_resource(
        kafka_topic,
        "urn:li:dataset:(urn:li:dataPlatform:kafka,SampleKafkaDataset,PROD)",
        [],
        SAMPLE_OWNERS,
    )

    # 3) Exercise the Trino catalog/schema/table abstractions against the metadata from
    #    22-create-metadata, checking the full RIF response (tags, domain, data products, owners).
    for case in TRINO_CASES:
        print(f"Checking {case['rule']}: {case['input']}")
        resource = query(case["rule"], case["input"])
        assert resource["urn"] == case["urn"], (
            f"{case['rule']}: expected urn {case['urn']}, got {resource['urn']}"
        )
        assert tag_urns(resource) == sorted(case["tags"]), (
            f"{case['urn']}: tags {tag_urns(resource)} != expected {sorted(case['tags'])}"
        )
        assert domain_urn(resource) == case["domain"], (
            f"{case['urn']}: domain {domain_urn(resource)} != expected {case['domain']}"
        )
        assert data_product_urns(resource) == sorted(case["data_products"]), (
            f"{case['urn']}: data products {data_product_urns(resource)} "
            f"!= expected {sorted(case['data_products'])}"
        )
        assert owners_by_type(resource) == case["owners"], (
            f"{case['urn']}: owners {owners_by_type(resource)} != expected {case['owners']}"
        )

    print("Test successful!")


if __name__ == "__main__":
    main()
