package stackable.opa.userinfo.v1

# Lookup by (human-readable) username
userInfoByUsername(username) := http.send({
  "method": "POST",
  "url": "http://127.0.0.1:9476/user",
  "body": {"username": username}, <2>
  "headers": {"Content-Type": "application/json"},
  "raise_error": true
}).body

# Lookup by stable user identifier
userInfoById(id) := http.send({
  "method": "POST",
  "url": "http://127.0.0.1:9476/user",
  "body": {"id": id}, <3>
  "headers": {"Content-Type": "application/json"},
  "raise_error": true
}).body

# Lookup by context
currentUserInfoByUsername := userInfoByUsername(input.username)
currentUserInfoById := userInfoById(input.id)
