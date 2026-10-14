//! PostgreSQL 锁的 tokio-postgres 实现，和 sqlx 的实现（`lock/sqlx/postgres.rs`）同一份 SQL 和规则、同样的方法名，
//! 共用 `impl_pg_lock!`。连接从 deadpool-postgres 的池里借

use std::time::Duration;

use deadpool_postgres::{Object, Pool};

use hygiea_core::app::RegistryConfig;
use hygiea_core::lock::{LockErr, LockGuard, LockKey};
use hygiea_core::{Result, ResultExt, err};

#[cfg(not(tokio_pg_lock_row))]
use super::super::hashed_name;
#[cfg(tokio_pg_lock_row)]
use super::super::table::LockTable;
use super::super::{i64_name, pair_name};
use crate::sql::postgres::LOCK_NOT_AVAILABLE;
use crate::sql::postgres::lock::{LOCK_I64, LOCK_PAIR, TRY_I64, TRY_PAIR, timeout_millis};
use crate::tokio_postgres::internal::{begin, sqlstate};

/// 连接池上锁的实现，`TokioPgPool` 持有，对外的方法见 `impl_pg_lock!`
#[derive(Clone)]
pub(crate) struct TokioPgLock {
    pool: Pool,
    #[cfg(tokio_pg_lock_row)]
    table: LockTable,
    /// 锁表检查、建表做过一次就不再做
    #[cfg(tokio_pg_lock_row)]
    table_ready: std::sync::Arc<tokio::sync::OnceCell<()>>,
}

impl TokioPgLock {
    /// 只建对象，不连库；锁表在 [`prepare`](Self::prepare) 里建
    pub(crate) fn new(pool: Pool, registry_config: &RegistryConfig) -> Result<Self> {
        #[cfg(not(tokio_pg_lock_row))]
        let _ = registry_config;
        Ok(Self {
            pool,
            #[cfg(tokio_pg_lock_row)]
            table: LockTable::new(&registry_config.app_name)?,
            #[cfg(tokio_pg_lock_row)]
            table_ready: Default::default(),
        })
    }

    /// 启动时调用：开了 `lock-row` 就检查、建锁表
    pub(crate) async fn prepare(&self) -> Result<()> {
        #[cfg(tokio_pg_lock_row)]
        self.ensure_table().await?;
        Ok(())
    }

    pub(crate) async fn try_i64(&self, key: i64) -> Result<Option<LockGuard>> {
        self.try_advisory(key, i64_name(key)).await
    }

    async fn try_advisory(&self, key: i64, name: String) -> Result<Option<LockGuard>> {
        let tx = self.begin(0, &name).await?;
        let got = async {
            let row = tx.conn()?.query_one(TRY_I64, &[&key]).await?;
            Ok::<bool, TxErr>(row.try_get(0)?)
        }
        .await
        .wrap_err(|| acquire_err(&name))?;
        Ok(guard(tx, name, got).await)
    }

    pub(crate) async fn try_pair(&self, key1: i32, key2: i32) -> Result<Option<LockGuard>> {
        let name = pair_name(key1, key2);
        let tx = self.begin(0, &name).await?;
        let got = async {
            let row = tx.conn()?.query_one(TRY_PAIR, &[&key1, &key2]).await?;
            Ok::<bool, TxErr>(row.try_get(0)?)
        }
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
        let tx = self.begin(timeout_millis(timeout)?, &name).await?;
        let result = execute(&tx, LOCK_I64, &[&key]).await;
        let got = waited(result, &name)?;
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
        let name = pair_name(key1, key2);
        let tx = self.begin(timeout_millis(timeout)?, &name).await?;
        let result = execute(&tx, LOCK_PAIR, &[&key1, &key2]).await;
        let got = waited(result, &name)?;
        Ok(guard(tx, name, got).await)
    }

    /// 字符串 key：在锁表里插入 key 那一行、不提交，见 `lock/sqlx/postgres.rs` 的同名方法
    #[cfg(tokio_pg_lock_row)]
    pub(crate) async fn try_key(&self, key: &LockKey) -> Result<Option<LockGuard>> {
        self.insert_key(key, 1).await
    }

