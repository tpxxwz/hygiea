//! PG 的锁表 `{app_name}_lock`（UNLOGGED）：加锁是在事务里插入 key 那一行、不提交，靠主键冲突互斥，释放时回滚，表一直是空的。
//!
//! 建表规则和动态配置一样，只看 `current_schema()`：没有就建（advisory lock `hygiea_core::builtin` 的 `(LOCK_TYPE, Lock::LockTableDdl)` 串行化，拿到锁后再查一遍）；
//! 有且列对上、是 UNLOGGED 就用；对不上不动它，拒绝启动，哪里不对和 DDL 打在日志里。已经建好的表不会自动升级，
//! 改了 `sql/postgres.sql` 的列要同时改 [`EXPECTED`]，已有的库手动改

use std::sync::Arc;

use sqlx::{AssertSqlSafe, Connection, PgPool};
use tokio::sync::OnceCell;

use hygiea_core::app::AppErr;
use hygiea_core::{Result, ResultExt, err};

use crate::RepoErr;
use crate::ddl::{is_ident, postgres_columns};

/// 建表语句：`{p}` 换成前缀（应用名加 `_`）
const DDL: &str = include_str!("sql/postgres.sql");

/// 期望的列：(列名, udt_name)
const EXPECTED: [(&str, &str); 1] = [("key", "text")];

#[derive(Clone, Debug)]
pub(crate) struct LockTable {
    /// 应用名加 `_`
    prefix: String,
    name: String,
    /// 检查、建表做过一次就不再做
    ready: Arc<OnceCell<()>>,
}

impl LockTable {
    /// 表名是应用名加 `_lock`，应用名拼进 SQL，得是小写下划线（经过 Registry 时入口已经校验过，这里再查一次）
    pub(crate) fn new(app_name: &str) -> Result<Self> {
        if !is_ident(app_name) {
            return Err(err!(
                AppErr::InvalidConfig,
                format!(
                    "lock table: app name {app_name:?} is the table prefix, needs snake_case [a-z][a-z0-9_]*"
                )
            ));
        }
        Ok(Self {
            prefix: format!("{app_name}_"),
            name: format!("{app_name}_lock"),
            ready: Arc::new(OnceCell::new()),
        })
    }

    fn ddl(&self) -> String {
        DDL.replace("{p}", &self.prefix)
    }

    /// 加锁：插入 key 那一行，不提交。别人正持有（插了没提交）时等它的事务结束，最多等事务的 `lock_timeout`
    pub(crate) fn insert(&self) -> String {
        format!("INSERT INTO {} (key) VALUES ($1)", self.name)
    }

    /// 检查、建表，只做一次
    pub(crate) async fn ensure(&self, pool: &PgPool) -> Result<()> {
        self.ready
            .get_or_try_init(|| async {
                let mut conn = pool.acquire().await.wrap_err(|| self.check_err())?;
                self.ensure_on(&mut conn).await
            })
            .await?;
        Ok(())
    }

    async fn ensure_on(&self, conn: &mut sqlx::PgConnection) -> Result<()> {
        if self.ready_or_broken(conn).await? {
            return Ok(());
        }
        let mut tx = conn.begin().await.wrap_err(|| self.check_err())?;
        sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
            .bind(hygiea_core::builtin::LOCK_TYPE)
            .bind(hygiea_core::builtin::Lock::LockTableDdl as i32)
            .execute(&mut *tx)
            .await
            .wrap_err(|| self.check_err())?;
        // 拿到锁后再查一遍：多个实例同时启动，前一个可能已经建好了
        if self.ready_or_broken(&mut tx).await? {
            return Ok(());
        }
        let result = async {
            sqlx::raw_sql(AssertSqlSafe(self.ddl()))
                .execute(&mut *tx)
                .await?;
            tx.commit().await
        }
        .await;
        if result.is_err() {
            tracing::error!(
                "lock table: create failed, run this DDL manually:\n{}",
                self.ddl()
            );
        }
        result.wrap_err(|| err!(RepoErr::LockTableCreateFailed, self.name.clone()))?;
        tracing::info!(table = self.name, "lock table: created");
        Ok(())
    }

    /// 表在、列对上且是 UNLOGGED：`true`；不在：`false`；对不上：拒绝启动
    async fn ready_or_broken(&self, conn: &mut sqlx::PgConnection) -> Result<bool> {
        let columns = postgres_columns(conn, vec![self.name.clone()])
            .await
            .wrap_err(|| self.check_err())?;
        let Some(actual) = columns.get(&self.name) else {
            return Ok(false);
        };
        let expected: Vec<(String, String)> = EXPECTED
            .iter()
            .map(|(name, ty)| (name.to_string(), ty.to_string()))
            .collect();
        if *actual != expected {
            tracing::error!(
                "lock table {}: columns expected {expected:?}, actual {actual:?}, left untouched; \
                 fix it by hand (or use another app name) to match this DDL, then restart:\n{}",
                self.name,
                self.ddl()
            );
            return Err(err!(RepoErr::LockTableSchemaMismatch, self.name.clone()));
        }
        // relpersistence：p 普通表，u UNLOGGED
        let persistence: Option<String> = sqlx::query_scalar(
            "SELECT c.relpersistence::text FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = current_schema() AND c.relname = $1",
        )
        .bind(&self.name)
        .fetch_optional(&mut *conn)
        .await
        .wrap_err(|| self.check_err())?;
        if persistence.as_deref() == Some("u") {
            return Ok(true);
        }
        tracing::error!(
            "lock table {}: expected UNLOGGED, actual relpersistence {persistence:?}, left untouched; \
             fix it by hand (ALTER TABLE {} SET UNLOGGED), then restart:\n{}",
            self.name,
            self.name,
            self.ddl()
        );
        Err(err!(RepoErr::LockTableSchemaMismatch, self.name.clone()))
    }

    fn check_err(&self) -> hygiea_core::HyErr {
        err!(
            RepoErr::QueryFailed,
            format!("lock table check {}", self.name)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_app_name() {
        let table = LockTable::new("dict_jp").unwrap();
        assert!(table.ddl().contains("CREATE UNLOGGED TABLE dict_jp_lock ("));
        assert_eq!(table.insert(), "INSERT INTO dict_jp_lock (key) VALUES ($1)");
        assert!(LockTable::new("dict-jp").is_err());
    }
}
