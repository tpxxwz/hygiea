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
            Self::DefaultHome => {
                let user = std::env::var("USER")
                    .or_else(|_| std::env::var("USERNAME"))
                    .ok()?;
                if cfg!(target_os = "macos") {
                    Some(format!("/Users/{}", user))
                } else if cfg!(target_os = "linux") {
                    Some(if user == "root" {
                        "/root".to_string()
                    } else {
                        format!("/home/{}", user)
                    })
                } else if cfg!(windows) {
                    Some(format!("C:\\Users\\{}", user))
                } else {
                    None
                }
            }
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

/// Returns the env var value, falling back to `default_value`. Panics if both absent.
pub fn env_get(key: impl EnvKey) -> String {
    let name = key.key_name();
    std::env::var(name)
        .ok()
        .or_else(|| key.default_value())
        .unwrap_or_else(|| panic!("environment variable not set: {}", name))
}

/// Returns the env var value if present, otherwise `None`.
pub fn env_get_opt(key: impl EnvKey) -> Option<String> {
    std::env::var(key.key_name()).ok()
}

/// Returns the env var value, using `fallback` when absent.
pub fn env_get_or(key: impl EnvKey, fallback: &str) -> String {
    std::env::var(key.key_name()).unwrap_or_else(|_| fallback.to_string())
}

/// Returns the env var value, calling `f` when absent.
pub fn env_get_or_else(key: impl EnvKey, f: impl FnOnce() -> String) -> String {
    std::env::var(key.key_name()).unwrap_or_else(|_| f())
}
