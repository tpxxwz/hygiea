//! PostgreSQL 上的执行部分

use sqlx::PgConnection;

use crate::sql::postgres::COLUMNS;
use crate::sql::{Columns, group_columns};

/// 这些表现在的列，见 [`COLUMNS`]
pub(crate) async fn columns(conn: &mut PgConnection, tables: Vec<String>) -> sqlx::Result<Columns> {
    let rows = sqlx::query_as(COLUMNS)
        .bind(tables)
        .fetch_all(&mut *conn)
        .await?;
    Ok(group_columns(rows))
}
