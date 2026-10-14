//! 锁、动态配置的 tokio-postgres 实现共用的执行部分（SQL 和规则在 [`crate::sql`]）

use tokio_postgres::Client;
#[cfg(any(tokio_pg_live_config, tokio_pg_lock_row))]
use tokio_postgres::Row;

#[cfg(any(tokio_pg_live_config, tokio_pg_lock_row))]
use crate::sql::postgres::COLUMNS;
use crate::sql::postgres::begin_sql;
#[cfg(any(tokio_pg_live_config, tokio_pg_lock_row))]
use crate::sql::{Columns, group_columns};

/// 开框架的事务，见 [`begin_sql`]：BEGIN 和四个 `SET LOCAL` 用简单查询协议一次发出去，一次往返
pub(crate) async fn begin(
    client: &Client,
    lock_timeout_ms: i32,
) -> Result<(), tokio_postgres::Error> {
    client.batch_execute(&begin_sql(lock_timeout_ms)).await
}

/// 这些表现在的列，见 [`COLUMNS`]
#[cfg(any(tokio_pg_live_config, tokio_pg_lock_row))]
pub(crate) async fn columns(
    client: &Client,
    tables: Vec<String>,
) -> Result<Columns, tokio_postgres::Error> {
    let rows = client.query(COLUMNS, &[&tables]).await?;
    let rows = rows
        .iter()
        .map(|row: &Row| Ok((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?)))
        .collect::<Result<_, tokio_postgres::Error>>()?;
    Ok(group_columns(rows))
}

/// 错误的 SQLSTATE，比如等锁超时的 55P03
#[cfg(tokio_pg_lock)]
pub(crate) fn sqlstate(error: &tokio_postgres::Error) -> Option<&str> {
    error.code().map(|code| code.code())
}