    #[cfg(tokio_pg_lock_row)]
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

    #[cfg(tokio_pg_lock_row)]
    async fn insert_key(&self, key: &LockKey, millis: i32) -> Result<Option<LockGuard>> {
        let name = key.as_str()?.to_string();
        self.ensure_table().await?;
        let tx = self.begin(millis, &name).await?;
        let result = execute(&tx, &self.table.insert(), &[&name]).await;
        let got = waited(result, &name)?;
        Ok(guard(tx, name, got).await)
    }

    /// 没开 `lock-row`：字符串 key 哈希成 i64 走 advisory lock，不同 key 可能撞到同一把锁
    #[cfg(not(tokio_pg_lock_row))]
    pub(crate) async fn try_key(&self, key: &LockKey) -> Result<Option<LockGuard>> {
        let hashed = key.hashed_i64()?.get();
        self.try_advisory(hashed, hashed_name(hashed, key)?).await
    }

    #[cfg(not(tokio_pg_lock_row))]
    pub(crate) async fn lock_key(
        &self,
        key: &LockKey,
        timeout: Duration,
    ) -> Result<Option<LockGuard>> {
        let hashed = key.hashed_i64()?.get();
        self.lock_advisory(hashed, hashed_name(hashed, key)?, timeout)
            .await
    }

    /// 开锁的事务，`lock_timeout_ms` 见 `sql::postgres::begin_sql`
    async fn begin(&self, lock_timeout_ms: i32, name: &str) -> Result<LockTx> {
        LockTx::begin(&self.pool, lock_timeout_ms)
            .await
            .wrap_err(|| acquire_err(name))
    }
}

fn acquire_err(name: &str) -> hygiea_core::HyErr {
    err!(LockErr::AcquireFailed, name)
}

/// 锁的事务：从池里借一条连接、BEGIN。正常结束靠 [`rollback`](Self::rollback)；没走到 rollback 就被丢掉
/// （出错、取消、回滚失败）时把连接从池里拿掉（`Object::take`，池可以另建一条补上）再丢掉，断开即释放，不还回池。
/// `conn` 只在 Drop 里取走，平时一直是 `Some`
struct LockTx {
    conn: Option<Object>,
}

impl LockTx {
    async fn begin(pool: &Pool, lock_timeout_ms: i32) -> std::result::Result<Self, TxErr> {
        let tx = Self {
            conn: Some(pool.get().await?),
        };
        begin(tx.conn()?, lock_timeout_ms).await?;
        Ok(tx)
    }

    fn conn(&self) -> std::result::Result<&Object, TxErr> {
        self.conn.as_ref().ok_or(TxErr::Finished)
    }

    async fn rollback(mut self) -> std::result::Result<(), TxErr> {
        self.conn()?.batch_execute("ROLLBACK").await?;
        // 回滚成功：正常还回池
        drop(self.conn.take());
        Ok(())
    }
}

impl Drop for LockTx {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            drop(Object::take(conn));
        }
    }
}

/// 锁的事务上的错误：借连接、执行语句
#[derive(Debug)]
enum TxErr {
    Pool(deadpool_postgres::PoolError),
    Pg(tokio_postgres::Error),
    /// 事务已经结束（不会发生：结束后 LockTx 就被消耗了）
    Finished,
}

impl std::fmt::Display for TxErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pool(e) => e.fmt(f),
            Self::Pg(e) => e.fmt(f),
            Self::Finished => f.write_str("lock transaction already finished"),
        }
    }
}

impl std::error::Error for TxErr {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Pool(e) => Some(e),
            Self::Pg(e) => Some(e),
            Self::Finished => None,
        }
    }
}

impl From<deadpool_postgres::PoolError> for TxErr {
    fn from(e: deadpool_postgres::PoolError) -> Self {
        Self::Pool(e)
    }
}

