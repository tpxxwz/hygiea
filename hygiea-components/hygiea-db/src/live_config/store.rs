//! 动态配置用的连接和在上面做的事，和后端、框架有关的部分都在这里，worker 和 `mod.rs` 不碰 sqlx。
//!
//! [`Store`] 持有一个独立小池：连接参数和主池一样（库、search_path、SQLite 的打开方式），池参数固定——
//! 最多 1 条、不空闲回收、不到期替换、拿之前先检查，只有 acquire 超时跟主池走。不占主池的名额，
//! 断线重连、超时交给 sqlx。建的时候不连，第一次 [`Store::open`] 时才连。
//! 在小池上检查表、读写配置表、等变更信号，错误都转成 [`RepoErr`]。
//! 现在只有 sqlx 的实现，接别的框架时这里改成 trait、每个框架一个实现。
//!
//! # 接别的框架（非 sqlx）
//!
//! 1. 把 [`Store`] 上这几个方法提成 trait：`backend`、`table`、`is_open`、`open`、`reset`、`load`、`revisions`、
//!    `seed`、`recv`、`close`。参数、返回值里已经没有 sqlx 的类型，现在的 PG、SQLite 两支变成 sqlx 的实现
//! 2. worker 改成持有 `Box<dyn ..>`：原生 `async fn` 写在 trait 里不能 `dyn`，用 `async_trait`（hygiea-core 重新导出了），
//!    或者 worker 写成泛型 `Worker<S>`
//! 3. `schema.rs` 里检查、建表的查询（`postgres_columns` 那几个）跟着搬进新框架的实现；表名、期望的列、
//!    `sql/*.sql` 直接复用
//! 4. `mod.rs`、`worker.rs` 不用改
//!
//! # 改这里时要注意
//!
//! - `recv` 要能被取消：worker 在 `select!` 里等它，定时器到了会把它丢掉。sqlx 收消息读完整条才动缓冲区，
//!   换别的库要确认同样能取消
//! - PG 的 listener 报错后一定要 `reset` 丢掉：sqlx 只对部分 IO 错误自己重连，别的错误（比如 connection reset）
//!   会一直留着坏连接，每次调都报同样的错
//! - 小池最多 1 条连接，listener 一直占着它：PG 的查询都要走 listener（`&mut **listener`），不能走 `&pool`，
//!   否则拿不到连接，等到 acquire 超时

use std::time::Duration;

use serde_json::{Map, Value};
use sqlx::AssertSqlSafe;
use sqlx::pool::PoolOptions;
#[cfg(feature = "postgres")]
use sqlx::postgres::{PgConnectOptions, PgListener, PgPool};
#[cfg(feature = "sqlite")]
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};

use hygiea_core::{Result, ResultExt, err};

use super::Backend;
use super::schema::{self, Tables};
use crate::{PoolConfig, RepoErr};

/// 加载时的一行：版本号、key、值（JSON 文本）。所有 key 都被删了时 key 和值是 NULL
type Row = (i64, Option<String>, Option<String>);

/// 一个 namespace 在库里的样子：版本号和各 key 的值
pub(crate) struct Loaded {
    pub(crate) revision: i64,
    pub(crate) values: Map<String, Value>,
}

/// 变更信号
pub(crate) enum Signal {
    /// PG 的通知：哪个 namespace 变到了哪个版本
    #[cfg_attr(not(feature = "postgres"), allow(dead_code))]
    Changed { namespace: String, revision: i64 },
    /// 连接断了又自动连上，断线期间的通知丢了
    #[cfg_attr(not(feature = "postgres"), allow(dead_code))]
    Reconnected,
}

/// 独立小池，加上在它上面做的事
pub(crate) struct Store {
    tables: Tables,
    sql: Sql,
    conn: Conn,
}

/// 和后端有关的连接部分
enum Conn {
    /// listener 占着小池里唯一那条连接：监听、加载、轮询都走它，断了 sqlx 自动重连、重新 LISTEN。
    /// `None` 是还没建，或者报错后丢掉了，[`Store::open`] 在同一个小池上重建
    #[cfg(feature = "postgres")]
    Postgres {
        pool: PgPool,
        listener: Option<Box<PgListener>>,
    },
    /// SQLite 没有推送，直接在小池上查，断了 sqlx 下次拿连接时重连
    #[cfg(feature = "sqlite")]
    Sqlite(SqlitePool),
}

/// 渲染好的查询，建 [`Store`] 时拼一次
struct Sql {
    load: String,
    revisions: String,
    seed: String,
    /// 变更历史只留最新的 N 行，参数是 N
    trim_history: String,
}

