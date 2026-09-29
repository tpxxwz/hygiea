// 墨墨记忆卡几个接口共用的函数，接口脚本里 `import "common" as c;` 引用
// 墨墨开放 API 的外层：成功 {"success":true,"data":..,"errors":[]}，失败 {"success":false,"errors":[..]}

fn ok(data) {
    #{ json: #{ success: true, data: data, errors: [] } }
}

fn failed(status, code, msg) {
    #{ status: status, json: #{ success: false, errors: [#{ code: code, msg: msg, info: "" }] } }
}

/// token 不对返回 401 的响应，对了返回 ()
fn check_auth(request, state) {
    if request.header("authorization") != `Bearer ${state.token}` {
        failed(401, "common_unauthorized", "Authorization failed")
    } else {
        ()
    }
}
