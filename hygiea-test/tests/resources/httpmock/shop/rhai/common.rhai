// 几个接口共用的函数：import "common" as c;

fn fail(status, code, message) {
    #{ status: status, json: #{ code: code, message: message } }
}

// 校验 x-token，不对返回 401 的响应，对了返回 ()
fn check_session(request, state) {
    let token = request.header("x-token");
    if token == () || !(token in state.sessions) {
        return fail(401, "unauthorized", "token missing or expired");
    }
    ()
}
