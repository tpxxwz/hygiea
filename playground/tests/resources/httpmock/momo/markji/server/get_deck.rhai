import "common" as c;

let denied = c::check_auth(request, state);
if type_of(denied) != "()" {
    return denied;
}

let id = request.param("deck");
let deck = ();
for d in state.decks {
    if d.id == id {
        deck = d;
    }
}
if deck == () {
    return c::failed(404, "markji_deck_not_found", "Deck not found");
}
if request.query("with_root") == "true" && "parent_id" in deck {
    deck.root_deck = state.root_decks[deck.parent_id];
}
c::ok(#{ deck: deck })
