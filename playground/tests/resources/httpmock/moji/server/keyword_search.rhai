// moji 关键词搜索：按 query 的 text 在词库里查单词，spell 或 pron 包含 text 就算命中
//
// 挂了 db（state 里有 sessions）时先校验 x-moji-token：不在 state.sessions 里就返回会话过期，
// 和真实服务一样。单独挂这个接口时没有 sessions，跳过校验

if "sessions" in state {
    let token = request.header("x-moji-token");
    if token == () || !(token in state.sessions) {
        return #{ status: 400, json: #{ code: 100000022, message: "session expired" } };
    }
}

let text = request.query("text");
if text == () || text == "" {
    return #{ status: 400, json: #{ code: 1, message: "text is required" } };
}

// types 是多个同名参数；没传或者包含 102（单词）才查单词
let types = request.query_all("types");
let list = [];
if types.is_empty() || types.contains("102") {
    for w in read_json("data/words.json") {
        if w.spell.contains(text) || w.pron.contains(text) {
            list.push(#{
                targetId: w.targetId,
                targetType: 102,
                title: `${w.spell} | ${w.pron} ${w.accent}`,
                excerpt: w.excerpt,
            });
        }
    }
}

#{ json: #{ word: #{ list: list } } }
