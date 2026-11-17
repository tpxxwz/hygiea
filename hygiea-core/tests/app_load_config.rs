//! `Registry::load_config` 的配置分层加载：环境文件 < `-f` 额外文件 < 环境变量 < `--set`，
//! `<PREFIX>_ENV` 选择环境，`-C` / `default_config_dir` 决定去哪个目录读文件；`ConfigArgs::fixed` 定死环境名，
//! `ConfigSourceArgs` 是不带 `--env` 的命令行参数。
//!
//! `load_config` 内部会调用 `Registry::with_config` 装全局日志：本文件里第一个成功的装上，
//! 其余的走 `InstallFailed` 静默沿用（见 `app_registry_log.rs`），不影响这里只关心配置本身的断言。
//!
//! 每个测试用不同的 `env_prefix`、各自的 `tempfile` 目录，避免互相干扰；`set_var` / `remove_var`
//! 在 2024 edition 是 unsafe 的，都写了 SAFETY 注释。

#![cfg(feature = "app")]

use hygiea_core::app::clap::{self, Parser};
use hygiea_core::app::{
    ConfigArgs, ConfigSourceArgs, IntoRegistryConfig, Registry, RegistryConfig,
};
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

/// 写配置文件，末尾补上必填的 `[registry] app_name`（顶层的 key 要写在段之前）。
/// app_name 要和环境变量前缀对得上，用前缀的小写
fn write(dir: &std::path::Path, file: &str, content: &str, prefix: &str) {
    let app_name = prefix.to_ascii_lowercase();
    write_raw(
        dir,
        file,
        &format!("{content}\n[registry]\napp_name = \"{app_name}\"\n"),
    );
}

fn write_raw(dir: &std::path::Path, file: &str, content: &str) {
    std::fs::write(dir.join(file), content).unwrap();
}

// ---- 1. 分层合并：环境文件 < -f 文件 < 环境变量 < --set --------------------

#[test]
fn config_layers_override_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let prefix = "HYGIEA_TEST_LAYERS";
    write(dir.path(), "dev.toml", "name = \"from-dev\"\n", prefix);
    write(dir.path(), "local.toml", "name = \"from-local\"\n", prefix);

    let base = ConfigArgs::default()
        .env_prefix(prefix)
        .default_config_dir(dir.path().to_str().unwrap());

    // 只有环境文件
    let (_registry, config) = Registry::load_config::<AppConfig>(&base);
    assert_eq!(config.name, "from-dev");

    // -f local：覆盖环境文件
    let mut with_extra_file = base.clone();
    with_extra_file.source.configs = vec!["local".to_string()];
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
    with_override.source.overrides = vec![("name".to_string(), "from-set".to_string())];
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
    let prefix = "HYGIEA_TEST_ENVSELECT";
    write(dir.path(), "dev.toml", "name = \"dev-env\"\n", prefix);
    write(dir.path(), "prod.toml", "name = \"prod-env\"\n", prefix);

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
    let prefix = "HYGIEA_TEST_CONFIGDIR";
    let default_dir = tempfile::tempdir().unwrap();
    write(
        default_dir.path(),
        "dev.toml",
        "name = \"from-default-dir\"\n",
        prefix,
    );
    let cli_dir = tempfile::tempdir().unwrap();
    write(
        cli_dir.path(),
        "dev.toml",
        "name = \"from-cli-dir\"\n",
        prefix,
    );

    let args = ConfigArgs::default()
        .env_prefix(prefix)
        .default_config_dir(default_dir.path().to_str().unwrap());

    // 只设了 default_config_dir：读它
    let (_registry, config) = Registry::load_config::<AppConfig>(&args);
    assert_eq!(config.name, "from-default-dir");

    // 命令行 -C（config_dir 字段）覆盖代码里设的默认目录
    let mut with_cli_dir = args.clone();
    with_cli_dir.source.config_dir = Some(cli_dir.path().to_str().unwrap().to_string());
    let (_registry, config) = Registry::load_config::<AppConfig>(&with_cli_dir);
    assert_eq!(config.name, "from-cli-dir");
}

// ---- 4. 环境变量里的数字和布尔值能正确解析 ----------------------------------

#[test]
fn env_var_parses_numbers_and_booleans() {
    let dir = tempfile::tempdir().unwrap();
    let prefix = "HYGIEA_TEST_TYPES";
    write(dir.path(), "dev.toml", "", prefix);

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
    write(
        dir.path(),
        "dev.toml",
        "count = \"not-a-number\"\n",
        "HYGIEA_TEST_WRONG_TYPE",
    );
    let args = ConfigArgs::default()
        .env_prefix("HYGIEA_TEST_WRONG_TYPE")
        .default_config_dir(dir.path().to_str().unwrap());
    let _ = Registry::load_config::<AppConfig>(&args);
}

/// app_name 和环境变量前缀对不上：启动失败
#[test]
#[should_panic(expected = "needs env prefix")]
fn app_name_prefix_mismatch_panics() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "dev.toml", "", "OTHER_APP");
    let args = ConfigArgs::default()
        .env_prefix("HYGIEA_TEST_PREFIX_MISMATCH")
        .default_config_dir(dir.path().to_str().unwrap());
    let _ = Registry::load_config::<AppConfig>(&args);
}

/// 不写 app_name：默认 `hygiea`，和默认前缀 `HYGIEA` 对得上；只设了前缀没写 app_name 就对不上
#[test]
fn default_app_name_matches_default_prefix() {
    let dir = tempfile::tempdir().unwrap();
    write_raw(dir.path(), "dev.toml", "name = \"x\"\n");
    let args = ConfigArgs::default().default_config_dir(dir.path().to_str().unwrap());
    let (_registry, config) = Registry::load_config::<AppConfig>(&args);
    assert_eq!(config.registry.app_name, "hygiea");
}

