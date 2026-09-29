// moji 下载发音：只认 tts_fetch 签发过的地址，错误响应照对象存储的 XML 格式

let denied = |code| #{
    status: 403,
    headers: #{ "content-type": "application/xml" },
    body: `<?xml version="1.0" encoding="UTF-8"?><Error><Code>${code}</Code></Error>`,
};

let sign = request.query("sign");
if sign == () || !("tts_links" in state) || !(sign in state.tts_links) {
    return denied.call("AccessDenied");
}
// 路径里的 voice、id 由 cassette 的 path 占位符 /tts/{voice}/{id}.mp3 取出来
let link = state.tts_links[sign];
if link.voice != request.param("voice") || link.id != request.param("id") {
    return denied.call("SignatureDoesNotMatch");
}

#{ headers: #{ "content-type": "audio/mpeg" }, body_file: link.file }
