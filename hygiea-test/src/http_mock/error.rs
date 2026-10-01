//! http_mock 的错误，分两种，和框架内置错误共用项目前缀 999：
//!
//! - [`HttpMockErr`]（内部模块前缀 800）：还没起来就错了，或者测试里调 API 出错。
//!   加载 cassette、编译脚本、检查路由、启动、读写 state，通过 `Result` 直接返回
//! - [`HttpMockRuntimeErr`]（内部模块前缀 801）：mock 起来以后处理请求时出的问题（没有路由、脚本出错……）。
//!   mock server 里没法把错误返回给测试，记在 [`Mocked::problems`](super::Mocked::problems) 里，
//!   由 [`Mocked::assert_valid`](super::Mocked::assert_valid) 统一报；同时作为这次请求的响应返回给 client：
//!   body 是 `{"err_code":..,"message":..}`，响应头 `x-hygiea-mock-problem` 是错误码，没有路由 404，其他 500

use hygiea_core::hy_err;

#[derive(hy_err)]
#[err_code_internal_module_prefix = "800"]
/// 构建、启动 mock，或者测试里调 API 时的错误
pub enum HttpMockErr {
    /// 服务目录 `<root>/<dir>` 不存在
    #[error(err_code = "01", err_tpl = "Cassette not found: {{ path }}")]
    CassetteNotFound,
    /// 读 cassette、state 文件失败；原始错误挂在 source 上
    #[error(err_code = "02", err_tpl = "Read failed: {{ path }}")]
    ReadFailed,
    /// cassette 或 state 文件解析失败；原始错误挂在 source 上
    #[error(err_code = "03", err_tpl = "Parse failed: {{ path }}")]
    ParseFailed,
    /// cassette 内容不合法：字段组合不对、方法 / 正则写错……
    #[error(err_code = "04", err_tpl = "Invalid cassette {{ path }}: {{ cause }}")]
    InvalidCassette,
    /// Rhai 脚本编译失败；原始错误挂在 source 上
    #[error(err_code = "05", err_tpl = "Script compile failed: {{ path }}")]
    ScriptCompileFailed,
    /// state 文件（data/state.json）顶层不是对象
    #[error(err_code = "06", err_tpl = "Invalid state {{ path }}: {{ cause }}")]
    InvalidState,
    /// cassette 里 `path` 的占位符模板不合法
    #[error(err_code = "08", err_tpl = "Invalid path template: {{ path }}")]
    InvalidTemplate,
    /// 状态和 Rust 类型 / JSON 互转失败；原始错误挂在 source 上
    #[error(err_code = "09", err_tpl = "State convert failed")]
    StateConvertFailed,
    /// 同一个 method 下两条路由能匹配同一个路径
    #[error(
        err_code = "10",
        err_tpl = "Route conflict: {{ a }} and {{ b }}, e.g. {{ example }}"
    )]
    RouteConflict,
    /// 固定响应里写死的 `body_file` 在文件表（`data/static/` 和代码加的文件）里找不到
    #[error(
        err_code = "11",
        err_tpl = "File not found: {{ route }}: {{ file }} (relative to data/static/)"
    )]
    FileNotFound,
}

/// mock 起来以后处理请求时出的问题：记进 problems，同时作为这次请求的响应
#[derive(hy_err)]
#[err_code_internal_module_prefix = "801"]
pub enum HttpMockRuntimeErr {
    /// 请求没有路由接住（404）：`reason` 是路由对上了但额外条件没对上 / 路径只有别的 method / 没有路径对得上
    #[error(err_code = "01", err_tpl = "No route: {{ request }}: {{ reason }}")]
    NoRoute,
    /// handler 链里的脚本运行出错（500）；原始错误挂在 source 上
    #[error(
        err_code = "02",
        err_tpl = "Script failed: {{ route }}, handler #{{ step }}"
    )]
    ScriptFailed,
    /// 脚本返回的不是合法响应（500）：不是对象、字段不认识、类型不对、body / body_file / json 写了多个。
    /// 固定响应的这些问题启动时就查了（[`HttpMockErr::InvalidCassette`]）
    #[error(
        err_code = "03",
        err_tpl = "Invalid response: {{ route }}: {{ cause }}"
    )]
    InvalidResponse,
    /// 整条 handler 链都返回了 ()，没给出响应（500）
    #[error(
        err_code = "04",
        err_tpl = "No response: {{ route }}: every handler returned () [{{ steps }}], the last handler must return a response"
    )]
    NoResponse,
    /// 脚本返回的 `body_file` 在文件表里找不到（500）；固定响应里写死的启动时就查了（[`HttpMockErr::FileNotFound`]）
    #[error(
        err_code = "05",
        err_tpl = "File not found: {{ route }}: {{ file }} (relative to data/static/)"
    )]
    FileNotFound,
    /// `match_script` 运行出错，按不匹配处理；原始错误挂在 source 上
    #[error(err_code = "06", err_tpl = "match_script failed: {{ route }}")]
    MatchScriptFailed,
}