#[test]
#[should_panic(expected = "needs env prefix")]
fn prefix_without_app_name_panics() {
    let dir = tempfile::tempdir().unwrap();
    write_raw(dir.path(), "dev.toml", "name = \"x\"\n");
    let args = ConfigArgs::default()
        .env_prefix("HYGIEA_TEST_NO_APP_NAME")
        .default_config_dir(dir.path().to_str().unwrap());
    let _ = Registry::load_config::<AppConfig>(&args);
}

#[test]
#[should_panic(expected = "failed to parse config")]
fn unknown_field_panics() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "dev.toml",
        "bogus_field = 1\n",
        "HYGIEA_TEST_UNKNOWN_FIELD",
    );
    let args = ConfigArgs::default()
        .env_prefix("HYGIEA_TEST_UNKNOWN_FIELD")
        .default_config_dir(dir.path().to_str().unwrap());
    let _ = Registry::load_config::<AppConfig>(&args);
}

// ---- 6. 环境名固定（ConfigArgs::fixed）、不带 --env 的命令行参数 ---------------

#[test]
fn fixed_env_reads_fixed_file_and_ignores_env_var() {
    let dir = tempfile::tempdir().unwrap();
    let prefix = "HYGIEA_TEST_FIXED";
    write(dir.path(), "myapp.toml", "name = \"from-fixed\"\n", prefix);
    write(dir.path(), "prod.toml", "name = \"from-prod\"\n", prefix);
    write(dir.path(), "local.toml", "name = \"from-local\"\n", prefix);
    let dir_text = dir.path().to_str().unwrap().to_string();
    let args = |source: ConfigSourceArgs| {
        ConfigArgs::fixed(source, "myapp")
            .env_prefix(prefix)
            .default_config_dir(dir_text.clone())
    };
    // 选环境的环境变量指向 prod：fixed 时不读它
    let env_key = format!("{prefix}_ENV");
    // SAFETY: 测试代码，env_key 带独有前缀，不会跟其他测试冲突
    unsafe {
        std::env::set_var(&env_key, "prod");
    }

    let only_fixed = args(ConfigSourceArgs::default());
    assert_eq!(only_fixed.config_file(), format!("{dir_text}/myapp.toml"));
    let (_registry, config) = Registry::load_config::<AppConfig>(&only_fixed);
    assert_eq!(config.name, "from-fixed");

    // -f 叠加、--set 覆盖照常
    let layered = args(ConfigSourceArgs {
        configs: vec!["local".to_string()],
        overrides: vec![("count".to_string(), "7".to_string())],
        ..Default::default()
    });
    let (_registry, config) = Registry::load_config::<AppConfig>(&layered);
    assert_eq!(config.name, "from-local");
    assert_eq!(config.count, 7);

    // SAFETY: 同上
    unsafe {
        std::env::remove_var(&env_key);
    }
}

#[test]
fn source_args_cli_has_no_env_flag() {
    #[derive(clap::Parser)]
    struct Cli {
        #[command(flatten)]
        source: ConfigSourceArgs,
    }
    let cli = Cli::try_parse_from([
        "app",
        "-f",
        "local,secret",
        "-C",
        "/etc/app",
        "--set",
        "a.b=1",
    ])
    .unwrap();
    assert_eq!(cli.source.configs, ["local", "secret"]);
    assert_eq!(cli.source.config_dir.as_deref(), Some("/etc/app"));
    assert_eq!(cli.source.overrides, [("a.b".to_string(), "1".to_string())]);
    assert!(Cli::try_parse_from(["app", "--env", "prod"]).is_err());
}

#[test]
fn config_args_cli_still_parses_all_flags() {
    // ConfigArgs 把 ConfigSourceArgs flatten 进来之后，命令行的写法和原来一样
    #[derive(clap::Parser)]
    struct Cli {
        #[command(flatten)]
        config: ConfigArgs,
    }
    let cli = Cli::try_parse_from([
        "app", "--env", "prod", "-f", "local", "-C", "/etc/app", "--set", "a.b=1",
    ])
    .unwrap();
    assert_eq!(cli.config.env.as_deref(), Some("prod"));
    assert_eq!(cli.config.source.configs, ["local"]);
    assert_eq!(cli.config.source.config_dir.as_deref(), Some("/etc/app"));
    assert_eq!(
        cli.config.source.overrides,
        [("a.b".to_string(), "1".to_string())]
    );
    assert_eq!(cli.config.config_file(), "/etc/app/prod.toml");
}

#[cfg(feature = "app-daemon")]
#[test]
fn daemon_args_and_config_source_args_share_a_cli() {
    // 合进同一个命令行：-d 是后台、-C 是配置目录，不冲突（重名的话 clap 在 debug 构建里直接 panic）
    #[derive(clap::Parser)]
    struct Cli {
        #[command(flatten)]
        source: ConfigSourceArgs,
        #[command(flatten)]
        daemon: hygiea_core::app::daemon::DaemonArgs,
    }
    let cli = Cli::try_parse_from(["app", "-d", "-C", "/etc/app", "-f", "local"]).unwrap();
    assert!(cli.daemon.detach);
    assert_eq!(cli.source.config_dir.as_deref(), Some("/etc/app"));
    let cli = Cli::try_parse_from(["app", "--config-dir", "/etc/app"]).unwrap();
    assert!(!cli.daemon.detach);
}
