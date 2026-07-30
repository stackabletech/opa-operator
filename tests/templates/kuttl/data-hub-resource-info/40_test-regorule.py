#!/usr/bin/env python
"""Assert the resource-info-fetcher (RIF) answers for every resource type of the rego library.

Each case resolves a Stackable abstraction (system + coordinates -> URN, see the bundle in
10-install-opa) and compares the full RIF record. The metadata is created in 34-create-metadata and
differs per resource, which is what makes the URN derivation in resource_to_urn_mapping.rs testable:
RIF answers with an empty record for a URN that resolves to nothing, so a wrong derivation shows up
as missing metadata rather than as an error.
"""

import argparse
import hashlib
import json
import os

import requests

BUSINESS = "urn:li:ownershipType:__system__business_owner"
TECHNICAL = "urn:li:ownershipType:__system__technical_owner"

# The ingestion recipes set the DataHub `instance` (platform instance) to
# `${POD_NAMESPACE}/<cluster>`, so the instances - and every URN derived from them - are only known
# at runtime.
NAMESPACE = os.environ["POD_NAMESPACE"]
TRINO = f"{NAMESPACE}/my-trino"
KAFKA = f"{NAMESPACE}/test-kafka"
SUPERSET = f"{NAMESPACE}/my-superset"

TRINO_DB = {"system": "trino", "instance": TRINO, "database": "tpch"}
TRINO_SCHEMA = {**TRINO_DB, "schema": "sf1"}
# The Superset seed in install-superset creates exactly one chart and one dashboard in a fresh
# Superset, so both have id 1.
SUPERSET_ITEM = {"system": "superset", "instance": SUPERSET, "id": 1}


def container_urn(opa_input):
    """Reproduce DataHub's `datahub_guid` for a database/schema (mirrors `container_urn` in
    resource_to_urn_mapping.rs): the container key is the rule input with `system` renamed to
    `platform`, serialized to compact, key-sorted JSON and MD5-hashed."""
    key = {("platform" if k == "system" else k): v for k, v in opa_input.items()}
    key_json = json.dumps(key, sort_keys=True, separators=(",", ":"))
    return f"urn:li:container:{hashlib.md5(key_json.encode()).hexdigest()}"


def dataset_urn(platform, instance, name):
    return f"urn:li:dataset:(urn:li:dataPlatform:{platform},{instance}.{name},PROD)"


def owned_by(type_urn, users=(), groups=()):
    return {type_urn: {"users": sorted(users), "groups": sorted(groups)}}


def case(rule, opa_input, urn, tags=(), domain=None, data_products=(), owners=None):
    """A (rule, input, expected record) triple, expected in the shape `normalize` produces."""
    return (
        rule,
        opa_input,
        {
            "urn": urn,
            "tags": sorted(tags),
            "domain": domain,
            "dataProducts": sorted(data_products),
            "owners": owners or {},
        },
    )


def trino_table(table, **expected):
    return case(
        "table",
        {**TRINO_SCHEMA, "table": table},
        dataset_urn("trino", TRINO, f"tpch.sf1.{table}"),
        **expected,
    )


def kafka_topic(queue, **expected):
    """A Kafka topic is a dataset too, just without the database/schema levels."""
    return case(
        "stream",
        {"system": "kafka", "instance": KAFKA, "queue": queue},
        dataset_urn("kafka", KAFKA, queue),
        **expected,
    )


def normalize(record):
    """Reduce a RIF record to the URNs the cases compare against."""
    return {
        "urn": record["urn"],
        "tags": sorted(tag["urn"] for tag in record["tags"]),
        # `domain` is optional (at most one per resource); None when unassigned.
        "domain": record["domain"]["urn"] if record["domain"] else None,
        "dataProducts": sorted(product["urn"] for product in record["dataProducts"]),
        "owners": {
            type_urn: {
                "users": sorted(user["urn"] for user in bucket["users"]),
                "groups": sorted(group["urn"] for group in bucket["groups"]),
            }
            for type_urn, bucket in record["owners"].items()
        },
    }


