//! 动态配置的 SQLite SQL：建表（`live_config.sql`）、期望的结构、各条查询。
//! 表名由调用方按前缀拼好传进来；下面的名字都是去掉前缀（应用名加 `_`）后的部分

/// 建表、触发器的语句：`{p}` 换成前缀
pub(crate) const DDL: &str = include_str!("live_config.sql");

/// 期望的列：每张表（配置、版本号、历史）按顺序的 (列名, 建表时写的类型)。改了 `live_config.sql` 的表结构要同时改这里
pub(crate) const COLUMNS: [&[(&str, &str)]; 3] = [
    &[
        ("namespace", "TEXT"),
        ("key", "TEXT"),
        ("value", "TEXT"),
        ("updated_at", "TEXT"),
    ],
    &[("namespace", "TEXT"), ("revision", "INTEGER")],
    &[
        ("id", "INTEGER"),
        ("namespace", "TEXT"),
        ("key", "TEXT"),
        ("op", "TEXT"),
        ("old_value", "TEXT"),
        ("new_value", "TEXT"),
        ("revision", "INTEGER"),
        ("changed_at", "TEXT"),
    ],
];

/// 触发器名
pub(crate) const TRIGGERS: [&str; 4] = [
    "config_insert",
    "config_delete",
    "config_update",
    "config_rename",
];

/// 库里的触发器名，调用方再挑出关心的
pub(crate) const TRIGGER_NAMES: &str = "SELECT name FROM sqlite_master WHERE type = 'trigger'";

/// 读一个 namespace，见 PG 的 `load`。参数是 namespace
pub(crate) fn load(config: &str, revision: &str) -> String {
    format!(
        "SELECT r.revision, c.key, c.value FROM {revision} r \
         LEFT JOIN {config} c ON c.namespace = r.namespace WHERE r.namespace = ?1"
    )
}

/// 所有 namespace 的版本号
pub(crate) fn revisions(revision: &str) -> String {
    format!("SELECT namespace, revision FROM {revision}")
}

/// 补默认值的语句开头，后面用 [`seed_sql`] 按行数拼 `(?, ?, ?), ..` 和 ON CONFLICT：
/// SQLite 的 json_each 展开会丢掉 JSON 的引号、true 变 1，不能像 PG 那样一个参数展开
pub(crate) fn seed(config: &str) -> String {
    format!("INSERT INTO {config} (namespace, key, value) VALUES ")
}

/// 一行 3 个参数，一条语句最多绑 32766 个（3.32 起），超过了分几条
pub(crate) const SEED_ROWS: usize = 10_000;

/// 补 `rows` 行默认值的语句：`insert`（[`seed`]）后面拼占位符和 ON CONFLICT
pub(crate) fn seed_sql(insert: &str, rows: usize) -> String {
    let placeholders = vec!["(?, ?, ?)"; rows].join(", ");
    format!("{insert}{placeholders} ON CONFLICT (namespace, key) DO NOTHING")
}

/// 变更历史只留最新的 N 行，参数是 N
pub(crate) fn trim_history(history: &str) -> String {
    format!(
        "DELETE FROM {history} WHERE id <= \
         (SELECT id FROM {history} ORDER BY id DESC LIMIT 1 OFFSET ?1)"
    )
}
