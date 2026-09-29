// 第 1、2 次 503，第 3 次正常；第 4 次起没有定义，脚本报错
nth(calls, [
    #{ status: 503, json: #{ code: "unavailable" } },
    #{ status: 503, json: #{ code: "unavailable" } },
    #{ json: #{ ok: true } },
])
