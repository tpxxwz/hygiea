use crate::{BaseErr, HyErr, err};

pub trait EnvKey {
    fn key_name(&self) -> &str;
    fn default_value(&self) -> Option<String> {
        None
    }
}

// ---- BuiltinKey -------------------------------------------------------------

#[derive(Copy, Clone, Debug)]
pub enum BuiltinKey {
    DefaultHome,
    DefaultUser,
    DefaultPassword,
    Developer,
    // ---- local ----
    LocalPgHost,
    LocalPgPort,
    LocalPgDb,
    LocalPgUser,
    LocalPgPassword,
    LocalPgParams,
    LocalMysqlHost,
    LocalMysqlPort,
    LocalMysqlDb,
    LocalMysqlUser,
    LocalMysqlPassword,
    LocalMysqlParams,
    LocalRedisHost,
    LocalRedisPort,
    LocalRedisDb,
    LocalRedisPassword,
    LocalRedisParams,
    // ---- cloud host ----
    AliCloudHost,
    AwsCloudHost,
    // ---- cloud ----
    CloudPgHost,
    CloudPgPort,
    CloudPgDb,
    CloudPgUser,
    CloudPgPassword,
    CloudPgParams,
    CloudMysqlHost,
    CloudMysqlPort,
    CloudMysqlDb,
    CloudMysqlUser,
    CloudMysqlPassword,
    CloudMysqlParams,
    CloudRedisHost,
    CloudRedisPort,
    CloudRedisDb,
    CloudRedisPassword,
    CloudRedisParams,
}

impl EnvKey for BuiltinKey {
    fn key_name(&self) -> &str {
        match self {
            Self::DefaultHome => "DEFAULT_HOME",
            Self::DefaultUser => "DEFAULT_USER",
            Self::DefaultPassword => "DEFAULT_PASSWORD",
            Self::Developer => "DEVELOPER",
            Self::LocalPgHost => "LOCAL_PG_HOST",
            Self::LocalPgPort => "LOCAL_PG_PORT",
            Self::LocalPgDb => "LOCAL_PG_DB",
            Self::LocalPgUser => "LOCAL_PG_USER",
            Self::LocalPgPassword => "LOCAL_PG_PASSWORD",
            Self::LocalPgParams => "LOCAL_PG_PARAMS",
            Self::LocalMysqlHost => "LOCAL_MYSQL_HOST",
            Self::LocalMysqlPort => "LOCAL_MYSQL_PORT",
            Self::LocalMysqlDb => "LOCAL_MYSQL_DB",
            Self::LocalMysqlUser => "LOCAL_MYSQL_USER",
            Self::LocalMysqlPassword => "LOCAL_MYSQL_PASSWORD",
            Self::LocalMysqlParams => "LOCAL_MYSQL_PARAMS",
            Self::LocalRedisHost => "LOCAL_REDIS_HOST",
            Self::LocalRedisPort => "LOCAL_REDIS_PORT",
            Self::LocalRedisDb => "LOCAL_REDIS_DB",
            Self::LocalRedisPassword => "LOCAL_REDIS_PASSWORD",
            Self::LocalRedisParams => "LOCAL_REDIS_PARAMS",
            Self::AliCloudHost => "ALI_CLOUD_HOST",
            Self::AwsCloudHost => "AWS_CLOUD_HOST",
            Self::CloudPgHost => "CLOUD_PG_HOST",
            Self::CloudPgPort => "CLOUD_PG_PORT",
            Self::CloudPgDb => "CLOUD_PG_DB",
            Self::CloudPgUser => "CLOUD_PG_USER",
            Self::CloudPgPassword => "CLOUD_PG_PASSWORD",
            Self::CloudPgParams => "CLOUD_PG_PARAMS",
            Self::CloudMysqlHost => "CLOUD_MYSQL_HOST",
            Self::CloudMysqlPort => "CLOUD_MYSQL_PORT",
            Self::CloudMysqlDb => "CLOUD_MYSQL_DB",
            Self::CloudMysqlUser => "CLOUD_MYSQL_USER",
            Self::CloudMysqlPassword => "CLOUD_MYSQL_PASSWORD",
            Self::CloudMysqlParams => "CLOUD_MYSQL_PARAMS",
            Self::CloudRedisHost => "CLOUD_REDIS_HOST",
            Self::CloudRedisPort => "CLOUD_REDIS_PORT",
            Self::CloudRedisDb => "CLOUD_REDIS_DB",
            Self::CloudRedisPassword => "CLOUD_REDIS_PASSWORD",
            Self::CloudRedisParams => "CLOUD_REDIS_PARAMS",
        }
    }

