//! 日志配置测试：文件 layer 关掉 non_blocking，同步写文件。
//!
//! 装的是进程级的全局 subscriber，所以单独一个文件、只有一个 `#[test]`，原因见 `log_file_layer.rs`。

#![cfg(feature = "log")]

use hygiea_core::log::{ConsoleLayer, FileLayer, TracingConfig, init};

#[test]
fn sync_file_layer_writes_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = TracingConfig {
        console: ConsoleLayer {
            disable: true,
            ..Default::default()
        },
        layers: vec![FileLayer {
            dir: dir.path().to_str().unwrap().to_string(),
            non_blocking: false,
            ..Default::default()
        }],
        ..Default::default()
    };
    let _guard = init(&cfg).unwrap();

    tracing::info!("sync hello");

    // 同步写：不用 drop guard，打完就已经在文件里
    let content = std::fs::read_to_string(dir.path().join("app.log")).unwrap();
    assert!(content.contains("sync hello"), "{content}");
}
