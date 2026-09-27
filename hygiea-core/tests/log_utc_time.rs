//! 日志配置测试：`utc_time`、自定义 `time_format`、单个 layer 自己的级别过滤。
//!
//! 单独一个文件、只有一个 `#[test]` 的原因见 `tests/log_file_layer.rs` 开头：日志是进程级全局的，
//! 一种配置一个进程。

#![cfg(feature = "log")]

use hygiea_core::log::{ConsoleLayer, FileLayer, Rolling, TracingConfig, init};

#[test]
fn utc_time_custom_format_and_layer_filter() {
    let dir = tempfile::tempdir().unwrap();
    let config = TracingConfig {
        console: ConsoleLayer {
            disable: true,
            ..Default::default()
        },
        root_env_filter: "info".to_string(),
        layers: vec![FileLayer {
            dir: dir.path().to_str().unwrap().to_string(),
            rolling: Rolling::Never,
            // 这个 layer 自己只要 WARN 及以上，覆盖 root 的 info
            env_filter: "warn".to_string(),
            ..Default::default()
        }],
        // 2024-01-05 17:00:00.000 +00:00
        time_format: "WithOffset.YmdHMS3F".parse().unwrap(),
        utc_time: true,
        ..Default::default()
    };
    let guard = init(&config).unwrap();

    tracing::info!("filtered info");
    tracing::warn!("kept warn");

    drop(guard);
    let content = std::fs::read_to_string(dir.path().join("app.log")).unwrap();

    assert!(content.contains("kept warn"), "{content}");
    assert!(!content.contains("filtered info"), "{content}");
    for line in content.lines() {
        // YmdHMS3F 带空格分隔，utc_time 下 offset 是 +00:00
        assert_eq!(&line[10..11], " ", "{line}");
        assert_eq!(&line[23..30], " +00:00", "{line}");
    }
}