    fn default_value(&self) -> Option<String> {
        match self {
            // Unix 先读 HOME，没有再查系统用户数据库；Windows 先读 USERPROFILE，没有再调系统 API。
            // 家目录不是合法 UTF-8 时转不成 String，按没有默认值处理
            Self::DefaultHome => std::env::home_dir()?.into_os_string().into_string().ok(),
            Self::LocalPgHost | Self::LocalMysqlHost | Self::LocalRedisHost => {
                Some("localhost".to_string())
            }
            Self::LocalPgPort => Some("5432".to_string()),
            Self::LocalMysqlPort => Some("3306".to_string()),
            Self::LocalRedisPort => Some("6379".to_string()),
            Self::LocalRedisDb => Some("0".to_string()),
            Self::LocalPgParams | Self::LocalMysqlParams | Self::LocalRedisParams => {
                Some(String::new())
            }
            Self::CloudPgParams | Self::CloudMysqlParams | Self::CloudRedisParams => {
                Some(String::new())
            }
            _ => None,
        }
    }
}

// ---- functions --------------------------------------------------------------

/// 取环境变量；没有时用 key 自带的 [`EnvKey::default_value`]，两者都没有返回 `EnvError`
pub fn env_get(key: impl EnvKey) -> Result<String, HyErr> {
    let name = key.key_name();
    std::env::var(name)
        .ok()
        .or_else(|| key.default_value())
        .ok_or_else(|| err!(BaseErr::EnvError, name))
}

/// 取环境变量，没有时返回 `None`；不看 key 自带的默认值
pub fn env_get_opt(key: impl EnvKey) -> Option<String> {
    std::env::var(key.key_name()).ok()
}

/// 取环境变量，没有时返回调用方给的 `fallback`；不看 key 自带的默认值
pub fn env_get_or(key: impl EnvKey, fallback: &str) -> String {
    std::env::var(key.key_name()).unwrap_or_else(|_| fallback.to_string())
}

