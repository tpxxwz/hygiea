//! PG 的锁表 `{app_name}_lock`：字符串 key 一行，加锁拿这一行的行锁。
//!
//! 建表规则和动态配置一样，只看 `current_schema()`：没有就建（advisory lock 串行化，拿到锁后再查一遍）；
//! 有且列对上就用；列对不上不动它，拒绝启动，哪里不对和 DDL 打在日志里。已经建好的表不会自动升级，
//! 改了 [`DDL`] 要同时改 [`EXPECTED`]，已有的库手动改

use std::sync::Arc;

use sqlx::{AssertSqlSafe, Connection, PgPool};
use tokio::sync::OnceCell;

use hygiea_core::app::AppErr;
use hygiea_core::lock::LockErr;
use hygiea_core::{Result, ResultExt, err};

use crate::RepoErr;
use crate::ddl::{advisory_key, is_ident, postgres_columns};

/// `{t}` 换成表名
const DDL: &str = "CREATE TABLE {t} (\n    key        text        PRIMARY KEY,\n    created_at timestamptz NOT NULL DEFAULT now()\n)";

/// 期望的列：(列名, udt_name)
const EXPECTED: [(&str, &str); 2] = [("key", "text"), ("created_at", "timestamptz")];

#[derive(Clone, Debug)]
pub(crate) struct LockTable {
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
            name: format!("{app_name}_lock"),
            ready: Arc::new(OnceCell::new()),
        })
    }

    fn ddl(&self) -> String {
        DDL.replace("{t}", &self.name)
    }

    pub(crate) fn select_skip_locked(&self) -> String {
        format!(
            "SELECT key FROM {} WHERE key = $1 FOR UPDATE SKIP LOCKED",
            self.name
        )
    }

    pub(crate) fn select_for_update(&self) -> String {
        format!("SELECT key FROM {} WHERE key = $1 FOR UPDATE", self.name)
    }

    /// 保证 key 那一行在（自动提交）。已有的行不加锁，别人正持有它也不会等
    pub(crate) async fn insert(&self, pool: &PgPool, key: &str) -> Result<()> {
        sqlx::query(AssertSqlSafe(format!(
            "INSERT INTO {} (key) VALUES ($1) ON CONFLICT DO NOTHING",
            self.name
        )))
        .bind(key)
        .execute(pool)
        .await
        .wrap_err(|| err!(LockErr::AcquireFailed, key))?;
        Ok(())
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
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(advisory_key(&self.name))
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

    /// 表在且列对上：`true`；不在：`false`；列对不上：拒绝启动
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
        if *actual == expected {
            return Ok(true);
        }
        tracing::error!(
            "lock table {}: columns expected {expected:?}, actual {actual:?}, left untouched; \
             fix it by hand (or use another app name) to match this DDL, then restart:\n{}",
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
        assert!(table.ddl().starts_with("CREATE TABLE dict_jp_lock ("));
        assert_eq!(
            table.select_skip_locked(),
            "SELECT key FROM dict_jp_lock WHERE key = $1 FOR UPDATE SKIP LOCKED"
        );
        assert!(LockTable::new("dict-jp").is_err());
    }
}
