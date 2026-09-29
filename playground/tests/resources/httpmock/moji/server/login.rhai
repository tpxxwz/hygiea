// moji 登录：body 是 {"authName":"PasswordAuth","authPayload":{"email":..,"code":..}}，code 是密码
// 成功返回 {"sessionToken": ..}，同时往 state.sessions 写一条会话；每次登录都发新 token

let body = request.json();
if body.authName != "PasswordAuth" {
    return #{ status: 400, json: #{ code: 100000001, message: "unsupported auth" } };
}
let payload = body.authPayload;

let user = ();
for u in state.users {
    if u.email == payload.email {
        user = u;
    }
}
if user == () || user.password != payload.code {
    return #{ status: 400, json: #{ code: 100000003, message: "email or password error" } };
}

state.token_seq += 1;
let token = `r:token-${state.token_seq}`;
state.sessions[token] = #{ user_id: user.id, seq: state.token_seq };
#{ json: #{ sessionToken: token } }