/// 取环境变量，没有时调用 `f` 算出默认值（只在没有时才调用）；不看 key 自带的默认值
pub fn env_get_or_else(key: impl EnvKey, f: impl FnOnce() -> String) -> String {
    std::env::var(key.key_name()).unwrap_or_else(|_| f())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Key(&'static str, Option<&'static str>);
    impl EnvKey for Key {
        fn key_name(&self) -> &str {
            self.0
        }
        fn default_value(&self) -> Option<String> {
            self.1.map(str::to_string)
        }
    }

    #[test]
    fn env_get_missing_returns_err() {
        let err = env_get(Key("HYGIEA_TEST_SURELY_UNSET_VAR", None)).unwrap_err();
        assert!(err.is(BaseErr::EnvError));
        assert_eq!(
            err.to_string(),
            "Environment variable not set: HYGIEA_TEST_SURELY_UNSET_VAR"
        );
    }

    #[test]
    fn default_home_uses_std_home_dir() {
        let expected = std::env::home_dir().and_then(|p| p.into_os_string().into_string().ok());
        assert_eq!(BuiltinKey::DefaultHome.default_value(), expected);
    }

    #[test]
    fn env_get_falls_back_to_default() {
        let v = env_get(Key("HYGIEA_TEST_SURELY_UNSET_VAR", Some("dflt"))).unwrap();
        assert_eq!(v, "dflt");
    }

    /// env_get_opt 变量不存在时返回 None
    #[test]
    fn env_get_opt_missing_returns_none() {
        let v = env_get_opt(Key("HYGIEA_TEST_UNSET_OPT", None));
        assert_eq!(v, None);
    }

    /// env_get_or 变量不存在时返回 fallback，不看 key 的默认值
    #[test]
    fn env_get_or_missing_returns_fallback() {
        let v = env_get_or(Key("HYGIEA_TEST_UNSET_OR", Some("ignored")), "custom");
        assert_eq!(v, "custom");
    }

    /// env_get_or_else 变量不存在时才调用闭包
    #[test]
    fn env_get_or_else_missing_invokes_closure() {
        let call_count = std::cell::Cell::new(0);
        let v = env_get_or_else(Key("HYGIEA_TEST_UNSET_ELSE", None), || {
            call_count.set(call_count.get() + 1);
            "computed".to_string()
        });
        assert_eq!(v, "computed");
        assert_eq!(call_count.get(), 1);
    }

    /// env_get_or_else 变量存在时不调用闭包
    #[test]
    fn env_get_or_else_existing_does_not_invoke_closure() {
        let call_count = std::cell::Cell::new(0);
        // 用 PATH 作为一定存在的变量
        let v = env_get_or_else(Key("PATH", None), || {
            call_count.set(call_count.get() + 1);
            "should_not_be_called".to_string()
        });
        assert_ne!(v, "should_not_be_called");
        assert_eq!(call_count.get(), 0);
    }

    /// BuiltinKey 各变体的 key_name 对照表（简单验证几个代表性的）
    #[test]
    fn builtin_key_names() {
        assert_eq!(BuiltinKey::DefaultHome.key_name(), "DEFAULT_HOME");
        assert_eq!(BuiltinKey::DefaultUser.key_name(), "DEFAULT_USER");
        assert_eq!(BuiltinKey::Developer.key_name(), "DEVELOPER");
        assert_eq!(BuiltinKey::LocalPgHost.key_name(), "LOCAL_PG_HOST");
        assert_eq!(BuiltinKey::LocalRedisPort.key_name(), "LOCAL_REDIS_PORT");
        assert_eq!(BuiltinKey::AliCloudHost.key_name(), "ALI_CLOUD_HOST");
        assert_eq!(BuiltinKey::CloudMysqlDb.key_name(), "CLOUD_MYSQL_DB");
    }

    /// BuiltinKey 各变体的默认值检验
    #[test]
    fn builtin_key_defaults() {
        // 本地服务默认地址是 localhost
        assert_eq!(
            BuiltinKey::LocalPgHost.default_value(),
            Some("localhost".to_string())
        );
        assert_eq!(
            BuiltinKey::LocalMysqlHost.default_value(),
            Some("localhost".to_string())
        );
        assert_eq!(
            BuiltinKey::LocalRedisHost.default_value(),
            Some("localhost".to_string())
        );

        // 本地服务默认端口
        assert_eq!(
            BuiltinKey::LocalPgPort.default_value(),
            Some("5432".to_string())
        );
        assert_eq!(
            BuiltinKey::LocalMysqlPort.default_value(),
            Some("3306".to_string())
        );
        assert_eq!(
            BuiltinKey::LocalRedisPort.default_value(),
            Some("6379".to_string())
        );

        // Redis 默认 db
        assert_eq!(
            BuiltinKey::LocalRedisDb.default_value(),
            Some("0".to_string())
        );

        // 无默认值的变体
        assert_eq!(BuiltinKey::DefaultPassword.default_value(), None);
        assert_eq!(BuiltinKey::CloudPgUser.default_value(), None);
    }
}
