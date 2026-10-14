//! MySQL 锁的 sqlx 实现：会话级命名锁 `GET_LOCK`（规则和 SQL 在 `sql/mysql/lock.rs`）。`SqlxMySqlPool`、`SeaOrmMySqlPool` 共用：
//! SeaORM 交出它底下的 sqlx 池

use std::time::Duration;

use sqlx::pool::PoolConnection;
use sqlx::{MySql, MySqlConnection, MySqlPool};

use hygiea_core::app::{AppErr, RegistryConfig};
use hygiea_core::lock::{LockErr, LockGuard, LockKey};
use hygiea_core::{Result, ResultExt, err};

use super::super::MySqlLockConfig;
use crate::config::{MYSQL_POOL_SIZE, MySqlConnectConfig, PoolConfig};
use crate::sql::mysql::lock::{
    ACQUIRE, ACQUIRED, RELEASE, RESTORE, i64_name, key_name, pair_name, timeout_secs,
};

/// 连接池上锁的实现，`SqlxMySqlPool`、`SeaOrmMySqlPool` 各持有一个，对外的方法见 [`impl_lock`](super::super::impl_lock)
#[derive(Clone, Debug)]
pub(crate) struct MySqlLock {
    /// 连接池本身的 sqlx 池，或者按 `[db.lock]` 单独建的池
    pool: MySqlPool,
    /// `pool` 是单独建的：连接池关闭时一起关
    own_pool: bool,
}

impl MySqlLock {
    /// `main` 是连接池本身的 sqlx 池，`connect`、`pool` 是它的配置。`[db.lock]` 的 `connect`、`max_connections` 都没写时
    /// 用 `main`；写了就单独建池（见 [`own_pool`]），连接池是 `connect_lazy` 时它也不连，否则连上确认能用
    pub(crate) async fn new(
        main: &MySqlPool,
        connect: &MySqlConnectConfig,
        pool: &PoolConfig,
        config: &MySqlLockConfig,
        registry_config: &RegistryConfig,
    ) -> Result<Self> {
        let own = !config.connect.is_empty() || config.max_connections.is_some();
        let lock_pool = if own {
            own_pool(connect, pool, config, registry_config).await?
        } else {
            main.clone()
        };
        Ok(Self {
            pool: lock_pool,
            own_pool: own,
        })
    }

    /// 连接池关闭时调用：单独建的池等借出去的连接（持有中的锁）还回来后关掉；用连接池本身的什么都不做
    pub(crate) async fn close(&self) {
        if self.own_pool {
            self.pool.close().await;
        }
    }

    pub(crate) async fn try_key(&self, key: &LockKey) -> Result<Option<LockGuard>> {
        self.acquire(key_name(key.as_str()?)?, 0).await
    }

    pub(crate) async fn lock_key(
        &self,
        key: &LockKey,
        timeout: Duration,
    ) -> Result<Option<LockGuard>> {
        let name = key_name(key.as_str()?)?;
        self.acquire(name, wait_secs(timeout)?).await
    }

    pub(crate) async fn try_i64(&self, key: i64) -> Result<Option<LockGuard>> {
        self.acquire(i64_name(key), 0).await
    }

    pub(crate) async fn lock_i64(&self, key: i64, timeout: Duration) -> Result<Option<LockGuard>> {
        self.acquire(i64_name(key), wait_secs(timeout)?).await
    }

    pub(crate) async fn try_pair(&self, key1: i32, key2: i32) -> Result<Option<LockGuard>> {
        self.acquire(pair_name(key1, key2), 0).await
    }

    pub(crate) async fn lock_pair(
        &self,
        key1: i32,
        key2: i32,
        timeout: Duration,
    ) -> Result<Option<LockGuard>> {
        self.acquire(pair_name(key1, key2), wait_secs(timeout)?)
            .await
    }

