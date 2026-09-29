// 按 content-type 选解析方式，把取到的东西都放进响应
let ct = request.content_type;
let body = if ct.starts_with("application/json") {
    request.json()
} else if ct.starts_with("application/x-www-form-urlencoded") {
    request.form()
} else {
    request.text()
};

#{
    json: #{
        method: request.method,
        path: request.path,
        q: request.query("q"),
        tags: request.query_all("tag"),
        missing: request.query("missing"),
        token: request.header("X-Token"),
        content_type: ct,
        body_len: request.body_len,
        body: body,
        calls: calls,
    }
}
