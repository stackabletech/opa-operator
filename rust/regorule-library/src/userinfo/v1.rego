package stackable.opa.userinfo.v1

# Lookup by (human-readable) username
userInfoByUsername(username) := userInfo({"username": username})

# Lookup by stable user identifier
userInfoById(id) := userInfo({"id": id})

# How long to wait for the user-info-fetcher before giving up.
#
# Set explicitly rather than inherited, because it is part of the contract: an authorization decision
# has to be answered promptly, so this is deliberately far below the fetcher's own 60s budget for
# talking to the identity provider. A provider that is slower than this leaves the caller without an
# answer while the fetcher finishes and caches the result, so the next lookup is served from the cache.
requestTimeout := "5s"

# Only a `200` yields a value. The fetcher answers a failed lookup with an error envelope
# (`{"error": {...}}`), which must never reach a policy as if it were user information. A policy that
# defaults a missing field (for example `object.get(user, "groups", [])`) would otherwise read a
# failed lookup as "this user is in no groups" and allow what it should deny. Requiring a `200` makes
# the whole call undefined instead, which such a policy cannot silently succeed on.
#
# An undefined call is still not a denial. OPA turns a builtin error (the fetcher being unreachable,
# or slower than `requestTimeout`) into an undefined value rather than aborting the query, unless
# the query sets `strict-builtin-errors`. Write policies so that absent user information denies
# rather than allows, and set `strict-builtin-errors` where the product's OPA client supports it, so
# that a failed lookup fails the decision instead of quietly dropping out of it.
userInfo(body) := response.body if {
    response := http.send({
        "method": "POST",
        "url": "http://127.0.0.1:9476/user",
        "body": body,
        "headers": {"Content-Type": "application/json"},
        "raise_error": true,
        "timeout": requestTimeout
    })
    response.status_code == 200
}
