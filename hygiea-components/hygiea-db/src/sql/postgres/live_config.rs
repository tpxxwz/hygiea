//! 动态配置的 PostgreSQL SQL：建表（`live_config.sql`）、期望的结构、各条查询，以及和触发器约定的格式。
//! 表名由调用方按前缀拼好传进来；下面的名字都是去掉前缀后的部分

use serde_json::{Map, Value};

/// 建表、函数、触发器的语句：`{p}` 换成前缀，`{channel}` 换成 NOTIFY 的 channel
pub(crate) const DDL: &str = include_str!("live_config.sql");

/// 期望的列：每张表（配置、版本号、历史）按顺序的 (列名, `udt_name`)。改了 `live_config.sql` 的表结构要同时改这里
pub(crate) const COLUMNS: [&[(&str, &str)]; 3] = [
    &[
        ("namespace", "text"),
        ("key", "text"),
        ("value", "jsonb"),
        ("updated_at", "timestamptz"),
    ],
    &[("namespace", "text"), ("revision", "int8")],
    &[
        ("id", "int8"),
        ("namespace", "text"),
        ("key", "text"),
        ("op", "bpchar"),
        ("old_value", "jsonb"),
        ("new_value", "jsonb"),
        ("revision", "int8"),
        ("changed_at", "timestamptz"),
        ("changed_by", "text"),
    ],
];

/// 函数名
pub(crate) const FUNCTIONS: [&str; 3] = ["config_record", "config_on_change", "config_touch"];

/// 触发器名
pub(crate) const TRIGGERS: [&str; 2] = ["config_on_change", "config_touch"];

/// 库里已有的同名函数、触发器（触发器挂在哪张表上都算），参数是函数名数组、触发器名数组
pub(crate) const OBJECTS: &str = "SELECT p.proname::text FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
         WHERE n.nspname = current_schema() AND p.proname = ANY($1) \
         UNION ALL \
         SELECT t.tgname::text FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = current_schema() AND t.tgname = ANY($2) AND NOT t.tgisinternal";

/// NOTIFY 的 channel 名最长多少字节（NAMEDATALEN - 1）
pub(crate) const CHANNEL_MAX_BYTES: usize = 63;

/// 建表时的 schema（表、函数建在这里），参数无。search_path 里没有存在的 schema 时是 NULL
pub(crate) const CURRENT_SCHEMA: &str = "SELECT current_schema()::text";

/// 已有函数的函数体，参数是函数名（`{p}config_record`），用来查它发通知的 channel；没有这个函数时没有行
pub(crate) const FUNCTION_SOURCE: &str = "SELECT p.prosrc FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
     WHERE n.nspname = current_schema() AND p.proname = $1";

/// 读一个 namespace：一条语句把版本号和所有行一起读出来，是同一个快照。所有 key 都被删了时只有一行，key 是 NULL。
/// 参数是 namespace，每行 (版本号, key, 值的 JSON 文本)
pub(crate) fn load(config: &str, revision: &str) -> String {
    format!(
        "SELECT r.revision, c.key, c.value::text FROM {revision} r \
         LEFT JOIN {config} c ON c.namespace = r.namespace WHERE r.namespace = $1"
    )
}

/// 所有 namespace 的版本号，每行 (namespace, 版本号)
pub(crate) fn revisions(revision: &str) -> String {
    format!("SELECT namespace, revision FROM {revision}")
}

/// 表里没有的 key 才插入默认值：一个参数（[`seed_param`]），在库里展开成多行，一条语句补完
pub(crate) fn seed(config: &str) -> String {
    format!(
        "INSERT INTO {config} (namespace, key, value) \
         SELECT n.key, k.key, k.value FROM jsonb_each($1::jsonb) n, jsonb_each(n.value) k \
         ON CONFLICT (namespace, key) DO NOTHING"
    )
}

/// [`seed`] 的参数：`{namespace: {key: value}}`
pub(crate) fn seed_param(defaults: &[(String, Map<String, Value>)]) -> String {
    let root: Map<String, Value> = defaults
        .iter()
        .map(|(namespace, values)| (namespace.clone(), Value::Object(values.clone())))
        .collect();
    Value::Object(root).to_string()
}

/// 变更历史只留最新的 N 行，参数是 N
pub(crate) fn trim_history(history: &str) -> String {
    format!(
        "DELETE FROM {history} WHERE id <= \
         (SELECT id FROM {history} ORDER BY id DESC LIMIT 1 OFFSET $1)"
    )
}

/// 通知的 payload `namespace:revision`（触发器发的），namespace 里可能有 `:`，从右边拆
#[cfg(all(sqlx_live_config, sqlx_pg))]
pub(crate) fn parse_payload(payload: &str) -> Option<(String, i64)> {
    let (namespace, revision) = payload.rsplit_once(':')?;
    Some((namespace.to_string(), revision.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(sqlx_live_config, sqlx_pg))]
    #[test]
    fn payload_splits_from_the_right() {
        assert_eq!(parse_payload("a:b:3"), Some(("a:b".to_string(), 3)));
        assert_eq!(parse_payload("a:x"), None);
        assert_eq!(parse_payload("a"), None);
    }
}