impl From<tokio_postgres::Error> for TxErr {
    fn from(e: tokio_postgres::Error) -> Self {
        Self::Pg(e)
    }
}

/// 拿到了：事务交给 guard，释放时回滚；没拿到：当场回滚，失败时连接被扔掉，不影响结果
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

/// 在锁的事务上执行加锁语句
async fn execute(
    tx: &LockTx,
    sql: &str,
    params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
) -> std::result::Result<u64, TxErr> {
    Ok(tx.conn()?.execute(sql, params).await?)
}

/// 阻塞等锁的结果：拿到是 `true`，超时（[`LOCK_NOT_AVAILABLE`]）是 `false`，其他错误照常报
fn waited(result: std::result::Result<u64, TxErr>, name: &str) -> Result<bool> {
    match result {
        Ok(_) => Ok(true),
        Err(TxErr::Pg(e)) if sqlstate(&e) == Some(LOCK_NOT_AVAILABLE) => Ok(false),
        Err(e) => Err(e).wrap_err(|| acquire_err(name)),
    }
}

// ---- 锁表（lock-row）的检查和建表 ---------------------------------------------------

#[cfg(tokio_pg_lock_row)]
impl TokioPgLock {
    /// 检查、建表，只做一次
    async fn ensure_table(&self) -> Result<()> {
        self.table_ready
            .get_or_try_init(|| self.ensure_table_once())
            .await?;
        Ok(())
    }

    /// 在借来的一条连接上检查、建表；出错时连接上可能留着失败的事务，从池里拿掉不还回去
    async fn ensure_table_once(&self) -> Result<()> {
        let table = &self.table;
        let conn = self.pool.get().await.wrap_err(|| table.check_err())?;
        let result = self.ensure_table_on(&conn).await;
        if result.is_err() {
            drop(Object::take(conn));
        }
        result
    }

    async fn ensure_table_on(&self, conn: &Object) -> Result<()> {
        let table = &self.table;
        if self.table_ready_or_broken(conn).await? {
            return Ok(());
        }
        // 等建表锁不被 DBA 设的超时打断，见 sql::postgres::begin_sql
        begin(conn, 0).await.wrap_err(|| table.check_err())?;
        conn.execute(
            crate::sql::postgres::DDL_LOCK,
            &[
                &hygiea_core::builtin::LOCK_TYPE,
                &(hygiea_core::builtin::Lock::LockTableDdl as i32),
            ],
        )
        .await
        .wrap_err(|| table.check_err())?;
        // 拿到锁后再查一遍：多个实例同时启动，前一个可能已经建好了
        if self.table_ready_or_broken(conn).await? {
            conn.batch_execute("ROLLBACK")
                .await
                .wrap_err(|| table.check_err())?;
            return Ok(());
        }
        let result = conn
            .batch_execute(&format!("{}; COMMIT", table.ddl()))
            .await;
        if result.is_err() {
            tracing::error!(
                "lock table: create failed, run this DDL manually:\n{}",
                table.ddl()
            );
        }
        result.wrap_err(|| {
            err!(
                crate::RepoErr::LockTableCreateFailed,
                table.name().to_string()
            )
        })?;
        tracing::info!(table = table.name(), "lock table: created");
        Ok(())
    }

    /// 表在、列对上且是 UNLOGGED：`true`；不在：`false`；对不上：拒绝启动
    async fn table_ready_or_broken(&self, conn: &Object) -> Result<bool> {
        let table = &self.table;
        let found = crate::tokio_postgres::internal::columns(conn, vec![table.name().to_string()])
            .await
            .wrap_err(|| table.check_err())?;
        if !table.check_columns(&found)? {
            return Ok(false);
        }
        let persistence: Option<String> = conn
            .query_opt(
                crate::sql::postgres::lock::TABLE_PERSISTENCE,
                &[&table.name()],
            )
            .await
            .and_then(|row| row.map(|row| row.try_get(0)).transpose())
            .wrap_err(|| table.check_err())?;
        table.check_persistence(persistence.as_deref())?;
        Ok(true)
    }
}
