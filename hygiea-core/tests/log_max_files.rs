//! 日志配置测试：`max_log_files` 清理旧文件，前缀不重叠的文件不受影响。
//!
//! 单独一个文件、只有一个 `#[test]` 的原因见 `tests/log_file_layer.rs` 开头：日志是进程级全局的，
//! 一种配置一个进程。

#![cfg(feature = "log")]

use hygiea_core::log::{ConsoleLayer, FileLayer, Rolling, TracingConfig, init};

#[test]
fn max_log_files_prunes_old_files_but_not_other_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();

    // 手工放 3 个同前缀 "app"、同后缀 "log" 的旧日志文件。文件名按 tracing-appender 的规则：
    // `<prefix>.<日期>.<suffix>`，Rolling::Daily 用 `[year]-[month]-[day]`，比如 `app.2020-01-01.log`
    for date in ["2020-01-01", "2020-01-02", "2020-01-03"] {
        std::fs::write(dir.join(format!("app.{date}.log")), "old").unwrap();
    }
    // 前缀不重叠的文件：清理只按「以前缀开头、以后缀结尾」匹配，这个不会被当成候选
    let other = dir.join("other.2020-01-01.log");
    std::fs::write(&other, "other").unwrap();

    let cfg = TracingConfig {
        console: ConsoleLayer {
            disable: true,
            ..Default::default()
        },
        root_env_filter: "info".to_string(),
        layers: vec![FileLayer {
            dir: dir.to_str().unwrap().to_string(),
            rolling: Rolling::Daily,
            max_log_files: Some(2),
            ..Default::default()
        }],
        ..Default::default()
    };
    let guard = init(&cfg).unwrap();
    tracing::info!("hello");
    drop(guard);

    // tracing-appender 建 appender 时先按 max_log_files 清理旧文件（删到剩 max_log_files - 1 个），
    // 再创建当天的新文件，所以清理后 "app" 前缀的文件总数正好是 max_log_files；
    // 只断言总数而不认定具体删的是哪个：旧文件的创建时间几乎同时写入，清理靠的是文件系统的创建时间
    // （或者退化成按文件名里的日期），谁被删不是这个测试要关心的
    let app_files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("app") && name.ends_with("log"))
        .collect();
    assert_eq!(app_files.len(), 2, "{app_files:?}");

    // 前缀不重叠的文件原样保留
    assert!(other.exists());
}
