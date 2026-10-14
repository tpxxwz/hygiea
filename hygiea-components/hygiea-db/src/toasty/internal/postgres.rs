//! PostgreSQL 上的执行部分

#[cfg(any(toasty_live_config, toasty_lock_row))]
use toasty::schema::db::Type;
#[cfg(any(toasty_live_config, toasty_lock_row))]
use toasty::stmt::Value;

#[cfg(any(toasty_live_config, toasty_lock_row))]
use super::{fields, string};
use crate::sql::postgres::{BEGIN, SET_TIMEOUTS};
#[cfg(any(toasty_live_config, toasty_lock_row))]
use crate::sql::{Columns, group_columns};

/// 开框架的事务：[`BEGIN`] 加 [`SET_TIMEOUTS`]（toasty 一次只能执行一条，两次往返）。`lock_timeout_ms` 是毫秒，0 是一直等
pub(crate) async fn begin(
    conn: &mut toasty::Connection,
    lock_timeout_ms: i32,
) -> toasty::Result<()> {
    toasty::sql::statement(BEGIN).exec(conn).await?;
    toasty::sql::query(SET_TIMEOUTS)
        .bind(lock_timeout_ms.to_string())
        .exec(conn)
        .await?;
    Ok(())
}

/// 文本数组参数（`= ANY($n)`）
#[cfg(any(toasty_live_config, toasty_lock_row))]
pub(crate) fn text_array(items: Vec<String>) -> (Value, Type) {
    (
        Value::List(items.into_iter().map(Value::from).collect()),
        Type::List(Box::new(Type::Text)),
    )
}

/// 这些表现在的列，见 `sql::postgres::COLUMNS`
#[cfg(any(toasty_live_config, toasty_lock_row))]
pub(crate) async fn columns(
    conn: &mut toasty::Connection,
    tables: Vec<String>,
) -> toasty::Result<Columns> {
    let (names, ty) = text_array(tables);
    let rows = toasty::sql::query(crate::sql::postgres::COLUMNS)
        .bind_typed(names, ty)
        .exec(conn)
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut it = fields(row)?.into_iter();
        let mut next = || {
            it.next()
                .ok_or_else(|| toasty::Error::invalid_result("expected 3 columns"))
                .and_then(string)
        };
        out.push((next()?, next()?, next()?));
    }
    Ok(group_columns(out))
}

/// 错误里的 SQLSTATE（比如等锁超时的 55P03）：toasty 把 tokio-postgres 的错误放在 source 链上
#[cfg(toasty_lock_pg)]
pub(crate) fn sqlstate(error: &toasty::Error) -> Option<String> {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(e) = source {
        if let Some(pg) = e.downcast_ref::<tokio_postgres::Error>() {
            return pg.code().map(|code| code.code().to_string());
        }
        source = e.source();
    }
    None
}
