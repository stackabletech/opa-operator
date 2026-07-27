curl 'localhost:9477/metadata/database?system=trino&instance=kuttl-secure-hermit-wf84/my-trino&database=tpch' | jq
curl 'localhost:9477/metadata/schema?system=trino&instance=kuttl-secure-hermit-wf84/my-trino&database=tpch&schema=sf1' | jq
curl 'localhost:9477/metadata/table?system=trino&instance=kuttl-secure-hermit-wf84/my-trino&database=tpch&schema=sf1&table=customer' | jq
curl 'localhost:9477/metadata/stream?system=kafka&instance=kuttl-secure-hermit-wf84/test-kafka&queue=orders' | jq
curl 'localhost:9477/metadata/dashboard?system=superset&instance=kuttl-secure-hermit-wf84/my-superset&id=1' | jq
curl 'localhost:9477/metadata/chart?system=superset&instance=kuttl-secure-hermit-wf84/my-superset&id=1' | jq

curl 'localhost:9477/metadata/rawIdentifier?identifier=urn:li:container:792ce6aace288712a1ef036fbca2bb37' | jq
curl 'localhost:9477/metadata/rawIdentifier?identifier=urn:li:container:ae5d10cb199e26e8df70c7563e6d1c58' | jq
curl 'localhost:9477/metadata/rawIdentifier?identifier=urn:li:dataset:(urn:li:dataPlatform:trino,kuttl-secure-hermit-wf84/my-trino.tpch.sf1.customer,PROD)' | jq
curl 'localhost:9477/metadata/rawIdentifier?identifier=urn:li:dataset:(urn:li:dataPlatform:kafka,kuttl-secure-hermit-wf84/test-kafka.orders,PROD)' | jq
curl 'localhost:9477/metadata/rawIdentifier?identifier=urn:li:dashboard:(superset,kuttl-secure-hermit-wf84/my-superset.1)' | jq
curl 'localhost:9477/metadata/rawIdentifier?identifier=urn:li:chart:(superset,kuttl-secure-hermit-wf84/my-superset.1)' | jq
