//! MySQL 上的执行部分

use sqlx::MySqlConnection;

use crate::sql::mysql::columns_sql;
use crate::sql::{Columns, group_columns};

/// 这些表现在的列，见 [`columns_sql`]
pub(crate) async fn columns(
    conn: &mut MySqlConnection,
    tables: Vec<String>,
) -> sqlx::Result<Columns> {
    let sql = columns_sql(tables.len());
    let mut query = sqlx::query_as(sqlx::AssertSqlSafe(sql));
    for table in tables {
        query = query.bind(table);
    }
    Ok(group_columns(query.fetch_all(&mut *conn).await?))
}
