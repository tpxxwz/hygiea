//! PostgreSQL 锁的 sqlx 实现（规则在 [`super::super::table`]，SQL 在 `sql/postgres/lock.rs`）。`SqlxPgPool`、`SeaOrmPgPool` 共用：
//! SeaORM 交出它底下的 sqlx 池

use std::time::Duration;

#[cfg(sqlx_lock_row)]
use sqlx::Connection;
use sqlx::pool::PoolConnection;
use sqlx::{AssertSqlSafe, PgConnection, PgPool, Postgres};

use hygiea_core::app::RegistryConfig;
use hygiea_core::lock::{LockErr, LockGuard, LockKey};
use hygiea_core::{Result, ResultExt, err};

#[cfg(not(sqlx_lock_row))]
use super::super::hashed_name;
#[cfg(sqlx_lock_row)]
use super::super::table::LockTable;
use super::super::{i64_name, pair_name};
#[cfg(sqlx_lock_row)]
use crate::RepoErr;
#[cfg(sqlx_lock_row)]
use crate::sql::postgres::DDL_LOCK;
#[cfg(sqlx_lock_row)]
use crate::sql::postgres::lock::TABLE_PERSISTENCE;
use crate::sql::postgres::lock::{LOCK_I64, LOCK_PAIR, TRY_I64, TRY_PAIR, timeout_millis};
use crate::sql::postgres::{LOCK_NOT_AVAILABLE, begin_sql};
#[cfg(sqlx_lock_row)]
use crate::sqlx::internal::postgres::columns;

/// 连接池上锁的实现，`SqlxPgPool`、`SeaOrmPgPool` 各持有一个（主池的 sqlx 池），对外的方法见 [`impl_pg_lock`]
#[derive(Clone, Debug)]
pub(crate) struct PgLock {
    pool: PgPool,
    #[cfg(sqlx_lock_row)]
    table: LockTable,
    /// 锁表检查、建表做过一次就不再做
    #[cfg(sqlx_lock_row)]
    table_ready: std::sync::Arc<tokio::sync::OnceCell<()>>,
}

impl PgLock {
    /// 只建对象，不连库；锁表在 [`prepare`](Self::prepare) 里建
    pub(crate) fn new(pool: PgPool, registry_config: &RegistryConfig) -> Result<Self> {
        #[cfg(not(sqlx_lock_row))]
        let _ = registry_config;
        Ok(Self {
            pool,
            #[cfg(sqlx_lock_row)]
            table: LockTable::new(&registry_config.app_name)?,
            #[cfg(sqlx_lock_row)]
            table_ready: Default::default(),
        })
    }

    /// 启动时调用：开了 `lock-row` 就检查、建锁表
    pub(crate) async fn prepare(&self) -> Result<()> {
        #[cfg(sqlx_lock_row)]
        self.ensure_table().await?;
        Ok(())
    }

    pub(crate) async fn try_i64(&self, key: i64) -> Result<Option<LockGuard>> {
        self.try_advisory(key, i64_name(key)).await
    }

    /// `name` 是日志、错误、guard 里显示的 key
    async fn try_advisory(&self, key: i64, name: String) -> Result<Option<LockGuard>> {
        let mut tx = self.begin(0, &name).await?;
        let got: bool = sqlx::query_scalar(TRY_I64)
            .bind(key)
            .fetch_one(tx.conn())
            .await
            .wrap_err(|| acquire_err(&name))?;
        Ok(guard(tx, name, got).await)
    }

    pub(crate) async fn try_pair(&self, key1: i32, key2: i32) -> Result<Option<LockGuard>> {
        let name = pair_name(key1, key2);
        let mut tx = self.begin(0, &name).await?;
        let got: bool = sqlx::query_scalar(TRY_PAIR)
            .bind(key1)
            .bind(key2)
            .fetch_one(tx.conn())
            .await
            .wrap_err(|| acquire_err(&name))?;
        Ok(guard(tx, name, got).await)
    }

    pub(crate) async fn lock_i64(&self, key: i64, timeout: Duration) -> Result<Option<LockGuard>> {
        self.lock_advisory(key, i64_name(key), timeout).await
    }

    async fn lock_advisory(
        &self,
        key: i64,
        name: String,
        timeout: Duration,
    ) -> Result<Option<LockGuard>> {
        if timeout.is_zero() {
            return self.try_advisory(key, name).await;
        }
        let millis = timeout_millis(timeout)?;
        let mut tx = self.begin(millis, &name).await?;
        let result = sqlx::query(LOCK_I64).bind(key).execute(tx.conn()).await;
        let got = waited(result.map(|_| ()), &name)?;
        Ok(guard(tx, name, got).await)
    }

