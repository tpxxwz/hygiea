//! 动态配置的 MySQL SQL：建表（`live_config.sql`）、期望的结构、各条查询。MySQL 没有 LISTEN / NOTIFY，只靠轮询。
//! 表名由调用方按前缀拼好传进来；下面的名字都是去掉前缀（应用名加 `_`）后的部分。`key` 是保留字，要加反引号

/// 建表、触发器的语句：`{p}` 换成前缀。一次只能执行一条（`CREATE TRIGGER` 里有分号），按 [`DDL_SEPARATOR`] 拆开
pub(crate) const DDL: &str = include_str!("live_config.sql");

/// [`DDL`] 里语句之间的分隔行
pub(crate) const DDL_SEPARATOR: &str = "\n-- @@\n";

/// 期望的列：每张表（配置、版本号、历史）按顺序的 (列名, `DATA_TYPE`)。改了 `live_config.sql` 的表结构要同时改这里
pub(crate) const COLUMNS: [&[(&str, &str)]; 3] = [
    &[
        ("namespace", "varchar"),
        ("key", "varchar"),
        ("value", "json"),
        ("updated_at", "datetime"),
    ],
    &[("namespace", "varchar"), ("revision", "bigint")],
    &[
        ("id", "bigint"),
        ("namespace", "varchar"),
        ("key", "varchar"),
        ("op", "char"),
        ("old_value", "json"),
        ("new_value", "json"),
        ("revision", "bigint"),
        ("changed_at", "datetime"),
        ("changed_by", "varchar"),
    ],
];

/// 触发器名
pub(crate) const TRIGGERS: [&str; 3] = ["config_insert", "config_delete", "config_update"];

/// 当前库里的触发器名，调用方再挑出关心的
pub(crate) const TRIGGER_NAMES: &str = "SELECT CAST(TRIGGER_NAME AS CHAR) \
     FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = DATABASE()";

/// 读一个 namespace，见 PG 的 `load`。参数是 namespace；JSON 列转成文本读
pub(crate) fn load(config: &str, revision: &str) -> String {
    format!(
        "SELECT r.revision, c.`key`, CAST(c.value AS CHAR) FROM {revision} r \
         LEFT JOIN {config} c ON c.namespace = r.namespace WHERE r.namespace = ?"
    )
}

/// 所有 namespace 的版本号
pub(crate) fn revisions(revision: &str) -> String {
    format!("SELECT namespace, revision FROM {revision}")
}

/// 补默认值的语句开头，后面用 [`seed_sql`] 按行数拼 `(?, ?, ?), ..`
pub(crate) fn seed(config: &str) -> String {
    format!("INSERT IGNORE INTO {config} (namespace, `key`, value) VALUES ")
}

/// 一行 3 个参数，一条语句最多绑 65535 个，超过了分几条
pub(crate) const SEED_ROWS: usize = 10_000;

/// 补 `rows` 行默认值的语句：`insert`（[`seed`]）后面拼占位符。已有的 key 被 `INSERT IGNORE` 跳过
pub(crate) fn seed_sql(insert: &str, rows: usize) -> String {
    let placeholders = vec!["(?, ?, ?)"; rows].join(", ");
    format!("{insert}{placeholders}")
}

/// 变更历史只留最新的 N 行，参数是 N。MySQL 不能在子查询里直接引用正在删的表（错误 1093），多包一层派生表
pub(crate) fn trim_history(history: &str) -> String {
    format!(
        "DELETE FROM {history} WHERE id <= \
         (SELECT id FROM (SELECT id FROM {history} ORDER BY id DESC LIMIT 1 OFFSET ?) t)"
    )
}

/// 建表时串行化的锁名（`GET_LOCK`），所有应用共用，只是互相排队建表
pub(crate) fn ddl_lock_name() -> String {
    format!(
        "{}:{}",
        hygiea_core::builtin::LOCK_TYPE,
        hygiea_core::builtin::Lock::LiveConfigDdl as i32
    )
}

/// 拿建表锁，一直等，参数是锁名
pub(crate) const DDL_LOCK: &str = "SELECT GET_LOCK(?, -1)";

/// 放建表锁，参数是锁名
pub(crate) const DDL_UNLOCK: &str = "SELECT RELEASE_LOCK(?)";

#[cfg(test)]
mod tests {
    use super::*;

    /// 拆出 3 张表、3 个触发器，每条都完整
    #[test]
    fn ddl_splits_into_statements() {
        let statements: Vec<&str> = DDL.split(DDL_SEPARATOR).collect();
        assert_eq!(statements.len(), 6);
        assert!(statements[3].contains("CREATE TRIGGER {p}config_insert"));
        assert!(statements[5].trim_end().ends_with("END"));
    }
}