impl Sql {
    fn new(tables: &Tables, backend: Backend) -> Self {
        let (config, revision) = (tables.config(), tables.revision());
        // 一条语句把版本号和所有行一起读出来，是同一个快照。所有 key 都被删了时只有一行，key 是 NULL
        let load = |param: &str, value: &str| {
            format!(
                "SELECT r.revision, c.key, {value} FROM {revision} r \
                 LEFT JOIN {config} c ON c.namespace = r.namespace WHERE r.namespace = {param}"
            )
        };
        let revisions = format!("SELECT namespace, revision FROM {revision}");
        let history = tables.history();
        let trim_history = |param: &str| {
            format!(
                "DELETE FROM {history} WHERE id <= \
                 (SELECT id FROM {history} ORDER BY id DESC LIMIT 1 OFFSET {param})"
            )
        };
        match backend {
            #[cfg(feature = "postgres")]
            Backend::Postgres => Self {
                load: load("$1", "c.value::text"),
                revisions,
                // 一个参数 {namespace: {key: value}}，在库里展开成多行，一条语句补完
                seed: format!(
                    "INSERT INTO {config} (namespace, key, value) \
                     SELECT n.key, k.key, k.value FROM jsonb_each($1::jsonb) n, jsonb_each(n.value) k \
                     ON CONFLICT (namespace, key) DO NOTHING"
                ),
                trim_history: trim_history("$1"),
            },
            #[cfg(feature = "sqlite")]
            Backend::Sqlite => Self {
                load: load("?1", "c.value"),
                revisions,
                // 后面按行数拼 `(?, ?, ?), ..` 和 ON CONFLICT：SQLite 的 json_each 展开会丢掉 JSON 的引号、true 变 1
                seed: format!("INSERT INTO {config} (namespace, key, value) VALUES "),
                trim_history: trim_history("?1"),
            },
        }
    }
}

impl Store {
    /// `options` 是主池的连接参数。主池有 `application_name`（应用名）时，小池用 `<应用名>-live-config`，
    /// 带到小池的每条连接上（包括 sqlx 断线重连时新开的），在 `pg_stat_activity` 里认得出来
    #[cfg(feature = "postgres")]
    pub(crate) fn postgres(options: PgConnectOptions, main: &PoolConfig, tables: Tables) -> Self {
        let options = match options
            .get_application_name()
            .map(|name| format!("{name}-live-config"))
        {
            Some(name) => options.application_name(&name),
            None => options,
        };
        Self {
            sql: Sql::new(&tables, Backend::Postgres),
            tables,
            conn: Conn::Postgres {
                pool: fixed(main).connect_lazy_with(options),
                listener: None,
            },
        }
    }

    #[cfg(feature = "sqlite")]
    pub(crate) fn sqlite(options: SqliteConnectOptions, main: &PoolConfig, tables: Tables) -> Self {
        Self {
            sql: Sql::new(&tables, Backend::Sqlite),
            tables,
            conn: Conn::Sqlite(fixed(main).connect_lazy_with(options)),
        }
    }

    pub(crate) fn backend(&self) -> Backend {
        match self.conn {
            #[cfg(feature = "postgres")]
            Conn::Postgres { .. } => Backend::Postgres,
            #[cfg(feature = "sqlite")]
            Conn::Sqlite(_) => Backend::Sqlite,
        }
    }

    /// 配置表名，日志用
    pub(crate) fn table(&self) -> String {
        self.tables.config()
    }

    /// 能不能用：PG 要建好 listener，SQLite 一直能用
    pub(crate) fn is_open(&self) -> bool {
        match &self.conn {
            #[cfg(feature = "postgres")]
            Conn::Postgres { listener, .. } => listener.is_some(),
            #[cfg(feature = "sqlite")]
            Conn::Sqlite(_) => true,
        }
    }

    /// 连上：PG 在小池上建 listener、LISTEN；`ensure_tables` 时检查表和函数、触发器（见 [`schema`]）
    pub(crate) async fn open(&mut self, ensure_tables: bool) -> Result<()> {
        let setup_err = || err!(RepoErr::ExecFailed, "live config setup");
        match &mut self.conn {
            #[cfg(feature = "postgres")]
            Conn::Postgres { pool, listener } => {
                let mut opened = PgListener::connect_with(pool).await.wrap_err(setup_err)?;
                if ensure_tables {
                    let conn = sqlx::Acquire::acquire(&mut opened)
                        .await
                        .wrap_err(setup_err)?;
                    schema::ensure_postgres(conn, &self.tables).await?;
                }
                opened
                    .listen(&self.tables.channel())
                    .await
                    .wrap_err(|| err!(RepoErr::ExecFailed, "live config listen"))?;
                *listener = Some(Box::new(opened));
            }
            #[cfg(feature = "sqlite")]
            Conn::Sqlite(pool) => {
                if ensure_tables {
                    let mut conn = pool.acquire().await.wrap_err(setup_err)?;
                    schema::ensure_sqlite(&mut conn, &self.tables).await?;
                }
            }
        }
        Ok(())
    }

