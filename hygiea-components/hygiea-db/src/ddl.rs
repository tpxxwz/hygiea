//! 框架自己建的表（动态配置、锁表）共用的检查代码：表名前缀的校验、建表时串行化用的 advisory key、读表的列

#[cfg(feature = "postgres")]
use std::collections::HashMap;

#[cfg(feature = "postgres")]
use sqlx::PgConnection;

/// 表现在的列：表名 → 按顺序的 (列名, 类型)，不在的表没有这一项
#[cfg(feature = "postgres")]
pub(crate) type Columns = HashMap<String, Vec<(String, String)>>;

/// `[a-z][a-z0-9_]*`：应用名拼进表名（表名不能绑参数），只允许这些字符
pub(crate) fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// 建表时串行化用的 advisory lock 的 key：按名字算的 FNV-1a，同一个名字的实例互斥，不同的互不影响
#[cfg(feature = "postgres")]
pub(crate) fn advisory_key(name: &str) -> i64 {
    let hash = name.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    hash as i64
}

/// `current_schema()` 里这些表的列，类型是 `udt_name`（`int8`、`bpchar` 这种）
#[cfg(feature = "postgres")]
pub(crate) async fn postgres_columns(
    conn: &mut PgConnection,
    tables: Vec<String>,
) -> sqlx::Result<Columns> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT table_name::text, column_name::text, udt_name::text FROM information_schema.columns \
         WHERE table_schema = current_schema() AND table_name = ANY($1) \
         ORDER BY table_name, ordinal_position",
    )
    .bind(tables)
    .fetch_all(&mut *conn)
    .await?;
    let mut columns = Columns::new();
    for (table, column, ty) in rows {
        columns.entry(table).or_default().push((column, ty));
    }
    Ok(columns)
}

#[cfg(test)]
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
}
