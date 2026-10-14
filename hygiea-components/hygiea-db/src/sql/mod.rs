//! 框架自己的全部 SQL（锁、动态配置的建表语句、查询、和数据库有关的规则），按数据库分目录（`postgres`、`sqlite`、`mysql`），
//! 每个目录里 `mod.rs` 放功能之间共用的，各功能一个文件（`lock.rs`、`live_config.rs`，`.sql` 建表语句和它放在一起）。
//! 和驱动无关：同一个数据库换驱动（sqlx、tokio-postgres ..）时 SQL、SQLSTATE 的含义不变，直接复用。
//! 执行（取连接、开事务、跑语句、转错误）在各功能的驱动目录里（`lock/sqlx/`、`live_config/sqlx/`），
//! 共用的执行在 `sqlx/internal/`。和数据库也无关的放在这里

#[cfg(any(live_config_any, lock_row_pg))]
use std::collections::HashMap;

#[cfg(any(live_config_mysql, lock_mysql))]
pub(crate) mod mysql;
#[cfg(any(live_config_pg, lock_pg))]
pub(crate) mod postgres;
#[cfg(live_config_sqlite)]
pub(crate) mod sqlite;

/// 框架建的表现在的列：表名 → 按顺序的 (列名, 类型)，不在的表没有这一项
#[cfg(any(live_config_any, lock_row_pg))]
pub(crate) type Columns = HashMap<String, Vec<(String, String)>>;

/// `[a-z][a-z0-9_]*`：应用名拼进表名（表名不能绑参数），只允许这些字符
#[cfg(any(live_config_any, lock_row_pg))]
pub(crate) fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// 框架建的表的名字前缀（`[db.live_config]`、`[db.lock]` 的 `table_prefix`）：不写用应用名（`[registry] app_name`），
/// 写 `""` 不加前缀，写了的末尾补 `_`（写成 `foo_` 也行）。前缀拼进 SQL（表名不能绑参数），去掉末尾的 `_` 后得是
/// 小写下划线 `[a-z][a-z0-9_]*`；经过 Registry 时应用名入口已经校验过，这里再查一次（不经过 Registry 时 RegistryConfig 是自己建的）
#[cfg(any(live_config_any, lock_row_pg))]
pub(crate) fn table_prefix(
    custom: Option<&str>,
    app_name: &str,
    what: &str,
) -> hygiea_core::Result<String> {
    use hygiea_core::app::AppErr;
    use hygiea_core::err;

    let (name, source) = match custom {
        Some("") => return Ok(String::new()),
        Some(prefix) => (prefix.strip_suffix('_').unwrap_or(prefix), "table_prefix"),
        None => (app_name, "app name"),
    };
    if !is_ident(name) {
        return Err(err!(
            AppErr::InvalidConfig,
            format!(
                "{what}: {source} {name:?} is the table prefix, needs snake_case [a-z][a-z0-9_]*"
            )
        ));
    }
    Ok(format!("{name}_"))
}

/// 读表的列的查询结果 (表名, 列名, 类型) 按表收起来
#[cfg(any(live_config_pg, live_config_mysql, lock_row_pg))]
pub(crate) fn group_columns(rows: Vec<(String, String, String)>) -> Columns {
    let mut columns = Columns::new();
    for (table, column, ty) in rows {
        columns.entry(table).or_default().push((column, ty));
    }
    columns
}

#[cfg(all(test, any(live_config_any, lock_row_pg)))]
mod tests {
    use super::*;

    #[test]
    fn ident_rules() {
        for bad in [
            "", "Hy", "hy-test", "1hy", "_hy", "hy;drop", "app.hy", "hy test",
        ] {
            assert!(!is_ident(bad), "应该拒绝 {bad:?}");
        }
        assert!(is_ident("dict_jp2"));
    }

    /// 不写用应用名，`""` 不加前缀，末尾的 `_` 补一个、不重复
    #[test]
    fn table_prefix_rules() {
        assert_eq!(table_prefix(None, "dict_jp", "t").unwrap(), "dict_jp_");
        assert_eq!(table_prefix(Some(""), "dict_jp", "t").unwrap(), "");
        assert_eq!(table_prefix(Some("foo"), "dict_jp", "t").unwrap(), "foo_");
        assert_eq!(table_prefix(Some("foo_"), "dict_jp", "t").unwrap(), "foo_");
        for bad in ["_", "Foo", "foo-bar", "1foo"] {
            assert!(table_prefix(Some(bad), "dict_jp", "t").is_err(), "{bad:?}");
        }
        assert!(table_prefix(None, "dict-jp", "t").is_err());
        assert!(table_prefix(None, "", "t").is_err());
    }
}
