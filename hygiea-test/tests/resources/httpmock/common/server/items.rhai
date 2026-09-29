import "common" as c;

let denied = c::check_session(request, state);
if type_of(denied) != "()" {
    return denied;
}
let items = state.items;
let q = request.query("q");
if q != () {
    items = items.filter(|it| it.name.contains(q));
}
let offset = request.query("offset");
offset = if offset == () { 0 } else { parse_int(offset) };
let limit = request.query("limit");
limit = if limit == () { 20 } else { parse_int(limit) };
#{ json: #{ items: items.extract(offset, limit), total: items.len() } }
