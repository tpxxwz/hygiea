//! `Registry::load_config` 的配置分层加载：环境文件 < `-f` 额外文件 < 环境变量 < `--set`，
//! `<PREFIX>_ENV` 选择环境，`-d` / `default_config_dir` 决定去哪个目录读文件。
//!
//! `load_config` 内部会调用 `Registry::with_config` 装全局日志：本文件里第一个成功的装上，
//! 其余的走 `InstallFailed` 静默沿用（见 `app_registry_log.rs`），不影响这里只关心配置本身的断言。
//!
//! 每个测试用不同的 `env_prefix`、各自的 `tempfile` 目录，避免互相干扰；`set_var` / `remove_var`
//! 在 2024 edition 是 unsafe 的，都写了 SAFETY 注释。

#![cfg(feature = "app")]

use hygiea_core::app::{ConfigArgs, IntoRegistryConfig, Registry, RegistryConfig};
use serde::Deserialize;

/// 测试用的顶层配置：`registry` 段省略时用默认值，顶层 `deny_unknown_fields` 用来测未知字段报错
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppConfig {
    #[serde(default)]
    registry: RegistryConfig,
    #[serde(default)]
    name: String,
    #[serde(default)]
    count: i64,
    #[serde(default)]
    verbose: bool,
}

impl IntoRegistryConfig for AppConfig {
    fn registry_config(&self) -> RegistryConfig {
        self.registry.clone()
    }
}

fn write(dir: &std::path::Path, file: &str, content: &str) {
    std::fs::write(dir.join(file), content).unwrap();
}

// ---- 1. 分层合并：环境文件 < -f 文件 < 环境变量 < --set --------------------

#[test]
fn config_layers_override_in_order() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "dev.toml", "name = \"from-dev\"\n");
    write(dir.path(), "local.toml", "name = \"from-local\"\n");

    let prefix = "HYGIEA_TEST_LAYERS";
    let base = ConfigArgs::default()
        .env_prefix(prefix)
        .default_config_dir(dir.path().to_str().unwrap());

    // 只有环境文件
    let (_registry, config) = Registry::load_config::<AppConfig>(&base);
    assert_eq!(config.name, "from-dev");

    // -f local：覆盖环境文件
    let mut with_extra_file = base.clone();
    with_extra_file.configs = vec!["local".to_string()];
    let (_registry, config) = Registry::load_config::<AppConfig>(&with_extra_file);
    assert_eq!(config.name, "from-local");

    // 环境变量：覆盖 -f 文件
    let env_key = format!("{prefix}__NAME");
    // SAFETY: 测试代码，env_key 带独有前缀，不会跟其他测试冲突
    unsafe {
        std::env::set_var(&env_key, "from-env");
    }
    let (_registry, config) = Registry::load_config::<AppConfig>(&with_extra_file);
    assert_eq!(config.name, "from-env");

    // --set：覆盖环境变量，优先级最高
    let mut with_override = with_extra_file.clone();
    with_override.overrides = vec![("name".to_string(), "from-set".to_string())];
    let (_registry, config) = Registry::load_config::<AppConfig>(&with_override);
    assert_eq!(config.name, "from-set");

    // SAFETY: 测试代码，清理掉自己设的变量
    unsafe {
        std::env::remove_var(&env_key);
    }
}

// ---- 2. <PREFIX>_ENV 选择环境 ------------------------------------------------

#[test]
fn env_var_selects_environment() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "dev.toml", "name = \"dev-env\"\n");
    write(dir.path(), "prod.toml", "name = \"prod-env\"\n");

    let prefix = "HYGIEA_TEST_ENVSELECT";
    let args = ConfigArgs::default()
        .env_prefix(prefix)
        .default_config_dir(dir.path().to_str().unwrap());

    // 不设 <PREFIX>_ENV、命令行也不传 --env：默认 dev
    let (_registry, config) = Registry::load_config::<AppConfig>(&args);
    assert_eq!(config.name, "dev-env");

    let env_var = format!("{prefix}_ENV");
    // SAFETY: 测试代码，env_var 带独有前缀
    unsafe {
        std::env::set_var(&env_var, "prod");
    }
    let (_registry, config) = Registry::load_config::<AppConfig>(&args);
    assert_eq!(config.name, "prod-env");

    // SAFETY: 测试代码，清理掉自己设的变量
    unsafe {
        std::env::remove_var(&env_var);
    }
}