    /// `GET_LOCK(name, secs)`，0 是不等。拿到了连接交给 guard，释放时 `RELEASE_LOCK`；没拿到当场恢复 `wait_timeout`、
    /// 还回池。等锁时被 KILL 等返回 NULL、语句报错，都是 [`LockErr::AcquireFailed`]
    async fn acquire(&self, name: String, secs: u64) -> Result<Option<LockGuard>> {
        let acquire_err = || err!(LockErr::AcquireFailed, name.clone());
        let mut held = Held(Some(self.pool.acquire().await.wrap_err(acquire_err)?));
        sqlx::query(ACQUIRE)
            .bind(&name)
            .bind(secs)
            .execute(held.conn()?)
            .await
            .wrap_err(acquire_err)?;
        let got: Option<i64> = sqlx::query_scalar(ACQUIRED)
            .fetch_one(held.conn()?)
            .await
            .wrap_err(acquire_err)?;
        match got {
            Some(1) => Ok(Some(LockGuard::new(name.clone(), async move {
                held.release(&name).await
            }))),
            Some(0) => {
                // 恢复失败时连接被关掉，不影响结果
                let _ = held.restore().await;
                Ok(None)
            }
            _ => Err(err!(
                LockErr::AcquireFailed,
                format!("{name}: GET_LOCK returned {got:?}")
            )),
        }
    }
}

/// 阻塞版的秒数，0 是不等（等于 try），见 [`timeout_secs`]
fn wait_secs(timeout: Duration) -> Result<u64> {
    if timeout.is_zero() {
        return Ok(0);
    }
    timeout_secs(timeout)
}

/// 锁单独用的池：连接参数是 `[db.lock.connect]` 盖到连接池的 `[db.connect]` 上。池参数只有 `max_connections` 能配
/// （不写沿用连接池的），其余固定：不预先建连接、空闲 10 分钟回收、最长存活 30 分钟、拿之前先检查，acquire 超时跟连接池走。
/// 空闲回收、到期替换只作用于还回池里的连接，持有中的锁那条连接不会被断开
async fn own_pool(
    connect: &MySqlConnectConfig,
    pool: &PoolConfig,
    config: &MySqlLockConfig,
    registry_config: &RegistryConfig,
) -> Result<MySqlPool> {
    let connect = config.connect.merge(connect);
    let target = connect.target();
    let options = connect.sqlx_options(registry_config)?;
    let pool_options = sqlx::pool::PoolOptions::<MySql>::new()
        .max_connections(
            config
                .max_connections
                .unwrap_or_else(|| pool.max_connections(MYSQL_POOL_SIZE)),
        )
        .min_connections(0)
        .acquire_timeout(Duration::from_secs(pool.acquire_timeout_secs()))
        .idle_timeout(Duration::from_secs(600))
        .max_lifetime(Duration::from_secs(1800))
        .test_before_acquire(true);
    if pool.connect_lazy {
        return Ok(pool_options.connect_lazy_with(options));
    }
    tracing::info!("Connecting lock pool: {target}");
    pool_options
        .connect_with(options)
        .await
        .wrap_err(|| err!(AppErr::ConnectFailed, format!("lock pool {target}")))
}

/// 加锁用的连接。从发出 `GET_LOCK` 起连接上可能挂着锁、`wait_timeout` 是改过的，只有走完
/// [`release`](Self::release) / [`restore`](Self::restore) 才把连接还回池；没走完就被丢掉（出错、取消、恢复失败）时
/// 把连接从池里摘下来直接断开（`detach` 后 drop），池马上能补一条新的，断开即释放锁。
/// 不用 `close_on_drop`：sqlx 的 MySQL 连接要先读完上一条语句的结果才发关闭，等锁时被取消的话要等到 `GET_LOCK` 超时，
/// 这段时间一直占着池的名额（实测：断开后 9.7 的服务端立刻结束等待，8.4 在持有者释放后结束，锁都不会留在断开的会话上）
struct Held(Option<PoolConnection<MySql>>);

impl Held {
    /// 连接只在走完 release / restore 或 drop 时拿走，之前一直在
    fn conn(&mut self) -> Result<&mut MySqlConnection> {
        self.0.as_deref_mut().ok_or_else(|| {
            err!(
                LockErr::AcquireFailed,
                "mysql: lock connection already returned"
            )
        })
    }

    async fn release(mut self, name: &str) -> Result<()> {
        sqlx::query(RELEASE)
            .bind(name)
            .execute(self.conn()?)
            .await
            .wrap_err(|| err!(LockErr::ReleaseFailed, name.to_string()))?;
        // 干净了，还回池
        self.0.take();
        Ok(())
    }

    async fn restore(mut self) -> Result<()> {
        sqlx::query(RESTORE)
            .execute(self.conn()?)
            .await
            .wrap_err(|| err!(LockErr::AcquireFailed, "mysql: restore wait_timeout"))?;
        self.0.take();
        Ok(())
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some(conn) = self.0.take() {
            drop(conn.detach());
        }
    }
}
