//! 环境变量优先级测试。
//!
//! 验证 env_get() 在获取环境变量时的优先级：
//! 1. 首先查询环境变量（std::env::var）
//! 2. 没有则使用 key 自带的 default_value()
//!
//! 由于 set_var 在 2024 edition 下是 unsafe 的，这里把这些测试放在集成测试中，
//! 每个用例使用唯一的变量名避免冲突。

use hygiea_core::env::{EnvKey, env_get};

#[derive(Debug, Clone, Copy)]
struct TestKey(&'static str, Option<&'static str>);

impl EnvKey for TestKey {
    fn key_name(&self) -> &str {
        self.0
    }
    fn default_value(&self) -> Option<String> {
        self.1.map(str::to_string)
    }
}

/// 环境变量存在时，忽略 key 的默认值
#[test]
fn env_var_overrides_default() {
    // SAFETY: 这是测试代码，用唯一的变量名避免并发问题
    unsafe {
        std::env::set_var("HYGIEA_TEST_ENV_OVERRIDE", "from_env");
    }

    let key = TestKey("HYGIEA_TEST_ENV_OVERRIDE", Some("from_default"));
    let result = env_get(key).unwrap();
    assert_eq!(result, "from_env");
}

/// 环境变量不存在时，使用 key 的默认值
#[test]
fn env_var_missing_uses_default() {
    // 清除变量（如果存在）
    // SAFETY: 这是测试代码，用唯一的变量名
    unsafe {
        std::env::remove_var("HYGIEA_TEST_ENV_MISSING_1");
    }

    let key = TestKey("HYGIEA_TEST_ENV_MISSING_1", Some("default_value"));
    let result = env_get(key).unwrap();
    assert_eq!(result, "default_value");
}

/// 环境变量不存在，key 也没有默认值时返回错误
#[test]
fn both_missing_returns_error() {
    // SAFETY: 这是测试代码，用唯一的变量名
    unsafe {
        std::env::remove_var("HYGIEA_TEST_ENV_MISSING_2");
    }

    let key = TestKey("HYGIEA_TEST_ENV_MISSING_2", None);
    let err = env_get(key).unwrap_err();
    assert!(err.to_string().contains("HYGIEA_TEST_ENV_MISSING_2"));
}

/// 环境变量优先于空的默认值（空字符串是有效的默认值，不是"没有"）
#[test]
fn env_var_overrides_empty_default() {
    // SAFETY: 这是测试代码，用唯一的变量名
    unsafe {
        std::env::set_var("HYGIEA_TEST_ENV_EMPTY_DEFAULT", "from_env");
    }

    let key = TestKey("HYGIEA_TEST_ENV_EMPTY_DEFAULT", Some(""));
    let result = env_get(key).unwrap();
    // 应该返回环境变量的值，不是空默认值
    assert_eq!(result, "from_env");
}

/// 环境变量值为空字符串时也被认为"存在"，不会再去查默认值
#[test]
fn empty_env_var_is_valid() {
    // SAFETY: 这是测试代码，用唯一的变量名
    unsafe {
        std::env::set_var("HYGIEA_TEST_ENV_EMPTY_VALUE", "");
    }

    let key = TestKey("HYGIEA_TEST_ENV_EMPTY_VALUE", Some("default"));
    let result = env_get(key).unwrap();
    // 环境变量值是空字符串，优先于默认值
    assert_eq!(result, "");
}
