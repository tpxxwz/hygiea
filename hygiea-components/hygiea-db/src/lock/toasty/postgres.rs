//! PostgreSQL 锁的 toasty 实现，和 sqlx 的实现（`lock/sqlx/postgres.rs`）用同一份 SQL 和规则，方法名一样，
//! 共用 `impl_pg_lock!`。和 sqlx 的不同：
//! - toasty 原生 SQL 一次只能执行一条：BEGIN 和设超时分两条发，带超时的加锁是三次往返
//! - toasty 不能把连接从池里拿掉或关掉：锁的事务没走到 ROLLBACK 就被丢掉（出错、取消）时，在后台发 ROLLBACK 再还回池。
//!   等锁时被取消，ROLLBACK 排在那条加锁语句后面，要等它结束（拿到锁或超时）连接才回到池里

use std::time::Duration;

use hygiea_core::app::RegistryConfig;
use hygiea_core::lock::{LockErr, LockGuard, LockKey};
use hygiea_core::{Result, ResultExt, err};

#[cfg(not(toasty_lock_row))]
use super::super::hashed_name;
#[cfg(toasty_lock_row)]
use super::super::table::LockTable;
use super::super::{i64_name, pair_name};
use crate::sql::postgres::LOCK_NOT_AVAILABLE;
use crate::sql::postgres::lock::{LOCK_I64, LOCK_PAIR, TRY_I64, TRY_PAIR, timeout_millis};
use crate::toasty::internal::postgres::{begin, sqlstate};
use crate::toasty::internal::{bool, single};

/// 连接池上锁的实现，`ToastyPgPool` 持有，对外的方法见 `impl_pg_lock!`
#[derive(Clone)]
pub(crate) struct ToastyPgLock {
    db: toasty::Db,
    #[cfg(toasty_lock_row)]
    table: LockTable,
    /// 锁表检查、建表做过一次就不再做
    #[cfg(toasty_lock_row)]
    table_ready: std::sync::Arc<tokio::sync::OnceCell<()>>,
}

impl std::fmt::Debug for ToastyPgLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToastyPgLock").finish_non_exhaustive()
    }
}

impl ToastyPgLock {
    /// 只建对象，不连库；锁表在 [`prepare`](Self::prepare) 里建
    pub(crate) fn new(db: toasty::Db, registry_config: &RegistryConfig) -> Result<Self> {
        #[cfg(not(toasty_lock_row))]
        let _ = registry_config;
        Ok(Self {
            db,
            #[cfg(toasty_lock_row)]
            table: LockTable::new(&registry_config.app_name)?,
            #[cfg(toasty_lock_row)]
            table_ready: Default::default(),
        })
    }

    /// 启动时调用：开了 `lock-row` 就检查、建锁表
    pub(crate) async fn prepare(&self) -> Result<()> {
        #[cfg(toasty_lock_row)]
        self.ensure_table().await?;
        Ok(())
    }

    pub(crate) async fn try_i64(&self, key: i64) -> Result<Option<LockGuard>> {
        self.try_advisory(key, i64_name(key)).await
    }

    async fn try_advisory(&self, key: i64, name: String) -> Result<Option<LockGuard>> {
        let mut tx = self.begin(0, &name).await?;
        let got = async {
            let rows = toasty::sql::query(TRY_I64)
                .bind(key)
                .exec(tx.conn()?)
                .await?;
            bool(single(rows)?)
        }
        .await
        .wrap_err(|| acquire_err(&name))?;
        Ok(guard(tx, name, got).await)
    }

    pub(crate) async fn try_pair(&self, key1: i32, key2: i32) -> Result<Option<LockGuard>> {
        let name = pair_name(key1, key2);
        let mut tx = self.begin(0, &name).await?;
        let got = async {
            let rows = toasty::sql::query(TRY_PAIR)
                .bind(key1)
                .bind(key2)
                .exec(tx.conn()?)
                .await?;
            bool(single(rows)?)
        }
        .await
        .wrap_err(|| acquire_err(&name))?;
        Ok(guard(tx, name, got).await)
    }

