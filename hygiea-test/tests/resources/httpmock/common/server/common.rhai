// 几个接口共用的函数：import "common" as c;

fn fail(status, code, message) {
    #{ status: status, json: #{ code: code, message: message } }
}

// 挂了 db（state 里有 sessions）时校验 x-token，不对返回 401 的响应；对了或者没挂 db 返回 ()
fn check_session(request, state) {
    if !("sessions" in state) {
        return ();
    }
    let token = request.header("x-token");
    if token == () || !(token in state.sessions) {
        return fail(401, "unauthorized", "token missing or expired");
    }
    ()
}
