//! 日志配置测试：文件输出永远不带 ANSI 颜色码，不管 `ConsoleLayer.ansi` 或者 `NO_COLOR` 怎么配置。
//!
//! 单独一个文件、只有一个 `#[test]` 的原因见 `tests/log_file_layer.rs` 开头：日志是进程级全局的，
//! 一种配置一个进程。
//!
//! 计划里还想测「`ConsoleLayer.ansi` 未配置、设了 `NO_COLOR` 时 `effective_ansi()` 为 false」，
//! 但 `effective_ansi` 是 `log.rs` 里的私有方法，集成测试（这里）只能通过公开的 `hygiea_core::log` API
//! 访问，测不到；这条按任务里的说明跳过，只保留文件输出这一条。

#![cfg(feature = "log")]

use hygiea_core::log::{ConsoleLayer, FileLayer, TracingConfig, init};

#[test]
fn file_output_never_contains_ansi_escape() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = TracingConfig {
        console: ConsoleLayer {
            // 控制台关掉，只看文件
            disable: true,
            ..Default::default()
        },
        layers: vec![FileLayer {
            dir: dir.path().to_str().unwrap().to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let guard = init(&cfg).unwrap();

    tracing::error!("colorful in a terminal, but not in a file");

    drop(guard);
    let content = std::fs::read_to_string(dir.path().join("app.log")).unwrap();
    assert!(content.contains("colorful in a terminal"), "{content}");
    assert!(!content.contains("\x1b["), "{content}");
}
