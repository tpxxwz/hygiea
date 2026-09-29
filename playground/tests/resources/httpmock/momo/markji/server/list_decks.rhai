import "common" as c;

let denied = c::check_auth(request, state);
if type_of(denied) != "()" {
    return denied;
}

let decks = state.decks;
let folder_id = request.query("folder_id");
if folder_id != () {
    let folder = ();
    for f in state.folders {
        if f.id == folder_id {
            folder = f;
        }
    }
    if folder == () {
        return c::failed(404, "markji_folder_not_found", "Folder not found");
    }
    let ids = [];
    for item in folder.items {
        if item.object_class == "DECK" {
            ids.push(item.object_id);
        }
    }
    decks = decks.filter(|d| ids.contains(d.id));
}

let offset = request.query("offset");
offset = if offset == () { 0 } else { parse_int(offset) };
let limit = request.query("limit");
limit = if limit == () { 20 } else { parse_int(limit) };

c::ok(#{ decks: decks.extract(offset, limit), total: decks.len() })
