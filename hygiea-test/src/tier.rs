//! 测试分层宏（[`container`](macro@crate::container) / [`live`](macro@crate::live)）生成的代码调用的函数。
//! 宏只改写代码结构，要在运行时做的事放在这里。

/// 检查环境变量，缺了就列出全部缺少的名字再 panic。`#[live(env = [..])]` 在每个测试开头调用它
pub fn require_env(keys: &[&str]) {
    let missing: Vec<&str> = keys
        .iter()
        .copied()
        .filter(|k| std::env::var_os(k).is_none())
        .collect();
    if !missing.is_empty() {
        panic!("live test needs env: {}", missing.join(", "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 都在就不报错
    #[test]
    fn present_env_passes() {
        require_env(&["PATH"]);
    }

    /// 缺了的全部列出来，已经有的不列
    #[test]
    #[should_panic(expected = "live test needs env: HYGIEA_TEST_MISSING_A, HYGIEA_TEST_MISSING_B")]
    fn lists_all_missing() {
        require_env(&["HYGIEA_TEST_MISSING_A", "PATH", "HYGIEA_TEST_MISSING_B"]);
    }
}
