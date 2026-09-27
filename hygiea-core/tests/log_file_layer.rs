//! 日志配置测试：文件 layer 的写入、默认时间格式、级别过滤，以及同一进程里第二次 `init` 返回 `InstallFailed`。
//!
//! **为什么单独一个文件、只有一个 `#[test]`**：`log::init` 装的是进程级的全局 subscriber，一个进程只能装一次，
//! 装上就卸不掉。`tests/` 下每个文件编译成独立的进程，一种日志配置放一个文件，配置才一定生效；
//! 同一个文件里多个 `#[test]` 会在同一进程里并行跑、抢着装，所以同一文件里只能有一个测试真正 init 成功，
//! 这里把「第一次 init 成功并写文件」和「第二次 init 返回 InstallFailed」都放进这一个 `#[test]`，按顺序分步检查。
//! 其他测试不要依赖日志配置，见 `hygiea_core::log` 模块文档的「测试注意」。

#![cfg(feature = "log")]

use std::path::Path;

use hygiea_core::datetime::{DateTimeFormatter, now_local};
use hygiea_core::log::{BaseLogErr, ConsoleLayer, FileLayer, Rolling, TracingConfig, init};

fn file_config(dir: &Path) -> TracingConfig {
    TracingConfig {
        // 控制台关掉，只看文件
        console: ConsoleLayer {
            disable: true,
            ..Default::default()
        },
        root_env_filter: "info".to_string(),
        layers: vec![FileLayer {
            dir: dir.to_str().unwrap().to_string(),
            rolling: Rolling::Never,
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[test]
fn first_init_writes_file_then_second_init_returns_install_failed() {
    // 第一步：第一次 init 成功，按配置写文件、按级别过滤
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let guard = init(&file_config(dir)).unwrap();

    tracing::info!("hello info");
    tracing::debug!("hidden debug");
    tracing::warn!("careful warn");

    // 第二步：同一进程再 init，返回 InstallFailed，而且不碰文件（第二份配置的目录不会被建出来）
    let other_base = tempfile::tempdir().unwrap();
    let other = other_base.path().join("second");
    let err = init(&file_config(&other)).err().unwrap();
    assert!(err.is(BaseLogErr::InstallFailed));
    assert!(!other.exists(), "second init must not create log files");

    // 非阻塞写入：drop guard 把缓冲刷进文件
    drop(guard);
    let content = std::fs::read_to_string(dir.join("app.log")).unwrap();

    // root_env_filter = info：INFO / WARN 写进去，DEBUG 被过滤
    assert!(content.contains("hello info"), "{content}");
    assert!(content.contains("careful warn"), "{content}");
    assert!(!content.contains("hidden debug"), "{content}");

    // 默认格式 WithOffset.YmdTHMS3F（2024-01-06T01:00:00.000+08:00），用系统时区：
    // 每行开头 29 个字符是时间，第 23 位之后是 offset，和 now_local 的一致
    let local_offset = DateTimeFormatter::default().format(&now_local()).unwrap()[23..].to_string();
    for line in content.lines() {
        assert_eq!(&line[10..11], "T", "{line}");
        assert_eq!(&line[23..29], local_offset, "{line}");
    }
}
