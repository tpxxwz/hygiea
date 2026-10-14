//! 锁、动态配置的 toasty 实现共用的执行部分（SQL 和规则在 [`crate::sql`]），不对外。
//! toasty 执行原生 SQL 走预编译，一次只能执行一条；结果是动态的 [`Value`] 行，这里转成 Rust 的类型

use toasty::stmt::Value;

pub(crate) mod postgres;

/// 一行查询结果的各列
pub(crate) fn fields(row: Value) -> toasty::Result<Vec<Value>> {
    match row {
        Value::Record(record) => Ok(record.fields),
        other => Err(toasty::Error::invalid_result(format!(
            "expected a row, got {other:?}"
        ))),
    }
}

/// 可以是 NULL 的文本列
#[cfg(any(toasty_live_config, toasty_lock_row))]
pub(crate) fn opt_string(value: Value) -> toasty::Result<Option<String>> {
    match value {
        Value::String(s) => Ok(Some(s)),
        Value::Null => Ok(None),
        other => Err(toasty::Error::invalid_result(format!(
            "expected text, got {other:?}"
        ))),
    }
}

#[cfg(any(toasty_live_config, toasty_lock_row))]
pub(crate) fn string(value: Value) -> toasty::Result<String> {
    opt_string(value)?.ok_or_else(|| toasty::Error::invalid_result("expected text, got NULL"))
}

#[cfg(toasty_live_config)]
pub(crate) fn i64(value: Value) -> toasty::Result<i64> {
    match value {
        Value::I64(n) => Ok(n),
        Value::I32(n) => Ok(n.into()),
        other => Err(toasty::Error::invalid_result(format!(
            "expected an integer, got {other:?}"
        ))),
    }
}

#[cfg(toasty_lock_pg)]
pub(crate) fn bool(value: Value) -> toasty::Result<bool> {
    match value {
        Value::Bool(b) => Ok(b),
        other => Err(toasty::Error::invalid_result(format!(
            "expected a boolean, got {other:?}"
        ))),
    }
}

/// 只有一行一列的查询结果
#[cfg(toasty_lock_pg)]
pub(crate) fn single(rows: Vec<Value>) -> toasty::Result<Value> {
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| toasty::Error::invalid_result("expected one row, got none"))?;
    fields(row)?
        .into_iter()
        .next()
        .ok_or_else(|| toasty::Error::invalid_result("expected one column, got none"))
}
