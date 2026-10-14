//! 动态配置的 MySQL SQL：建表（`live_config.sql`）、期望的结构、各条查询。MySQL 没有 LISTEN / NOTIFY，只靠轮询。
//! 表名由调用方按前缀拼好传进来；下面的名字都是去掉前缀后的部分。`key` 是保留字，要加反引号

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

/// 小池每条连接建好时执行一次（会话级，包括 sqlx 重连时新开的连接），不受库上 DBA 设的超时影响：
/// - `max_execution_time`：不打断读配置（SELECT）
/// - `innodb_lock_wait_timeout`：补默认值要等业务持有的版本号行锁，等到为止（上限 1073741824 秒）
/// - `lock_wait_timeout`：建表时等元数据锁，等到为止（上限一年）
/// - `wait_timeout`：轮询间隔长时连接不因空闲被断开、反复重连
///
/// 普通用户就能 SET，只影响这个小池，主池照旧受 DBA 的超时保护
pub(crate) const SESSION_SETTINGS: &str = "SET SESSION max_execution_time = 0, \
     innodb_lock_wait_timeout = 1073741824, \
     lock_wait_timeout = 31536000, \
     wait_timeout = 31536000";

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

/// 补默认值的语句开头，后面用 [`seed_sql`] 按行数拼 `(?, ?, ?), ..` 和 ON DUPLICATE KEY UPDATE
pub(crate) fn seed(config: &str) -> String {
    format!("INSERT INTO {config} (namespace, `key`, value) VALUES ")
}

/// 一行 3 个参数，一条语句最多绑 65535 个，超过了分几条
pub(crate) const SEED_ROWS: usize = 10_000;

/// 补 `rows` 行默认值的语句：`insert`（[`seed`]）后面拼占位符。已有的 key 走 `ON DUPLICATE KEY UPDATE` 的空操作，
/// 值和 `updated_at` 都不变，触发器看到值没变不记历史。不用 `INSERT IGNORE`：它把别的错误也降成警告，
/// 超长的 key 会被截断后插进去
pub(crate) fn seed_sql(insert: &str, rows: usize) -> String {
    let placeholders = vec!["(?, ?, ?)"; rows].join(", ");
    format!("{insert}{placeholders} ON DUPLICATE KEY UPDATE namespace = namespace")
}

/// 变更历史只留最新的 N 行，参数是 N。MySQL 不能在子查询里直接引用正在删的表（错误 1093），多包一层派生表
pub(crate) fn trim_history(history: &str) -> String {
    format!(
        "DELETE FROM {history} WHERE id <= \
         (SELECT id FROM (SELECT id FROM {history} ORDER BY id DESC LIMIT 1 OFFSET ?) t)"
    )
}

/// 建表时串行化的锁名（`GET_LOCK`，会话级），所有应用共用，只是互相排队建表
pub(crate) fn ddl_lock_name() -> String {
    format!(
        "{}:{}",
        hygiea_core::builtin::LOCK_TYPE,
        hygiea_core::builtin::Lock::LiveConfigDdl as i32
    )
}

/// 拿建表锁，一直等，参数是锁名：拿到 1，出错 NULL
pub(crate) const DDL_LOCK: &str = "SELECT GET_LOCK(?, -1)";

/// 放建表锁，参数是锁名
pub(crate) const DDL_UNLOCK: &str = "DO RELEASE_LOCK(?)";

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

    #[test]
    fn seed_sql_rows() {
        assert_eq!(
            seed_sql(&seed("hy_config"), 2),
            "INSERT INTO hy_config (namespace, `key`, value) VALUES (?, ?, ?), (?, ?, ?) \
             ON DUPLICATE KEY UPDATE namespace = namespace"
        );
    }
}
