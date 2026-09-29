// x-token 不是 admin 返回 401，是就返回 () 交给下一个 handler
if request.header("x-token") != "admin" {
    return #{ status: 401, json: #{ code: "unauthorized" } };
}
()
