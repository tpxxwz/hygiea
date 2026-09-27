//! Registry 和日志：同一个进程里创建多个 Registry 时，只有第一个的日志配置生效，后面的静默沿用，
//! 不 panic，也不创建自己配置的日志文件。
//!
//! 单独一个文件、只有一个 `#[test]` 的原因见 `tests/log_file_layer.rs` 开头：日志是进程级全局的，
//! 一种配置一个进程。

#![cfg(feature = "app")]

use std::path::Path;

use hygiea_core::app::{Registry, RegistryConfig};
use hygiea_core::log::{ConsoleLayer, FileLayer, Rolling, TracingConfig};

fn config(dir: &Path) -> RegistryConfig {
    RegistryConfig {
        tracing: TracingConfig {
            console: ConsoleLayer {
                disable: true,
                ..Default::default()
            },
            layers: vec![FileLayer {
                dir: dir.to_str().unwrap().to_string(),
                rolling: Rolling::Never,
                ..Default::default()
            }],
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn second_registry_keeps_first_log_config() {
    let first = tempfile::tempdir().unwrap();
    // second 原本不存在：第二个 Registry 的日志配置不生效，不应该把它建出来
    let second_base = tempfile::tempdir().unwrap();
    let second = second_base.path().join("second");

    let registry1 = Registry::with_config(config(first.path()));
    // 第二个不 panic，它的日志配置不生效，也不去建它的日志目录
    let registry2 = Registry::with_config(config(&second));
    assert!(
        !second.exists(),
        "second registry must not create log files"
    );

    tracing::info!("logged after second registry");

    // 第一个 Registry 持有 guard，drop 时把缓冲刷进文件
    drop(registry2);
    drop(registry1);
    let content = std::fs::read_to_string(first.path().join("app.log")).unwrap();
    assert!(
        content.contains("logged after second registry"),
        "{content}"
    );
}
