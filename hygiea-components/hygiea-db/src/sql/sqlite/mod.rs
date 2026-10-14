//! SQLite 的 SQL，和驱动无关。各功能的在子模块（[`live_config`]），这里放共用的

/// 一张表的列，参数是表名，每行 (列名, 建表时写的类型名)
pub(crate) const TABLE_INFO: &str = "SELECT name, type FROM pragma_table_info(?1) ORDER BY cid";

pub(crate) mod live_config;