    /// 查询、收通知报错后调用。PG 丢掉 listener，下次 [`Store::open`] 重建：sqlx 只对部分 IO 错误自己重连，
    /// 别的错误会一直留着坏连接。SQLite 什么都不用做，返回 false
    pub(crate) fn reset(&mut self) -> bool {
        match &mut self.conn {
            #[cfg(feature = "postgres")]
            Conn::Postgres { listener, .. } => listener.take().is_some(),
            #[cfg(feature = "sqlite")]
            Conn::Sqlite(_) => false,
        }
    }

    /// 读一个 namespace。还没有 revision 的（表里从来没有过它的行）是版本 0、没有值
    pub(crate) async fn load(&mut self, namespace: &str) -> Result<Loaded> {
        let query = AssertSqlSafe(self.sql.load.as_str());
        let rows: sqlx::Result<Vec<Row>> = match &mut self.conn {
            #[cfg(feature = "postgres")]
            Conn::Postgres { listener, .. } => match opened(listener) {
                Ok(listener) => {
                    sqlx::query_as(query)
                        .bind(namespace)
                        .fetch_all(&mut **listener)
                        .await
                }
                Err(e) => Err(e),
            },
            #[cfg(feature = "sqlite")]
            Conn::Sqlite(pool) => {
                sqlx::query_as(query)
                    .bind(namespace)
                    .fetch_all(&*pool)
                    .await
            }
        };
        let rows = rows.wrap_err(|| {
            err!(
                RepoErr::QueryFailed,
                format!("live config load {namespace}")
            )
        })?;
        let mut loaded = Loaded {
            revision: 0,
            values: Map::new(),
        };
        for (revision, key, value) in rows {
            loaded.revision = revision;
            if let (Some(key), Some(value)) = (key, value) {
                // 两个库都校验过是合法 JSON（PG 的 jsonb、SQLite 的 CHECK），解析不了按字符串处理
                let value = serde_json::from_str(&value).unwrap_or(Value::String(value));
                loaded.values.insert(key, value);
            }
        }
        Ok(loaded)
    }

    /// 所有 namespace 的版本号
    pub(crate) async fn revisions(&mut self) -> Result<Vec<(String, i64)>> {
        let query = AssertSqlSafe(self.sql.revisions.as_str());
        let rows = match &mut self.conn {
            #[cfg(feature = "postgres")]
            Conn::Postgres { listener, .. } => match opened(listener) {
                Ok(listener) => sqlx::query_as(query).fetch_all(&mut **listener).await,
                Err(e) => Err(e),
            },
            #[cfg(feature = "sqlite")]
            Conn::Sqlite(pool) => sqlx::query_as(query).fetch_all(&*pool).await,
        };
        rows.wrap_err(|| err!(RepoErr::QueryFailed, "live config revisions"))
    }

    /// 表里没有的 key 才插入默认值，有的不动。`defaults` 是各 namespace 的默认值（第一层是 key），一条语句补完
    pub(crate) async fn seed(&mut self, defaults: &[(String, Map<String, Value>)]) -> Result<()> {
        if defaults.iter().all(|(_, values)| values.is_empty()) {
            return Ok(());
        }
        let done = match &mut self.conn {
            #[cfg(feature = "postgres")]
            Conn::Postgres { listener, .. } => {
                let root: Map<String, Value> = defaults
                    .iter()
                    .map(|(namespace, values)| (namespace.clone(), Value::Object(values.clone())))
                    .collect();
                match opened(listener) {
                    Ok(listener) => sqlx::query(AssertSqlSafe(self.sql.seed.as_str()))
                        .bind(Value::Object(root).to_string())
                        .execute(&mut **listener)
                        .await
                        .map(|_| ()),
                    Err(e) => Err(e),
                }
            }
            #[cfg(feature = "sqlite")]
            Conn::Sqlite(pool) => seed_sqlite(pool, &self.sql.seed, defaults).await,
        };
        done.wrap_err(|| err!(RepoErr::InsertFailed, "live config seed defaults"))
    }