    pub(crate) async fn lock_i64(&self, key: i64, timeout: Duration) -> Result<Option<LockGuard>> {
        if timeout.is_zero() {
            return self.try_i64(key).await;
        }
        let name = i64_name(key);
        let mut tx = self.begin(timeout_millis(timeout)?, &name).await?;
        let result = async {
            toasty::sql::statement(LOCK_I64)
                .bind(key)
                .exec(tx.conn()?)
                .await
        }
        .await;
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
        let mut tx = self.begin(timeout_millis(timeout)?, &name).await?;
        let result = async {
            toasty::sql::statement(LOCK_PAIR)
                .bind(key1)
                .bind(key2)
                .exec(tx.conn()?)
                .await
        }
        .await;
        let got = waited(result, &name)?;
        Ok(guard(tx, name, got).await)
    }

    /// 字符串 key：在锁表里插入 key 那一行、不提交，见 `lock/sqlx/postgres.rs` 的同名方法
    #[cfg(toasty_lock_row)]
    pub(crate) async fn try_key(&self, key: &LockKey) -> Result<Option<LockGuard>> {
        self.insert_key(key, 1).await
    }

    #[cfg(toasty_lock_row)]
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

    #[cfg(toasty_lock_row)]
    async fn insert_key(&self, key: &LockKey, millis: i32) -> Result<Option<LockGuard>> {
        let name = key.as_str()?.to_string();
        self.ensure_table().await?;
        let mut tx = self.begin(millis, &name).await?;
        let insert = self.table.insert();
        let result = async {
            toasty::sql::statement(insert)
                .bind(name.as_str())
                .exec(tx.conn()?)
                .await
        }
        .await;
        let got = waited(result, &name)?;
        Ok(guard(tx, name, got).await)
    }

    /// 没开 `lock-row`：字符串 key 哈希成 i64 走 advisory lock
    #[cfg(not(toasty_lock_row))]
    pub(crate) async fn try_key(&self, key: &LockKey) -> Result<Option<LockGuard>> {
        let hashed = key.hashed_i64()?.get();
        self.try_advisory(hashed, hashed_name(hashed, key)?).await
    }

    #[cfg(not(toasty_lock_row))]
    pub(crate) async fn lock_key(
        &self,
        key: &LockKey,
        timeout: Duration,
    ) -> Result<Option<LockGuard>> {
        let hashed = key.hashed_i64()?.get();
        if timeout.is_zero() {
            return self.try_advisory(hashed, hashed_name(hashed, key)?).await;
        }
        let name = hashed_name(hashed, key)?;
        let mut tx = self.begin(timeout_millis(timeout)?, &name).await?;
        let result = async {
            toasty::sql::statement(LOCK_I64)
                .bind(hashed)
                .exec(tx.conn()?)
                .await
        }
        .await;
        let got = waited(result, &name)?;
        Ok(guard(tx, name, got).await)
    }

    async fn begin(&self, lock_timeout_ms: i32, name: &str) -> Result<LockTx> {
        LockTx::begin(&self.db, lock_timeout_ms)
            .await
            .wrap_err(|| acquire_err(name))
    }
}

fn acquire_err(name: &str) -> hygiea_core::HyErr {
    err!(LockErr::AcquireFailed, name)
}

/// 锁的事务：一条固定的连接上 BEGIN 了还没结束。`conn` 是 `None` 表示已经回滚
struct LockTx {
    conn: Option<toasty::Connection>,
}

impl LockTx {
    async fn begin(db: &toasty::Db, lock_timeout_ms: i32) -> toasty::Result<Self> {
        let mut tx = Self {
            conn: Some(db.connection().await?),
        };
        // 报错时 tx 被丢掉，Drop 里回滚
        begin(tx.conn()?, lock_timeout_ms).await?;
        Ok(tx)
    }

