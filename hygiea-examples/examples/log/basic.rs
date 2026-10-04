//! 日志：console + 文件双输出、按 layer 设置不同的 filter、日志时间用 UTC。
//! 直接调 `hygiea::log::init`（不经过 `hygiea::app` 组件框架，脚本、小工具也能用）。
//!
//! ```bash
//! cargo run -p hygiea-examples --example log_basic
//! ```
//!
//! 日志文件写在系统临时目录下的 `hygiea-examples/log_basic/`，运行结束会打印这个路径和文件内容，
//! 方便直接在输出里对比控制台和文件两份日志的差异。

use hygiea::Result;
use hygiea::datetime::WithOffsetFormatter;
use hygiea::log::{ConsoleLayer, FileLayer, Rolling, TracingConfig, init};

fn main() -> Result<()> {
    // 每次运行前清空，只看这一次的输出
    let log_dir = std::env::temp_dir().join("hygiea-examples/log_basic");
    let _ = std::fs::remove_dir_all(&log_dir);

    let cfg = TracingConfig {
        console: ConsoleLayer {
            // 控制台只看 info 及以上
            env_filter: "info".to_string(),
            ..Default::default()
        },
        layers: vec![FileLayer {
            dir: log_dir.to_string_lossy().into_owned(),
            // 文件这边放开到 debug，能看到控制台看不到的那一行
            env_filter: "debug".to_string(),
            rolling: Rolling::Never,
            ..Default::default()
        }],
        root_env_filter: "info".to_string(),
        // 日志时间用 UTC，不用系统时区；默认 false 是系统时区
        utc_time: true,
        time_format: WithOffsetFormatter::YmdTHMS3F.into(),
        ..Default::default()
    };

    {
        // guard 要一直持有到不再需要写日志为止：drop 时才把非阻塞队列里的日志刷到磁盘。
        // 这里特意把 guard 的生命周期限制在这个块里，好在块结束后马上读文件内容
        let _guard = init(&cfg)?;
        tracing::debug!(
            "debug 级别：文件 layer 的 filter 是 debug，只会出现在文件里，不会出现在控制台"
        );
        tracing::info!("info 级别：控制台和文件都能看到");
        tracing::warn!("warn 级别：同上");
        tracing::error!("error 级别：同上");
    }

    println!("\n日志文件目录：{}", log_dir.display());
    match std::fs::read_dir(&log_dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let path = entry.path();
                let content =
                    std::fs::read_to_string(&path).unwrap_or_else(|e| format!("(读取失败: {e})"));
                println!("--- {} ---\n{}", path.display(), content);
            }
        }
        Err(e) => println!("(读取日志目录失败: {e})"),
    }

    Ok(())
}