    /// 变更历史表只留最新的 `keep_rows` 行，返回删了几行
    pub(crate) async fn trim_history(&mut self, keep_rows: u64) -> Result<u64> {
        let query = AssertSqlSafe(self.sql.trim_history.as_str());
        // 配置里的数字很小，转不过去就用最大值（等于不删）
        let keep_rows = i64::try_from(keep_rows).unwrap_or(i64::MAX);
        let done = match &mut self.conn {
            #[cfg(feature = "postgres")]
            Conn::Postgres { listener, .. } => match opened(listener) {
                Ok(listener) => sqlx::query(query)
                    .bind(keep_rows)
                    .execute(&mut **listener)
                    .await
                    .map(|r| r.rows_affected()),
                Err(e) => Err(e),
            },
            #[cfg(feature = "sqlite")]
            Conn::Sqlite(pool) => sqlx::query(query)
                .bind(keep_rows)
                .execute(&*pool)
                .await
                .map(|r| r.rows_affected()),
        };
        done.wrap_err(|| err!(RepoErr::DeleteFailed, "live config trim history"))
    }

    /// 等下一个变更信号。SQLite 没有推送，一直等不到，只靠轮询；PG 还没建好 listener 时也一直等。
    /// 可以取消：sqlx 收消息时读完整条才动缓冲区（`PgStream::recv_unchecked`）
    pub(crate) async fn recv(&mut self) -> Result<Signal> {
        match &mut self.conn {
            #[cfg(feature = "postgres")]
            Conn::Postgres {
                listener: Some(listener),
                ..
            } => {
                loop {
                    let received = listener
                        .try_recv()
                        .await
                        .wrap_err(|| err!(RepoErr::ExecFailed, "live config receive"))?;
                    let Some(notification) = received else {
                        return Ok(Signal::Reconnected);
                    };
                    // payload 是 `namespace:revision`，namespace 里可能有 `:`，从右边拆
                    let parsed = notification.payload().rsplit_once(':').and_then(
                        |(namespace, revision)| {
                            Some((namespace.to_string(), revision.parse().ok()?))
                        },
                    );
                    match parsed {
                        Some((namespace, revision)) => {
                            return Ok(Signal::Changed {
                                namespace,
                                revision,
                            });
                        }
                        None => tracing::warn!(
                            payload = notification.payload(),
                            "live config: ignored malformed notification"
                        ),
                    }
                }
            }
            _ => std::future::pending().await,
        }
    }

    /// 放掉 listener，等借出去的连接还回来后关掉小池
    pub(crate) async fn close(&mut self) {
        match &mut self.conn {
            #[cfg(feature = "postgres")]
            Conn::Postgres { pool, listener } => {
                listener.take();
                pool.close().await;
            }
            #[cfg(feature = "sqlite")]
            Conn::Sqlite(pool) => pool.close().await,
        }
    }
}

/// SQLite 一行 3 个参数，一条语句最多绑 32766 个（3.32 起），超过了分几条
#[cfg(feature = "sqlite")]
const SQLITE_SEED_ROWS: usize = 10_000;

/// SQLite 补默认值：拼 `VALUES (?, ?, ?), ..`，在一个事务里执行（超过 [`SQLITE_SEED_ROWS`] 行分几条）
#[cfg(feature = "sqlite")]
async fn seed_sqlite(
    pool: &SqlitePool,
    insert: &str,
    defaults: &[(String, Map<String, Value>)],
) -> sqlx::Result<()> {
    let rows: Vec<(&str, &str, String)> = defaults
        .iter()
        .flat_map(|(namespace, values)| {
            values
                .iter()
                .map(move |(key, value)| (namespace.as_str(), key.as_str(), value.to_string()))
        })
        .collect();
    let mut tx = pool.begin().await?;
    for chunk in rows.chunks(SQLITE_SEED_ROWS) {
        let placeholders = vec!["(?, ?, ?)"; chunk.len()].join(", ");
        let sql = format!("{insert}{placeholders} ON CONFLICT (namespace, key) DO NOTHING");
        let mut query = sqlx::query(AssertSqlSafe(sql));
        for (namespace, key, value) in chunk {
            query = query.bind(*namespace).bind(*key).bind(value.as_str());
        }
        query.execute(&mut *tx).await?;
    }
    tx.commit().await
}

/// 建好的 listener；没建（还没 open，或者报错后丢掉了）时报错，worker 会先重建再用
#[cfg(feature = "postgres")]
fn opened(listener: &mut Option<Box<PgListener>>) -> sqlx::Result<&mut Box<PgListener>> {
    listener.as_mut().ok_or(sqlx::Error::PoolClosed)
}

/// 固定的池参数，只有 acquire 超时跟主池走
fn fixed<DB: sqlx::Database>(main: &PoolConfig) -> PoolOptions<DB> {
    PoolOptions::new()
        .max_connections(1)
        .min_connections(0)
        .acquire_timeout(Duration::from_secs(main.acquire_timeout_secs()))
        .idle_timeout(None)
        .max_lifetime(None)
        .test_before_acquire(true)
}
