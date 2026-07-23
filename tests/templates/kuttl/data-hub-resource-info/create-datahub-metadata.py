#!/usr/bin/env python
"""Populate DataHub with the test users, groups, tags, domains, data products and assignments
that the resource-info-fetcher (RIF) test asserts on.

Everything is attached to the Trino `tpch.sf1` catalog/schema/tables ingested in 21-scrape-trino.
Writes go to the GMS OpenAPI v3 entity endpoint as UPSERTs (createIfNotExists=false), so the
script is idempotent / re-runnable. async=false makes a rejected write fail loudly.

The model (see the RIF response contract in rust/resource-info-fetcher/src/backend/data_hub):
  - 3 groups own the two halves of the TPC-H model plus the shared reference layer.
  - 5 users are members of those groups.
  - tag `pii` on customer+supplier, `public` on nation+region (rest untagged for contrast).
  - domain Sales (customer/orders/lineitem) and Supply Chain (supplier/part/partsupp).
  - data products Order Analytics (Sales) and Supplier 360 (Supply Chain).
  - ownership at catalog + schema (technical) and table (business) level; the schema mixes a
    group *and* a user owner so the RIF's owner-by-type resolution is fully exercised.
"""

import os
import sys
import time

import requests

GMS = os.environ.get("GMS", "http://datahub-datahub-gms:8080")
TOKEN = os.environ["DATAHUB_GMS_TOKEN"]
SESSION = requests.Session()
SESSION.headers.update({"Authorization": f"Bearer {TOKEN}", "Content-Type": "application/json"})

BUSINESS = "urn:li:ownershipType:__system__business_owner"
TECHNICAL = "urn:li:ownershipType:__system__technical_owner"

# Deterministic URNs from the Trino ingestion (platform=trino, env=PROD -> instance=PROD).
# These match resource_to_urn_mapping.rs and were verified against a live DataHub.
CATALOG = "urn:li:container:6a39142fc39af8ec4ec5340eb21c1dee"  # tpch
SCHEMA = "urn:li:container:727821ddae4cbef3856d53190f82489c"  # tpch.sf1


def dataset(table):
    return f"urn:li:dataset:(urn:li:dataPlatform:trino,tpch.sf1.{table},PROD)"


def upsert(entity_type, objects):
    """UPSERT a list of {urn, <aspect>:{value:...}} objects to /openapi/v3/entity/<type>."""
    resp = SESSION.post(
        f"{GMS}/openapi/v3/entity/{entity_type}",
        params={"async": "false", "createIfNotExists": "false"},
        json=objects,
    )
    if not resp.ok:
        sys.exit(f"ERROR upserting {entity_type}: HTTP {resp.status_code}: {resp.text}")


def ownership_aspect(owners):
    """Build an `ownership` aspect value from a list of (owner_urn, ownership_type_urn) pairs."""
    owner_types = {}
    for owner_urn, type_urn in owners:
        owner_types.setdefault(type_urn, []).append(owner_urn)
    return {
        "value": {
            "owners": [
                {
                    "owner": owner_urn,
                    "typeUrn": type_urn,
                    "type": "NONE",
                    "source": {"type": "MANUAL"},
                }
                for owner_urn, type_urn in owners
            ],
            "ownerTypes": owner_types,
            "lastModified": {"actor": "urn:li:corpuser:datahub", "time": 0},
        }
    }


def wait_for_health():
    for attempt in range(60):
        try:
            if SESSION.get(f"{GMS}/health", timeout=10).ok:
                return
        except requests.RequestException:
            pass
        print(f"  GMS not ready yet (attempt {attempt + 1}/60), retrying in 5s")
        time.sleep(5)
    sys.exit("GMS did not become healthy in time")


# --- The test fixture ----------------------------------------------------------------------------

# group id -> display name
GROUPS = {
    "sales-analytics": "Sales Analytics",
    "procurement": "Procurement",
    "data-platform": "Data Platform",
}

# username -> (full name, group id)
USERS = {
    "alice.turner": ("Alice Turner", "sales-analytics"),
    "bob.ramirez": ("Bob Ramirez", "sales-analytics"),
    "carla.nowak": ("Carla Nowak", "procurement"),
    "david.okoye": ("David Okoye", "procurement"),
    "erin.fischer": ("Erin Fischer", "data-platform"),
}

# tag id -> display name
TAGS = {"pii": "PII", "public": "Public"}

# domain id -> (display name, description)
DOMAINS = {
    "sales": ("Sales", "Sales and order data"),
    "supply-chain": ("Supply Chain", "Suppliers, parts and procurement data"),
}

# data product id -> (display name, description, domain id, [asset tables])
DATA_PRODUCTS = {
    "order-analytics": (
        "Order Analytics",
        "Curated order analytics datasets",
        "sales",
        ["customer", "orders", "lineitem"],
    ),
    "supplier-360": (
        "Supplier 360",
        "Supplier, part and procurement datasets",
        "supply-chain",
        ["supplier", "part", "partsupp"],
    ),
}

