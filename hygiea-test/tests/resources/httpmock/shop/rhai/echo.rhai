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
        id: request.param("id"),
        q: request.query("q"),
        tags: request.query_all("tag"),
        queries_all: request.queries_all,
        accept_all: request.header_all("Accept"),
        headers_all_accept: request.headers_all.accept,
        missing: request.query("missing"),
        token: request.header("X-Token"),
        sid: request.cookie("sid"),
        cookies: request.cookies,
        cookies_all: request.cookies_all,
        tag_cookies: request.cookie_all("tag"),
        content_type: ct,
        body_len: request.body_len,
        body: body,
        base: base,
    }
}
