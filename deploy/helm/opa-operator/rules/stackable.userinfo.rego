package stackable.userinfo

userInfo(username) := http.send({"method": "POST", "url": "http://127.0.0.1:9476/user", "body": {"username": username}, "headers": {"Content-Type": "application/json"}, "raise_error": true}).body
currentUserInfo := userInfo(input.username)