// ---- 3. config_dir（命令行字段）优先于 default_config_dir ------------------

#[test]
fn config_dir_field_overrides_default_config_dir() {
    let default_dir = tempfile::tempdir().unwrap();
    write(
        default_dir.path(),
        "dev.toml",
        "name = \"from-default-dir\"\n",
    );
    let cli_dir = tempfile::tempdir().unwrap();
    write(cli_dir.path(), "dev.toml", "name = \"from-cli-dir\"\n");

    let prefix = "HYGIEA_TEST_CONFIGDIR";
    let args = ConfigArgs::default()
        .env_prefix(prefix)
        .default_config_dir(default_dir.path().to_str().unwrap());

    // 只设了 default_config_dir：读它
    let (_registry, config) = Registry::load_config::<AppConfig>(&args);
    assert_eq!(config.name, "from-default-dir");

    // 命令行 -d（config_dir 字段）覆盖代码里设的默认目录
    let mut with_cli_dir = args.clone();
    with_cli_dir.config_dir = Some(cli_dir.path().to_str().unwrap().to_string());
    let (_registry, config) = Registry::load_config::<AppConfig>(&with_cli_dir);
    assert_eq!(config.name, "from-cli-dir");
}

// ---- 4. 环境变量里的数字和布尔值能正确解析 ----------------------------------

#[test]
fn env_var_parses_numbers_and_booleans() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "dev.toml", "");

    let prefix = "HYGIEA_TEST_TYPES";
    let args = ConfigArgs::default()
        .env_prefix(prefix)
        .default_config_dir(dir.path().to_str().unwrap());

    let count_key = format!("{prefix}__COUNT");
    let verbose_key = format!("{prefix}__VERBOSE");
    // SAFETY: 测试代码，变量名带独有前缀
    unsafe {
        std::env::set_var(&count_key, "42");
        std::env::set_var(&verbose_key, "true");
    }

    let (_registry, config) = Registry::load_config::<AppConfig>(&args);
    assert_eq!(config.count, 42);
    assert!(config.verbose);

    // SAFETY: 测试代码，清理掉自己设的变量
    unsafe {
        std::env::remove_var(&count_key);
        std::env::remove_var(&verbose_key);
    }
}

// ---- 5. 文件缺失、类型不对、未知字段都会 panic -----------------------------

#[test]
#[should_panic(expected = "failed to load config")]
fn missing_config_file_panics() {
    let dir = tempfile::tempdir().unwrap();
    // dev.toml 不存在
    let args = ConfigArgs::default()
        .env_prefix("HYGIEA_TEST_MISSING_FILE")
        .default_config_dir(dir.path().to_str().unwrap());
    let _ = Registry::load_config::<AppConfig>(&args);
}

#[test]
#[should_panic(expected = "failed to parse config")]
fn wrong_type_panics() {
    let dir = tempfile::tempdir().unwrap();
    // count 是 i64 字段，给个字符串
    write(dir.path(), "dev.toml", "count = \"not-a-number\"\n");
    let args = ConfigArgs::default()
        .env_prefix("HYGIEA_TEST_WRONG_TYPE")
        .default_config_dir(dir.path().to_str().unwrap());
    let _ = Registry::load_config::<AppConfig>(&args);
}

#[test]
#[should_panic(expected = "failed to parse config")]
fn unknown_field_panics() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "dev.toml", "bogus_field = 1\n");
    let args = ConfigArgs::default()
        .env_prefix("HYGIEA_TEST_UNKNOWN_FIELD")
        .default_config_dir(dir.path().to_str().unwrap());
    let _ = Registry::load_config::<AppConfig>(&args);
}
