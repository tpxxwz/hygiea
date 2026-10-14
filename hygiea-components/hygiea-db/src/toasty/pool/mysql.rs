//! MySQL 连接池组件（toasty），feature `toasty` + `mysql`。占位：只按 `[db.connect]` 连上，池参数用 toasty 的默认值

use serde::Deserialize;

use hygiea_core::app::{
    AppErr, ConfigResource, ImmediateResourceComponent, RegistryConfig, async_trait,
};
use hygiea_core::{Result, ResultExt, err};

use crate::config::MySqlConnectConfig;

/// 配置文件里是 `[db.connect]`，不写用默认值
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToastyMySqlConfig {
    pub connect: MySqlConnectConfig,
}

/// MySQL 连接池，`Deref` 到 toasty 的 `Db`。用 [`ConfigResource::from_config`] 建，不经过 Registry 也能用
#[derive(Clone)]
pub struct ToastyMySqlPool {
    inner: toasty::Db,
}

impl std::fmt::Debug for ToastyMySqlPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToastyMySqlPool").finish_non_exhaustive()
    }
}

#[async_trait]
impl ConfigResource for ToastyMySqlPool {
    type Config = ToastyMySqlConfig;

    async fn from_config(
        config: &ToastyMySqlConfig,
        registry_config: &RegistryConfig,
    ) -> Result<Self> {
        let target = config.connect.target();
        let url = config.connect.url(registry_config)?;
        tracing::info!("Connecting to database: {target}");
        let inner = toasty::Db::builder()
            .connect(&url)
            .await
            .wrap_err(|| err!(AppErr::ConnectFailed, target))?;
        tracing::info!("Database connection established");
        Ok(Self { inner })
    }

    /// toasty 没有关闭池的接口，最后一个 `Db` 被丢掉时连接才关
    async fn close(&self) -> Result<()> {
        Ok(())
    }
}

impl std::ops::Deref for ToastyMySqlPool {
    type Target = toasty::Db;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// MySQL 连接池组件（toasty），按组件名放进 Resources 的是 [`ToastyMySqlPool`]。用法见 [`ImmediateResourceComponent`]
pub type ToastyMySqlComponent = ImmediateResourceComponent<ToastyMySqlPool>;
