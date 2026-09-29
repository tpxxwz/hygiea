//! http_mock 的错误：加载 cassette、启动 mock server、读写状态时出错。和框架内置错误共用项目前缀 999，
//! 模块前缀 10。
//!
//! 请求处理时的问题（脚本出错、响应不符合约定）不走这里：mock server 里没法把错误返回给测试，
//! 记在 [`Mocked::problems`](super::Mocked::problems) 里，由 [`Mocked::assert_valid`](super::Mocked::assert_valid) 统一报

use hygiea::hy_err;

#[derive(hy_err)]
#[err_code_module_prefix = "10"]
pub enum HttpMockErr {
    /// `<root>/<name>.toml` 不存在
    #[error(err_code = "001", err_tpl = "Cassette not found: {{ path }}")]
    CassetteNotFound,
    /// 读 cassette、db 文件失败；原始错误挂在 source 上
    #[error(err_code = "002", err_tpl = "Read failed: {{ path }}")]
    ReadFailed,
    /// cassette 或 db 文件解析失败；原始错误挂在 source 上
    #[error(err_code = "003", err_tpl = "Parse failed: {{ path }}")]
    ParseFailed,
    /// cassette 内容不合法：字段组合不对、方法 / 正则写错……
    #[error(err_code = "004", err_tpl = "Invalid cassette {{ path }}: {{ cause }}")]
    InvalidCassette,
    /// Rhai 脚本编译失败；原始错误挂在 source 上
    #[error(err_code = "005", err_tpl = "Script compile failed: {{ path }}")]
    ScriptCompileFailed,
    /// db 文件不能当 state 用：顶层不是对象
    #[error(err_code = "006", err_tpl = "Invalid db {{ path }}: {{ cause }}")]
    InvalidDb,
    /// cassette 里 `path` 的占位符模板不合法
    #[error(err_code = "008", err_tpl = "Invalid path template: {{ path }}")]
    InvalidTemplate,
    /// 状态和 Rust 类型 / JSON 互转失败；原始错误挂在 source 上
    #[error(err_code = "009", err_tpl = "State convert failed")]
    StateConvertFailed,
}
