//! 日志配置测试：多个文件 layer 各自的 filter、不配 filter 时沿用 `root_env_filter`、
//! disable 的 layer 不产生文件。
//!
//! 单独一个文件、只有一个 `#[test]` 的原因见 `tests/log_file_layer.rs` 开头：日志是进程级全局的，
//! 一种配置一个进程。

#![cfg(feature = "log")]

use hygiea_core::log::{ConsoleLayer, FileLayer, TracingConfig, init};

fn layer(dir: &str, env_filter: &str, disable: bool) -> FileLayer {
    FileLayer {
        dir: dir.to_string(),
        env_filter: env_filter.to_string(),
        disable,
        ..Default::default()
    }
}

#[test]
fn each_layer_filter_independent_and_disabled_layer_produces_no_file() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    let cfg = TracingConfig {
        console: ConsoleLayer {
            disable: true,
            ..Default::default()
        },
        root_env_filter: "info".to_string(),
        layers: vec![
            layer("info-layer", "info", false),
            layer("warn-layer", "warn", false),
            // 不配 env_filter：沿用 root_env_filter（"info"）
            layer("default-layer", "", false),
            layer("disabled-layer", "info", true),
        ],
        root_dir: root.to_str().unwrap().to_string(),
        ..Default::default()
    };
    let guard = init(&cfg).unwrap();

    tracing::info!("info line");
    tracing::warn!("warn line");

    drop(guard);

    let read = |dir: &str| std::fs::read_to_string(root.join(dir).join("app.log")).unwrap();

    let info_layer = read("info-layer");
    assert!(info_layer.contains("info line"), "{info_layer}");
    assert!(info_layer.contains("warn line"), "{info_layer}");

    let warn_layer = read("warn-layer");
    assert!(!warn_layer.contains("info line"), "{warn_layer}");
    assert!(warn_layer.contains("warn line"), "{warn_layer}");

    // 不配 filter 时和 root_env_filter = "info" 效果一样：两条都留下
    let default_layer = read("default-layer");
    assert!(default_layer.contains("info line"), "{default_layer}");
    assert!(default_layer.contains("warn line"), "{default_layer}");

    // disable 的 layer 不建目录，更谈不上产生文件
    assert!(!root.join("disabled-layer").exists());
}