PLATFORM_OWNED = owned_by(TECHNICAL, groups=["urn:li:corpGroup:data-platform"])

CASES = [
    case("database", TRINO_DB, container_urn(TRINO_DB), owners=PLATFORM_OWNED),
    case(
        "schema",
        TRINO_SCHEMA,
        container_urn(TRINO_SCHEMA),
        # The schema deliberately mixes a group and a user owner (both technical).
        owners=owned_by(
            TECHNICAL,
            users=["urn:li:corpuser:erin.fischer"],
            groups=["urn:li:corpGroup:data-platform"],
        ),
    ),
    trino_table(
        "customer",
        tags=["urn:li:tag:pii"],
        domain="urn:li:domain:sales",
        data_products=["urn:li:dataProduct:order-analytics"],
        owners=owned_by(BUSINESS, groups=["urn:li:corpGroup:sales-analytics"]),
    ),
    trino_table(
        "supplier",
        tags=["urn:li:tag:pii"],
        domain="urn:li:domain:supply-chain",
        data_products=["urn:li:dataProduct:supplier-360"],
        owners=owned_by(BUSINESS, groups=["urn:li:corpGroup:procurement"]),
    ),
    # A reference table: tagged public, no domain, in no data product.
    trino_table("nation", tags=["urn:li:tag:public"], owners=PLATFORM_OWNED),
    kafka_topic(
        "orders",
        tags=["urn:li:tag:pii"],
        domain="urn:li:domain:sales",
        owners=owned_by(BUSINESS, groups=["urn:li:corpGroup:sales-analytics"]),
    ),
    # Ingested but deliberately left unannotated in 34-create-metadata: a resource DataHub knows
    # about answers with an empty record, it is not an error.
    kafka_topic("page-views"),
    case(
        "dashboard",
        SUPERSET_ITEM,
        f"urn:li:dashboard:(superset,{SUPERSET}.1)",
        tags=["urn:li:tag:public"],
        domain="urn:li:domain:supply-chain",
        owners=owned_by(
            BUSINESS,
            users=["urn:li:corpuser:carla.nowak"],
            groups=["urn:li:corpGroup:procurement"],
        ),
    ),
    case(
        "chart",
        SUPERSET_ITEM,
        f"urn:li:chart:(superset,{SUPERSET}.1)",
        owners=PLATFORM_OWNED,
    ),
]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "-u",
        "--url",
        required=True,
        help="OPA data API base URL for the 'test' package",
    )
    url = parser.parse_args().url

    def query(rule, opa_input):
        # strict-builtin-errors turns a failing resource-info-fetcher call (e.g. rejected GMS
        # authentication) into a 500 rather than a silently undefined result.
        response = requests.post(
            f"{url}/{rule}",
            params={"strict-builtin-errors": "true"},
            json={"input": opa_input},
        )
        assert response.status_code == 200, (
            f"{rule}: expected 200 from OPA, got {response.status_code}: {response.text}"
        )
        body = response.json()
        # A missing 'result' means the rule was undefined - typically an auth/connection problem.
        assert "result" in body, f"{rule}: rule did not evaluate: {body}"
        return body["result"]

    for rule, opa_input, expected in CASES:
        print(f"Checking {rule}: {opa_input}")
        record = query(rule, opa_input)
        assert normalize(record) == expected, (
            f"{rule} {opa_input}: got {normalize(record)}, expected {expected}"
        )

        # The raw-identifier lookup bypasses the URN derivation and asks for the very URN the
        # abstraction resolved to, so both must return the identical record.
        raw = query("rawIdentifier", {"identifier": expected["urn"]})
        assert raw == record, (
            f"{expected['urn']}: rawIdentifier returned {raw}, but {rule} returned {record}"
        )

    print("Test successful!")


if __name__ == "__main__":
    main()
