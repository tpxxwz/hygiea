import "common" as c;

let denied = c::check_auth(request, state);
if type_of(denied) != "()" {
    return denied;
}
c::ok(#{ folders: state.folders })
