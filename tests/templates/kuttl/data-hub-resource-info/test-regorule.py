#!/usr/bin/env python
import argparse
import hashlib
import json
import os

import requests

BUSINESS = "urn:li:ownershipType:__system__business_owner"
TECHNICAL = "urn:li:ownershipType:__system__technical_owner"

# Every resource is addressed by its DataHub `instance` (platform instance), which the ingestion
# recipes in 31/32/33 set to `${POD_NAMESPACE}/<cluster>`. The kuttl namespace is only known at
# runtime, so the instances - and every URN derived from them - are computed here rather than
# hardcoded.
NAMESPACE = os.environ["POD_NAMESPACE"]
TRINO_INSTANCE = f"{NAMESPACE}/my-trino"
KAFKA_INSTANCE = f"{NAMESPACE}/test-kafka"
SUPERSET_INSTANCE = f"{NAMESPACE}/my-superset"


def container_urn(container_key):
    """Reproduce DataHub's `datahub_guid` (mirrors `container_urn` in resource_to_urn_mapping.rs):
    serialize the container key to compact, key-sorted JSON and MD5-hash it."""
    key_json = json.dumps(container_key, sort_keys=True, separators=(",", ":"))
    return f"urn:li:container:{hashlib.md5(key_json.encode()).hexdigest()}"


def trino_dataset(table):
    return f"urn:li:dataset:(urn:li:dataPlatform:trino,{TRINO_INSTANCE}.tpch.sf1.{table},PROD)"


def kafka_dataset(topic):
    return f"urn:li:dataset:(urn:li:dataPlatform:kafka,{KAFKA_INSTANCE}.{topic},PROD)"


CATALOG_URN = container_urn({"platform": "trino", "instance": TRINO_INSTANCE, "database": "tpch"})
SCHEMA_URN = container_urn(
    {"platform": "trino", "instance": TRINO_INSTANCE, "database": "tpch", "schema": "sf1"}
)

# One case per resource type of the rego library (data.test.<rule>, see the bundle in
# 10-install-opa). Each case resolves the Stackable abstraction (system + coordinates -> URN) and
# checks the full RIF response: tags, domain, data products and owners grouped by ownership type.
# The metadata is created in 34-create-metadata and differs per resource, which is what makes the
# URN derivation in resource_to_urn_mapping.rs testable: RIF answers with an empty record for a URN
# that resolves to nothing, so a wrong derivation would show up as missing metadata rather than as
# an error. The container URNs (database/schema) are opaque GUIDs, so they only match if the
# GUID computation is reproduced correctly as well.
CASES = [
    {
        "rule": "database",
        "input": {"system": "trino", "instance": TRINO_INSTANCE, "database": "tpch"},
        "urn": CATALOG_URN,
        "tags": [],
        "domain": None,
        "data_products": [],
        "owners": {TECHNICAL: {"users": [], "groups": ["urn:li:corpGroup:data-platform"]}},
    },
    {
        "rule": "schema",
        "input": {
            "system": "trino",
            "instance": TRINO_INSTANCE,
            "database": "tpch",
            "schema": "sf1",
        },
        "urn": SCHEMA_URN,
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
        "rule": "table",
        "input": {
            "system": "trino",
            "instance": TRINO_INSTANCE,
            "database": "tpch",
            "schema": "sf1",
            "table": "customer",
        },
        "urn": trino_dataset("customer"),
        "tags": ["urn:li:tag:pii"],
        "domain": "urn:li:domain:sales",
        "data_products": ["urn:li:dataProduct:order-analytics"],
        "owners": {BUSINESS: {"users": [], "groups": ["urn:li:corpGroup:sales-analytics"]}},
    },
    {
        "rule": "table",
        "input": {
            "system": "trino",
            "instance": TRINO_INSTANCE,
            "database": "tpch",
            "schema": "sf1",
            "table": "supplier",
        },
        "urn": trino_dataset("supplier"),
        "tags": ["urn:li:tag:pii"],
        "domain": "urn:li:domain:supply-chain",
        "data_products": ["urn:li:dataProduct:supplier-360"],
        "owners": {BUSINESS: {"users": [], "groups": ["urn:li:corpGroup:procurement"]}},
    },
    {
        # A reference table: tagged public, no domain, in no data product.
        "rule": "table",
        "input": {
            "system": "trino",
            "instance": TRINO_INSTANCE,
            "database": "tpch",
            "schema": "sf1",
            "table": "nation",
        },
        "urn": trino_dataset("nation"),
        "tags": ["urn:li:tag:public"],
        "domain": None,
        "data_products": [],
        "owners": {TECHNICAL: {"users": [], "groups": ["urn:li:corpGroup:data-platform"]}},
    },
    {
        # A Kafka topic: a dataset without the database/schema levels.
        "rule": "stream",
        "input": {"system": "kafka", "instance": KAFKA_INSTANCE, "queue": "orders"},
        "urn": kafka_dataset("orders"),
        "tags": ["urn:li:tag:pii"],
        "domain": "urn:li:domain:sales",
        "data_products": [],
        "owners": {BUSINESS: {"users": [], "groups": ["urn:li:corpGroup:sales-analytics"]}},
    },
    {
        # Ingested but deliberately left unannotated in 34-create-metadata: a resource DataHub knows
        # about answers with an empty record, it is not an error.
        "rule": "stream",
        "input": {"system": "kafka", "instance": KAFKA_INSTANCE, "queue": "page-views"},
        "urn": kafka_dataset("page-views"),
        "tags": [],
        "domain": None,
        "data_products": [],
        "owners": {},
    },
    {
        # The Superset seed in 23-install-superset creates exactly one chart and one dashboard in a
        # fresh Superset, so both have id 1.
        "rule": "dashboard",
        "input": {"system": "superset", "instance": SUPERSET_INSTANCE, "id": 1},
        "urn": f"urn:li:dashboard:(superset,{SUPERSET_INSTANCE}.1)",
        "tags": ["urn:li:tag:public"],
        "domain": "urn:li:domain:supply-chain",
        "data_products": [],
        "owners": {
            BUSINESS: {
                "users": ["urn:li:corpuser:carla.nowak"],
                "groups": ["urn:li:corpGroup:procurement"],
            }
        },
    },
    {
        "rule": "chart",
        "input": {"system": "superset", "instance": SUPERSET_INSTANCE, "id": 1},
        "urn": f"urn:li:chart:(superset,{SUPERSET_INSTANCE}.1)",
        "tags": [],
        "domain": None,
        "data_products": [],
        "owners": {TECHNICAL: {"users": [], "groups": ["urn:li:corpGroup:data-platform"]}},
    },
]


def tag_urns(resource):
    return sorted(tag["urn"] for tag in resource["tags"])


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

    for case in CASES:
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

        # The raw-identifier lookup bypasses the URN derivation and asks for the very URN the
        # abstraction resolved to, so both must return the identical record.
        raw = query("rawIdentifier", {"identifier": case["urn"]})
        assert raw == resource, (
            f"{case['urn']}: rawIdentifier returned {raw}, "
            f"but {case['rule']} returned {resource}"
        )

    print("Test successful!")


if __name__ == "__main__":
    main()