# table -> (tag ids, domain id or None, [(owner urn, ownership type urn)])
TABLES = {
    "customer": (["pii"], "sales", [("urn:li:corpGroup:sales-analytics", BUSINESS)]),
    "orders": ([], "sales", [("urn:li:corpGroup:sales-analytics", BUSINESS)]),
    "lineitem": ([], "sales", [("urn:li:corpGroup:sales-analytics", BUSINESS)]),
    "supplier": (["pii"], "supply-chain", [("urn:li:corpGroup:procurement", BUSINESS)]),
    "part": ([], "supply-chain", [("urn:li:corpGroup:procurement", BUSINESS)]),
    "partsupp": ([], "supply-chain", [("urn:li:corpGroup:procurement", BUSINESS)]),
    "nation": (["public"], None, [("urn:li:corpGroup:data-platform", TECHNICAL)]),
    "region": (["public"], None, [("urn:li:corpGroup:data-platform", TECHNICAL)]),
}


def main():
    print("==> Waiting for DataHub GMS to be healthy")
    wait_for_health()

    print("==> Groups")
    upsert(
        "corpgroup",
        [
            {
                "urn": f"urn:li:corpGroup:{gid}",
                "corpGroupInfo": {
                    "value": {
                        "displayName": name,
                        "description": f"{name} team",
                        "admins": [],
                        "members": [],
                        "groups": [],
                    }
                },
            }
            for gid, name in GROUPS.items()
        ],
    )

    print("==> Users")
    upsert(
        "corpuser",
        [
            {
                "urn": f"urn:li:corpuser:{username}",
                "corpUserInfo": {
                    "value": {
                        "active": True,
                        "displayName": full_name,
                        "fullName": full_name,
                        "email": f"{username}@example.com",
                    }
                },
                "corpUserEditableInfo": {"value": {"email": f"{username}@example.com"}},
                "groupMembership": {"value": {"groups": [f"urn:li:corpGroup:{gid}"]}},
            }
            for username, (full_name, gid) in USERS.items()
        ],
    )

    print("==> Tags")
    upsert(
        "tag",
        [
            {"urn": f"urn:li:tag:{tid}", "tagProperties": {"value": {"name": name}}}
            for tid, name in TAGS.items()
        ],
    )

    print("==> Domains")
    upsert(
        "domain",
        [
            {
                "urn": f"urn:li:domain:{did}",
                "domainProperties": {"value": {"name": name, "description": desc}},
            }
            for did, (name, desc) in DOMAINS.items()
        ],
    )

    print("==> Data products (assets create the DataProductContains edges the RIF reads)")
    upsert(
        "dataproduct",
        [
            {
                "urn": f"urn:li:dataProduct:{pid}",
                "dataProductProperties": {
                    "value": {
                        "name": name,
                        "description": desc,
                        "assets": [{"destinationUrn": dataset(t)} for t in tables],
                    }
                },
                "domains": {"value": {"domains": [f"urn:li:domain:{did}"]}},
            }
            for pid, (name, desc, did, tables) in DATA_PRODUCTS.items()
        ],
    )

    print("==> Table tags / domains / ownership")
    dataset_objects = []
    for table, (tags, did, owners) in TABLES.items():
        obj = {"urn": dataset(table), "ownership": ownership_aspect(owners)}
        if tags:
            obj["globalTags"] = {"value": {"tags": [{"tag": f"urn:li:tag:{t}"} for t in tags]}}
        if did:
            obj["domains"] = {"value": {"domains": [f"urn:li:domain:{did}"]}}
        dataset_objects.append(obj)
    upsert("dataset", dataset_objects)

    print("==> Catalog / schema ownership")
    upsert(
        "container",
        [
            {"urn": CATALOG, "ownership": ownership_aspect([("urn:li:corpGroup:data-platform", TECHNICAL)])},
            # The schema deliberately mixes a group and a user owner (both technical).
            {
                "urn": SCHEMA,
                "ownership": ownership_aspect(
                    [
                        ("urn:li:corpGroup:data-platform", TECHNICAL),
                        ("urn:li:corpuser:erin.fischer", TECHNICAL),
                    ]
                ),
            },
        ],
    )

    # DataProductContains edges are graph-indexed asynchronously (unlike the aspects above), so
    # wait until at least one asset per data product resolves the incoming edge. This makes the
    # metadata state deterministic for the RIF assertions in the next step.
    print("==> Waiting for data-product membership edges to index")
    for pid, (_, _, _, tables) in DATA_PRODUCTS.items():
        probe = dataset(tables[0])
        query = (
            '{ entity(urn: "%s") { relationships(input: {types: ["DataProductContains"], '
            'direction: INCOMING, count: 10}) { total } } }' % probe
        )
        for attempt in range(24):
            resp = SESSION.post(f"{GMS}/api/graphql", json={"query": query}, timeout=20)
            total = (
                resp.json().get("data", {}).get("entity", {}).get("relationships", {}).get("total", 0)
                if resp.ok
                else 0
            )
            if total:
                print(f"  {pid}: edge indexed")
                break
            print(f"  {pid}: not indexed yet (attempt {attempt + 1}/24), retrying in 5s")
            time.sleep(5)
        else:
            sys.exit(f"data product {pid} membership edge never indexed")

    print("==> Successfully created all DataHub users, groups, tags, domains, data products")


if __name__ == "__main__":
    main()
