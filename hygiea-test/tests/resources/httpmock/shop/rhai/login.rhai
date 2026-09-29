import "common" as c;

let body = request.json();
let user = ();
for u in state.users {
    if u.name == body.name {
        user = u;
    }
}
if user == () || user.password != body.password {
    return c::fail(401, "bad_credentials", "name or password error");
}
state.token_seq += 1;
let token = `tk-${state.token_seq}`;
state.sessions[token] = #{ user_id: user.id };
#{ json: #{ token: token } }
