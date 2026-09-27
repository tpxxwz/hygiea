//! 组件依赖：`provides` / `depends_on` 决定启动顺序，跟 `add` 的顺序无关。
//!
//! ```bash
//! cargo run -p hygiea-examples --example app_dependencies
//! ```
//!
//! 先演示一个缺依赖的 Registry：在启动任何组件之前就报错。
//! 再启动一个正常的：依赖链是 Db ← Cache ← Api，`add` 时故意倒着写，按 Ctrl+C 退出。
//!
//! 组件都是假的（只打日志），不连真实服务，只看顺序。

use hygiea::HyErr;
use hygiea::app::{
    CancellationToken, Component, Name, Registry, ResourceId, Resources, async_trait,
};

// ---- 组件之间传递的资源 ----

#[derive(Clone)]
pub struct Db;

#[derive(Clone)]
pub struct Cache;

// ---- 数据库：不依赖别人，提供 Db ----

pub struct DbComponent;

#[async_trait]
impl Component for DbComponent {
    type Config = ();

    fn build(_name: Name, _config: Self::Config) -> Self {
        Self
    }

    fn provides(&self) -> Vec<ResourceId> {
        vec![ResourceId::of::<Db>()]
    }

    async fn startup(
        &mut self,
        resources: &Resources,
        _shutdown: CancellationToken,
    ) -> Result<Option<tokio::task::JoinHandle<()>>, HyErr> {
        tracing::info!("db ready");
        resources.insert(Db);
        Ok(None)
    }
}

// ---- 缓存：依赖 Db，提供 Cache ----

pub struct CacheComponent;

#[async_trait]
impl Component for CacheComponent {
    type Config = ();

    fn build(_name: Name, _config: Self::Config) -> Self {
        Self
    }

    fn provides(&self) -> Vec<ResourceId> {
        vec![ResourceId::of::<Cache>()]
    }

    fn depends_on(&self) -> Vec<ResourceId> {
        vec![ResourceId::of::<Db>()]
    }

    async fn startup(
        &mut self,
        resources: &Resources,
        _shutdown: CancellationToken,
    ) -> Result<Option<tokio::task::JoinHandle<()>>, HyErr> {
        // depends_on 里声明过，这时一定已经在了；require 仍然返回错误而不是 panic
        let _db = resources.require::<Db>()?;
        tracing::info!("cache ready (uses db)");
        resources.insert(Cache);
        Ok(None)
    }
}

// ---- 接口层：依赖 Db 和 Cache，不提供资源 ----

pub struct ApiComponent;

#[async_trait]
impl Component for ApiComponent {
    type Config = ();

    fn build(_name: Name, _config: Self::Config) -> Self {
        Self
    }

    fn depends_on(&self) -> Vec<ResourceId> {
        vec![ResourceId::of::<Db>(), ResourceId::of::<Cache>()]
    }

    async fn startup(
        &mut self,
        resources: &Resources,
        _shutdown: CancellationToken,
    ) -> Result<Option<tokio::task::JoinHandle<()>>, HyErr> {
        let _db = resources.require::<Db>()?;
        let _cache = resources.require::<Cache>()?;
        tracing::info!("api ready (uses db and cache)");
        Ok(None)
    }
}

#[tokio::main]
async fn main() -> Result<(), HyErr> {
    // 1. 缺依赖：Api 依赖 Db、Cache，但没有注册提供它们的组件。
    //    run 在启动任何组件之前就返回 ResourceMissing，闭包不会执行
    let (result, _log_guard) = Registry::new()
        .add::<ApiComponent>(())
        .run(|_| async { Ok(()) })
        .await;
    // 错误 Registry 已经打过 ERROR 日志，这里不用再打
    if result.is_err() {
        tracing::info!("registry refused to start, as expected");
    }

    // 2. 正常启动：add 的顺序是 Api、Cache、Db，实际启动顺序是 Db → Cache → Api，
    //    关闭顺序反过来：Api → Cache → Db
    let (result, _log_guard) = Registry::new()
        .add::<ApiComponent>(())
        .add::<CacheComponent>(())
        .add::<DbComponent>(())
        .run(|_| async {
            tracing::info!("all started, press Ctrl+C to exit");
            Ok(())
        })
        .await;
    result
}
