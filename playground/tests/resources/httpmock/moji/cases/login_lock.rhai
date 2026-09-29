// 密码是 nyan123；错 3 次以后，就算密码对了也返回锁定
if state.failures >= 3 {
    return #{ status: 403, json: #{ code: 100000009, message: "account locked" } };
}
if request.json().authPayload.code != "nyan123" {
    state.failures += 1;
    return #{ status: 400, json: #{ code: 100000003, message: "email or password error" } };
}
state.failures = 0;
#{ json: #{ sessionToken: "t-ok" } }
