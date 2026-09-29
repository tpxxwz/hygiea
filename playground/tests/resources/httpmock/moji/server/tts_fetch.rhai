// moji 取发音地址：返回 {"text":..,"url":..,"type":"word"}
// url 指向 mock server 上的 /tts/<voice>/<targetId>.mp3?sign=..，签发记录放进 state.tts_links，
// tts_download 按它校验。挂了 db（state 里有 sessions）时先校验 token

if "sessions" in state {
    let token = request.header("x-moji-token");
    if token == () || !(token in state.sessions) {
        return #{ status: 400, json: #{ code: 100000022, message: "session expired" } };
    }
}

let id = request.query("targetId");
let target_type = request.query("targetType");
let voice = request.query("voiceId");
if voice == () {
    voice = "f002";
}

let entry = ();
for e in read_json("data/tts.json") {
    if e.targetId == id && `${e.targetType}` == target_type {
        entry = e;
    }
}
if entry == () {
    return #{ status: 404, json: #{ code: 100000404, message: "tts not found" } };
}

if !("tts_links" in state) {
    state.tts_links = #{};
}
let sign = `s${calls}`;
let path = `/tts/${voice}/${id}.mp3`;
state.tts_links[sign] = #{ voice: voice, id: id, file: entry.file };

#{ json: #{ text: entry.text, url: `${base}${path}?sign=${sign}`, type: "word" } }