    pub(crate) async fn lock_pair(
        &self,
        key1: i32,
        key2: i32,
        timeout: Duration,
    ) -> Result<Option<LockGuard>> {
        if timeout.is_zero() {
            return self.try_pair(key1, key2).await;
        }
        let millis = timeout_millis(timeout)?;
        let name = pair_name(key1, key2);
        let mut tx = self.begin(millis, &name).await?;
        let result = sqlx::query(LOCK_PAIR)
            .bind(key1)
            .bind(key2)
            .execute(tx.conn())
            .await;
        let got = waited(result.map(|_| ()), &name)?;
        Ok(guard(tx, name, got).await)
    }

    /// 字符串 key：在锁表里插入 key 那一行、不提交。INSERT 没有 `NOWAIT` / `SKIP LOCKED`，
    /// 别人持有时最多等 1 毫秒（`lock_timeout` 的最小值，0 在 PG 里是不限时），等不到返回 `None`
    #[cfg(sqlx_lock_row)]
    pub(crate) async fn try_key(&self, key: &LockKey) -> Result<Option<LockGuard>> {
        self.insert_key(key, 1).await
    }

    #[cfg(sqlx_lock_row)]
    pub(crate) async fn lock_key(
        &self,
        key: &LockKey,
        timeout: Duration,
    ) -> Result<Option<LockGuard>> {
        if timeout.is_zero() {
            return self.try_key(key).await;
        }
        self.insert_key(key, timeout_millis(timeout)?).await
    }

    /// 插入 key 那一行，最多等 `millis` 毫秒。撞上已经提交的行（被人手动插入）报 23505，作为加锁失败报出来
    #[cfg(sqlx_lock_row)]
    async fn insert_key(&self, key: &LockKey, millis: i32) -> Result<Option<LockGuard>> {
        let name = key.as_str()?.to_string();
        self.ensure_table().await?;
        let mut tx = self.begin(millis, &name).await?;
        let result = sqlx::query(sqlx::AssertSqlSafe(self.table.insert()))
            .bind(&name)
            .execute(tx.conn())
            .await;
        let got = waited(result.map(|_| ()), &name)?;
        Ok(guard(tx, name, got).await)
    }

    /// 没开 `lock-row`：字符串 key 哈希成 i64 走 advisory lock，不同 key 可能撞到同一把锁
    #[cfg(not(sqlx_lock_row))]
    pub(crate) async fn try_key(&self, key: &LockKey) -> Result<Option<LockGuard>> {
        let hashed = key.hashed_i64()?.get();
        self.try_advisory(hashed, hashed_name(hashed, key)?).await
    }

    #[cfg(not(sqlx_lock_row))]
    pub(crate) async fn lock_key(
        &self,
        key: &LockKey,
        timeout: Duration,
    ) -> Result<Option<LockGuard>> {
        let hashed = key.hashed_i64()?.get();
        self.lock_advisory(hashed, hashed_name(hashed, key)?, timeout)
            .await
    }

    /// 开锁的事务，`lock_timeout_ms` 见 [`begin_sql`]
    async fn begin(&self, lock_timeout_ms: i32, name: &str) -> Result<LockTx> {
        LockTx::begin(&self.pool, lock_timeout_ms)
            .await
            .wrap_err(|| acquire_err(name))
    }
}

fn acquire_err(name: &str) -> hygiea_core::HyErr {
    err!(LockErr::AcquireFailed, name)
}

/// 锁的事务。不用 sqlx 的 `Transaction`：`begin_with` 中途报错时拿不回连接，它会带着失败的事务回到池里。
/// 这里自己取连接、发 BEGIN，正常结束靠 [`rollback`](Self::rollback)；没走到 rollback 就被丢掉（出错、取消、回滚失败）时
/// 把连接关掉，不还回池
struct LockTx {
    conn: PoolConnection<Postgres>,
    open: bool,
}

impl LockTx {
    async fn begin(pool: &PgPool, lock_timeout_ms: i32) -> sqlx::Result<Self> {
        let mut tx = Self {
            conn: pool.acquire().await?,
            open: true,
        };
        sqlx::raw_sql(AssertSqlSafe(begin_sql(lock_timeout_ms)))
            .execute(tx.conn())
            .await?;
        Ok(tx)
    }

