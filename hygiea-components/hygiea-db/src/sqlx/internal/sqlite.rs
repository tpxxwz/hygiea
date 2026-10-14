//! SQLite 上的执行部分

use sqlx::SqliteConnection;

use crate::sql::Columns;
use crate::sql::sqlite::TABLE_INFO;

/// 这些表现在的列，见 [`TABLE_INFO`]
pub(crate) async fn columns(
    conn: &mut SqliteConnection,
    tables: Vec<String>,
) -> sqlx::Result<Columns> {
    let mut columns = Columns::new();
    for table in tables {
        let rows: Vec<(String, String)> = sqlx::query_as(TABLE_INFO)
            .bind(&table)
            .fetch_all(&mut *conn)
            .await?;
        if !rows.is_empty() {
            columns.insert(table, rows);
        }
    }
    Ok(columns)
}
