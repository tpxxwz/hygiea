//! PG 的锁表 `{p}lock`（UNLOGGED，`{p}` 是表名前缀，见 [`LockConfig`](super::LockConfig)）的规则，和驱动无关：加锁是在事务里插入 key 那一行、不提交，
//! 靠主键冲突互斥，释放时回滚，没有已提交的行。
//!
//! 建表规则和动态配置一样，只看 `current_schema()`：没有就建（advisory lock `hygiea_core::builtin` 的 `(LOCK_TYPE, Lock::LockTableDdl)` 串行化，拿到锁后再查一遍）；
//! 有且列对上、是 UNLOGGED 就用；对不上不动它，拒绝启动，哪里不对和 DDL 打在日志里。已经建好的表不会自动升级，
//! SQL（DDL、期望的列、查询）在 `sql/postgres/lock.rs`，执行在各驱动的实现里（现在是 `sqlx/table.rs`）

use hygiea_core::{Result, err};

use crate::RepoErr;
use crate::sql::Columns;
use crate::sql::postgres::lock::{TABLE_COLUMNS, TABLE_DDL, table_insert};

#[derive(Clone, Debug)]
pub(crate) struct LockTable {
    /// 已经校验过，空的或者以 `_` 结尾
    prefix: String,
    name: String,
}

impl LockTable {
    /// 表名是前缀加 `lock`，前缀的规则见 [`table_prefix`](crate::sql::table_prefix)
    pub(crate) fn new(prefix: String) -> Self {
        Self {
            name: format!("{prefix}lock"),
            prefix,
        }
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn ddl(&self) -> String {
        TABLE_DDL.replace("{p}", &self.prefix)
    }

    /// 加锁的语句，见 [`table_insert`]
    pub(crate) fn insert(&self) -> String {
        table_insert(&self.name)
    }

    /// 查到的列（`sql::postgres::COLUMNS` 的结果）：表不在是 `false`，列对上是 `true`，对不上拒绝启动
    pub(crate) fn check_columns(&self, columns: &Columns) -> Result<bool> {
        let Some(actual) = columns.get(&self.name) else {
            return Ok(false);
        };
        let expected: Vec<(String, String)> = TABLE_COLUMNS
            .iter()
            .map(|(name, ty)| (name.to_string(), ty.to_string()))
            .collect();
        if *actual == expected {
            return Ok(true);
        }
        tracing::error!(
            "lock table {}: columns expected {expected:?}, actual {actual:?}, left untouched; \
             fix it by hand (or use another table_prefix) to match this DDL, then restart:\n{}",
            self.name,
            self.ddl()
        );
        Err(err!(RepoErr::LockTableSchemaMismatch, self.name.clone()))
    }

    /// 查到的持久化方式（`TABLE_PERSISTENCE` 的结果）：UNLOGGED 才能用，不是拒绝启动
    pub(crate) fn check_persistence(&self, persistence: Option<&str>) -> Result<()> {
        if persistence == Some("u") {
            return Ok(());
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

    /// 检查、建表时查询失败
    pub(crate) fn check_err(&self) -> hygiea_core::HyErr {
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
    fn names_follow_prefix() {
        let table = LockTable::new("dict_jp_".to_string());
        assert!(table.ddl().contains("CREATE UNLOGGED TABLE dict_jp_lock ("));
        assert_eq!(table.insert(), "INSERT INTO dict_jp_lock (key) VALUES ($1)");
        assert_eq!(LockTable::new(String::new()).name(), "lock");
    }

    /// 表不在、列对上、列对不上、不是 UNLOGGED
    #[test]
    fn checks_columns_and_persistence() {
        let table = LockTable::new("hy_".to_string());
        let mut columns = Columns::new();
        assert!(!table.check_columns(&columns).unwrap());
        columns.insert("hy_lock".into(), vec![("key".into(), "text".into())]);
        assert!(table.check_columns(&columns).unwrap());
        columns.insert("hy_lock".into(), vec![("key".into(), "int8".into())]);
        assert!(table.check_columns(&columns).is_err());
        assert!(table.check_persistence(Some("u")).is_ok());
        assert!(table.check_persistence(Some("p")).is_err());
    }
}
