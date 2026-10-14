//! MySQL 的 SQL，和驱动无关。各功能的在子模块（[`lock`]、[`live_config`]），这里放共用的

#[cfg(live_config_mysql)]
pub(crate) mod live_config;
#[cfg(lock_mysql)]
pub(crate) mod lock;

/// 当前库（`DATABASE()`）里这些表的列，每行 (表名, 列名, `DATA_TYPE`)，`DATA_TYPE` 是 `varchar`、`bigint` 这种。
/// MySQL 不能绑数组，`tables` 个表名就拼几个占位符，参数依次是表名
#[cfg(live_config_mysql)]
pub(crate) fn columns_sql(tables: usize) -> String {
    let placeholders = vec!["?"; tables].join(", ");
    format!(
        "SELECT CAST(TABLE_NAME AS CHAR), CAST(COLUMN_NAME AS CHAR), CAST(DATA_TYPE AS CHAR) \
         FROM information_schema.COLUMNS \
         WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME IN ({placeholders}) \
         ORDER BY TABLE_NAME, ORDINAL_POSITION"
    )
}