    fn conn(&mut self) -> &mut PgConnection {
        &mut self.conn
    }

    async fn rollback(mut self) -> sqlx::Result<()> {
        sqlx::raw_sql("ROLLBACK").execute(self.conn()).await?;
        self.open = false;
        Ok(())
    }
}

impl Drop for LockTx {
    fn drop(&mut self) {
        if self.open {
            self.conn.close_on_drop();
        }
    }
}

/// 拿到了：事务交给 guard，释放时回滚；没拿到：当场回滚，失败时连接被关掉，不影响结果
async fn guard(tx: LockTx, name: String, got: bool) -> Option<LockGuard> {
    if !got {
        let _ = tx.rollback().await;
        return None;
    }
    let key = name.clone();
    Some(LockGuard::new(name, async move {
        tx.rollback()
            .await
            .wrap_err(|| err!(LockErr::ReleaseFailed, key))
    }))
}

/// 阻塞等锁的结果：拿到是 `true`，超时（[`LOCK_NOT_AVAILABLE`]）是 `false`，其他错误照常报
fn waited(result: sqlx::Result<()>, name: &str) -> Result<bool> {
    match result {
        Ok(()) => Ok(true),
        Err(e) if is_lock_timeout(&e) => Ok(false),
        Err(e) => Err(e).wrap_err(|| acquire_err(name)),
    }
}

fn is_lock_timeout(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .and_then(|e| e.code())
        .is_some_and(|code| code == LOCK_NOT_AVAILABLE)
}

// ---- 锁表（lock-row）的检查和建表 ---------------------------------------------------

#[cfg(sqlx_lock_row)]
impl PgLock {
    /// 检查、建表，只做一次
    async fn ensure_table(&self) -> Result<()> {
        self.table_ready
            .get_or_try_init(|| async {
                let mut conn = self
                    .pool
                    .acquire()
                    .await
                    .wrap_err(|| self.table.check_err())?;
                let result = self.ensure_table_on(&mut conn).await;
                if result.is_err() {
                    // 连接可能带着失败的事务（begin_with 中途报错时 sqlx 不回滚），关掉不还回池
                    conn.close_on_drop();
                }
                result
            })
            .await?;
        Ok(())
    }

    async fn ensure_table_on(&self, conn: &mut PgConnection) -> Result<()> {
        let table = &self.table;
        if self.table_ready_or_broken(conn).await? {
            return Ok(());
        }
        // 等建表锁不被 DBA 设的超时打断，见 sql::postgres::begin_sql
        let mut tx = conn
            .begin_with(AssertSqlSafe(begin_sql(0)))
            .await
            .wrap_err(|| table.check_err())?;
        sqlx::query(DDL_LOCK)
            .bind(hygiea_core::builtin::LOCK_TYPE)
            .bind(hygiea_core::builtin::Lock::LockTableDdl as i32)
            .execute(&mut *tx)
            .await
            .wrap_err(|| table.check_err())?;
        // 拿到锁后再查一遍：多个实例同时启动，前一个可能已经建好了
        if self.table_ready_or_broken(&mut tx).await? {
            return Ok(());
        }
        let result = async {
            sqlx::raw_sql(AssertSqlSafe(table.ddl()))
                .execute(&mut *tx)
                .await?;
            tx.commit().await
        }
        .await;
        if result.is_err() {
            tracing::error!(
                "lock table: create failed, run this DDL manually:\n{}",
                table.ddl()
            );
        }
        result.wrap_err(|| err!(RepoErr::LockTableCreateFailed, table.name().to_string()))?;
        tracing::info!(table = table.name(), "lock table: created");
        Ok(())
    }

    /// 表在、列对上且是 UNLOGGED：`true`；不在：`false`；对不上：拒绝启动
    async fn table_ready_or_broken(&self, conn: &mut PgConnection) -> Result<bool> {
        let table = &self.table;
        let found = columns(conn, vec![table.name().to_string()])
            .await
            .wrap_err(|| table.check_err())?;
        if !table.check_columns(&found)? {
            return Ok(false);
        }
        let persistence: Option<String> = sqlx::query_scalar(TABLE_PERSISTENCE)
            .bind(table.name())
            .fetch_optional(&mut *conn)
            .await
            .wrap_err(|| table.check_err())?;
        table.check_persistence(persistence.as_deref())?;
        Ok(true)
    }
}