    fn conn(&mut self) -> toasty::Result<&mut toasty::Connection> {
        self.conn
            .as_mut()
            .ok_or_else(|| toasty::Error::invalid_result("lock transaction already finished"))
    }

    async fn rollback(mut self) -> toasty::Result<()> {
        if let Some(mut conn) = self.conn.take() {
            toasty::sql::statement("ROLLBACK").exec(&mut conn).await?;
        }
        Ok(())
    }
}

impl Drop for LockTx {
    /// 没走到 [`rollback`](LockTx::rollback)：在后台回滚再还回池。不在 tokio 运行时里（极少见）只能直接还回池，
    /// 连接带着没结束的事务，锁要等这条连接被池回收、断开才释放
    fn drop(&mut self) {
        if let Some(mut conn) = self.conn.take()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            runtime.spawn(async move {
                let _ = toasty::sql::statement("ROLLBACK").exec(&mut conn).await;
            });
        }
    }
}

/// 拿到了：事务交给 guard，释放时回滚；没拿到：当场回滚，失败时由 Drop 在后台再试
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
fn waited(result: toasty::Result<u64>, name: &str) -> Result<bool> {
    match result {
        Ok(_) => Ok(true),
        Err(e) if sqlstate(&e).as_deref() == Some(LOCK_NOT_AVAILABLE) => Ok(false),
        Err(e) => Err(e).wrap_err(|| acquire_err(name)),
    }
}

// ---- 锁表（lock-row）的检查和建表 ---------------------------------------------------

#[cfg(toasty_lock_row)]
impl ToastyPgLock {
    /// 检查、建表，只做一次
    async fn ensure_table(&self) -> Result<()> {
        self.table_ready
            .get_or_try_init(|| self.ensure_table_once())
            .await?;
        Ok(())
    }

    async fn ensure_table_once(&self) -> Result<()> {
        let table = &self.table;
        let mut conn = self.db.connection().await.wrap_err(|| table.check_err())?;
        if self.table_ready_or_broken(&mut conn).await? {
            return Ok(());
        }
        // 等建表锁不被 DBA 设的超时打断，见 sql::postgres::begin_sql
        begin(&mut conn, 0).await.wrap_err(|| table.check_err())?;
        let result = self.create_table(&mut conn).await;
        // 没提交的都回滚（建好了已经提交，这条只是警告）
        let _ = toasty::sql::statement("ROLLBACK").exec(&mut conn).await;
        result
    }

    async fn create_table(&self, conn: &mut toasty::Connection) -> Result<()> {
        let table = &self.table;
        toasty::sql::statement(crate::sql::postgres::DDL_LOCK)
            .bind(hygiea_core::builtin::LOCK_TYPE)
            .bind(hygiea_core::builtin::Lock::LockTableDdl as i32)
            .exec(conn)
            .await
            .wrap_err(|| table.check_err())?;
        // 拿到锁后再查一遍：多个实例同时启动，前一个可能已经建好了
        if self.table_ready_or_broken(conn).await? {
            return Ok(());
        }
        let result: toasty::Result<()> = async {
            toasty::sql::statement(table.ddl()).exec(conn).await?;
            toasty::sql::statement("COMMIT").exec(conn).await?;
            Ok(())
        }
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
    async fn table_ready_or_broken(&self, conn: &mut toasty::Connection) -> Result<bool> {
        let table = &self.table;
        let found =
            crate::toasty::internal::postgres::columns(conn, vec![table.name().to_string()])
                .await
                .wrap_err(|| table.check_err())?;
        if !table.check_columns(&found)? {
            return Ok(false);
        }
        let persistence = async {
            let rows = toasty::sql::query(crate::sql::postgres::lock::TABLE_PERSISTENCE)
                .bind(table.name())
                .exec(conn)
                .await?;
            match rows.into_iter().next() {
                Some(row) => single(vec![row]).and_then(crate::toasty::internal::opt_string),
                None => Ok(None),
            }
        }
        .await
        .wrap_err(|| table.check_err())?;
        table.check_persistence(persistence.as_deref())?;
        Ok(true)
    }
}
