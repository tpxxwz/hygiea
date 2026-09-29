import "common" as c;

let denied = c::check_session(request, state);
if type_of(denied) != "()" {
    return denied;
}
let id = request.param("id");
for it in state.items {
    if it.id == id {
        return #{ json: it };
    }
}
c::fail(404, "not_found", `item ${id} not found`)
