//! 验证 `now_local` 确实读取了 `TZ` 环境变量。
//!
//! `LOCAL_TIMEZONE_OVERRIDE` 只在 hygiea_core 自己的单元测试里可用（`#[cfg(test)]`，`pub(super)`），
//! 这个集成测试是独立的二进制，看不到那个 override，只能测真实的 `TZ` 环境变量路径；
//! `local_timezone()` 把探测结果缓存进 `OnceLock`，同一进程只能生效一次，所以每个 TZ 场景都用
//! 子进程重新跑一遍当前测试二进制（用 `--exact` 选中同名测试），子进程把 `now_local()` 的 offset
//! （整数秒）打到 stdout，父进程解析后断言。
//!
//! 手动运行：`cargo test -p hygiea-core --all-features --test datetime_tz_env`

#![cfg(feature = "datetime-iana")]

use std::env;
use std::process::Command;

use hygiea_core::datetime::now_local;

/// 子进程标记：设了这个环境变量就说明是被父进程重新拉起来跑同一个测试的
const CHILD_ENV: &str = "HYGIEA_DATETIME_TZ_ENV_CHILD";
/// 子进程打到 stdout 的行前缀，父进程按这个前缀找结果
const OFFSET_PREFIX: &str = "HYGIEA_OFFSET_SECS=";

/// 子进程模式下，打印 `now_local()` 的 offset（整数秒）后直接退出，不再往下走
fn exit_if_child() {
    if env::var(CHILD_ENV).is_ok() {
        println!("{OFFSET_PREFIX}{}", now_local().offset().whole_seconds());
        std::process::exit(0);
    }
}

/// 用给定的 `TZ`（`None` 表示不设）重新跑当前测试二进制里名为 `test_name` 的这一个测试，
/// 返回子进程打印出来的 offset（整数秒）
fn offset_with_tz(test_name: &str, tz: Option<&str>) -> i32 {
    let exe = env::current_exe().expect("current test binary path");
    let mut cmd = Command::new(exe);
    cmd.arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .env(CHILD_ENV, "1");
    match tz {
        Some(tz) => {
            cmd.env("TZ", tz);
        }
        None => {
            cmd.env_remove("TZ");
        }
    }
    let output = cmd.output().expect("spawn child test process");
    assert!(
        output.status.success(),
        "child process failed, stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .find_map(|line| line.strip_prefix(OFFSET_PREFIX))
        .unwrap_or_else(|| panic!("child did not print offset, stdout={stdout}"))
        .parse()
        .expect("offset is an integer")
}

/// `TZ=:Asia/Shanghai`（glibc 风格的冒号前缀）时，`now_local` 用的是 +08:00
#[test]
fn now_local_reads_tz_env_shanghai() {
    exit_if_child();
    let offset = offset_with_tz("now_local_reads_tz_env_shanghai", Some(":Asia/Shanghai"));
    assert_eq!(offset, 8 * 3600);
}

/// `TZ=Bad/Zone` 查不到时，和完全不设 `TZ` 走的是同一条系统探测路径，结果应该一致
#[test]
fn now_local_falls_back_to_system_on_bad_tz() {
    exit_if_child();
    let system = offset_with_tz("now_local_falls_back_to_system_on_bad_tz", None);
    let bad = offset_with_tz("now_local_falls_back_to_system_on_bad_tz", Some("Bad/Zone"));
    assert_eq!(bad, system);
}
