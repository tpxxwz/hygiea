//! 动态配置的 sqlx 实现，一种数据库一个文件（`postgres.rs`、`sqlite.rs`、`mysql.rs`），这里的 [`SqlxStore`] 只做分发和转错误；
//! worker 和上层的 `mod.rs` 不碰 sqlx。SQL 在 `sql/<数据库>/live_config.*`。sqlx、SeaORM 的连接池组件共用
//! （小池按组件配置里的 `[db.connect]` 直接用 sqlx 建，和组件用哪个框架无关）
//!
//! [`Store`] 持有一个独立小池：连接参数和主池一样（库、search_path、SQLite 的打开方式），池参数固定——
//! 最多 1 条、不空闲回收、不到期替换、拿之前先检查，只有 acquire 超时跟主池走。不占主池的名额，
//! 断线重连、超时交给 sqlx。建的时候不连，第一次 [`SqlxStore::open`] 时才连。
//! 在小池上检查表、读写配置表、等变更信号，错误都转成 [`RepoErr`]。
//!
//!
//! # 接别的框架（非 sqlx）
//!
//! 1. 把 [`Store`] 上这几个方法提成 trait：`backend`、`table`、`is_open`、`open`、`load`、`revisions`、
//!    `seed`、`recv`、`close`。参数、返回值里已经没有 sqlx 的类型，现在这个 `Store` 变成 sqlx 的实现
//! 2. worker 改成持有 `Box<dyn ..>`：原生 `async fn` 写在 trait 里不能 `dyn`，用 `async_trait`（hygiea-core 重新导出了），
//!    或者 worker 写成泛型 `Worker<S>`
//! 3. 各数据库的执行（这个目录下的文件）新框架另写一份；表名、期望的列、状态判断（`schema.rs`）、
//!    SQL（`sql/<数据库>/live_config.*`）直接复用
//! 4. `mod.rs`、`worker.rs` 不用改
//!
//! # 改这里时要注意
//!
//! - `recv` 要能被取消：worker 在 `select!` 里等它，定时器到了会把它丢掉。sqlx 收消息读完整条才动缓冲区，
//!   换别的库要确认同样能取消
//! - PG 的连接坏了（`postgres.rs` 的 `connection_broken`）一定要丢掉 listener：sqlx 只对部分 IO 错误自己重连，别的错误（比如 connection reset）
//!   会一直留着坏连接，每次调都报同样的错
//! - 小池最多 1 条连接，listener 一直占着它：PG 的查询都要走 listener（`&mut **listener`），不能走 `&pool`，
//!   否则拿不到连接，等到 acquire 超时

use std::time::Duration;

use serde_json::{Map, Value};
#[cfg(sqlx_mysql)]
use sqlx::mysql::MySqlConnectOptions;
use sqlx::pool::PoolOptions;
#[cfg(sqlx_pg)]
use sqlx::postgres::PgConnectOptions;
#[cfg(sqlx_sqlite)]
use sqlx::sqlite::SqliteConnectOptions;

use hygiea_core::{Result, ResultExt, err};

use super::Backend;
use super::schema::{Sql, Tables};
use super::worker::{Loaded, Signal};
use crate::{PoolConfig, RepoErr};

#[cfg(sqlx_mysql)]
mod mysql;
#[cfg(sqlx_pg)]
mod postgres;
#[cfg(sqlx_sqlite)]
mod sqlite;

/// 独立小池，加上在它上面做的事
pub(crate) struct SqlxStore {
    tables: Tables,
    sql: Sql,
    conn: Conn,
}

/// 各数据库的连接部分
enum Conn {
    #[cfg(sqlx_pg)]
    Postgres(postgres::PgConn),
    #[cfg(sqlx_sqlite)]
    Sqlite(sqlite::SqliteConn),
    #[cfg(sqlx_mysql)]
    MySql(mysql::MySqlConn),
}

impl SqlxStore {
    fn new(tables: Tables, backend: Backend, conn: Conn) -> Self {
        Self {
            sql: Sql::new(&tables, backend),
            tables,
            conn,
        }
    }

    /// `options` 是主池的连接参数
    #[cfg(sqlx_pg)]
    pub(crate) fn postgres(options: PgConnectOptions, main: &PoolConfig, tables: Tables) -> Self {
        let conn = Conn::Postgres(postgres::PgConn::new(options, main));
        Self::new(tables, Backend::Postgres, conn)
    }

    #[cfg(sqlx_sqlite)]
    pub(crate) fn sqlite(options: SqliteConnectOptions, main: &PoolConfig, tables: Tables) -> Self {
        let conn = Conn::Sqlite(sqlite::SqliteConn::new(options, main));
        Self::new(tables, Backend::Sqlite, conn)
    }

    #[cfg(sqlx_mysql)]
    pub(crate) fn mysql(options: MySqlConnectOptions, main: &PoolConfig, tables: Tables) -> Self {
        let conn = Conn::MySql(mysql::MySqlConn::new(options, main));
        Self::new(tables, Backend::MySql, conn)
    }

    pub(crate) fn backend(&self) -> Backend {
        match self.conn {
            #[cfg(sqlx_pg)]
            Conn::Postgres(_) => Backend::Postgres,
            #[cfg(sqlx_sqlite)]
            Conn::Sqlite(_) => Backend::Sqlite,
            #[cfg(sqlx_mysql)]
            Conn::MySql(_) => Backend::MySql,
        }
    }

    /// 有没有推送：只有 PG 有（LISTEN）
    pub(crate) fn has_push(&self) -> bool {
        match self.conn {
            #[cfg(sqlx_pg)]
            Conn::Postgres(_) => true,
            #[allow(unreachable_patterns)]
            _ => false,
        }
    }

    /// 配置表名，日志用
    pub(crate) fn table(&self) -> String {
        self.tables.config()
    }

    /// 能不能用：PG 要建好 listener，SQLite、MySQL 一直能用
    pub(crate) fn is_open(&self) -> bool {
        match &self.conn {
            #[cfg(sqlx_pg)]
            Conn::Postgres(conn) => conn.is_open(),
            #[cfg(sqlx_sqlite)]
            Conn::Sqlite(_) => true,
            #[cfg(sqlx_mysql)]
            Conn::MySql(_) => true,
        }
    }

    /// 连上：PG 在小池上建 listener、LISTEN；`ensure_tables` 时检查表和函数、触发器（规则见上层的 `schema.rs`）
    pub(crate) async fn open(&mut self, ensure_tables: bool) -> Result<()> {
        match &mut self.conn {
            #[cfg(sqlx_pg)]
            Conn::Postgres(conn) => conn.open(&self.tables, ensure_tables).await,
            #[cfg(sqlx_sqlite)]
            Conn::Sqlite(conn) => conn.open(&self.tables, ensure_tables).await,
            #[cfg(sqlx_mysql)]
            Conn::MySql(conn) => conn.open(&self.tables, ensure_tables).await,
        }
    }

    /// 读一个 namespace。还没有 revision 的（表里从来没有过它的行）是版本 0、没有值
    pub(crate) async fn load(&mut self, namespace: &str) -> Result<Loaded> {
        let sql = &self.sql.load;
        let rows = match &mut self.conn {
            #[cfg(sqlx_pg)]
            Conn::Postgres(conn) => conn.load(sql, namespace).await,
            #[cfg(sqlx_sqlite)]
            Conn::Sqlite(conn) => conn.load(sql, namespace).await,
            #[cfg(sqlx_mysql)]
            Conn::MySql(conn) => conn.load(sql, namespace).await,
        };
        let rows = rows.wrap_err(|| {
            err!(
                RepoErr::QueryFailed,
                format!("live config load {namespace}")
            )
        })?;
        Ok(Loaded::from_rows(rows))
    }

    /// 所有 namespace 的版本号
    pub(crate) async fn revisions(&mut self) -> Result<Vec<(String, i64)>> {
        let sql = &self.sql.revisions;
        let rows = match &mut self.conn {
            #[cfg(sqlx_pg)]
            Conn::Postgres(conn) => conn.revisions(sql).await,
            #[cfg(sqlx_sqlite)]
            Conn::Sqlite(conn) => conn.revisions(sql).await,
            #[cfg(sqlx_mysql)]
            Conn::MySql(conn) => conn.revisions(sql).await,
        };
        rows.wrap_err(|| err!(RepoErr::QueryFailed, "live config revisions"))
    }

    /// 表里没有的 key 才插入默认值，有的不动。`defaults` 是各 namespace 的默认值（第一层是 key）
    pub(crate) async fn seed(&mut self, defaults: &[(String, Map<String, Value>)]) -> Result<()> {
        if defaults.iter().all(|(_, values)| values.is_empty()) {
            return Ok(());
        }
        let sql = &self.sql.seed;
        let done = match &mut self.conn {
            #[cfg(sqlx_pg)]
            Conn::Postgres(conn) => conn.seed(sql, defaults).await,
            #[cfg(sqlx_sqlite)]
            Conn::Sqlite(conn) => conn.seed(sql, defaults).await,
            #[cfg(sqlx_mysql)]
            Conn::MySql(conn) => conn.seed(sql, defaults).await,
        };
        done.wrap_err(|| err!(RepoErr::InsertFailed, "live config seed defaults"))
    }

    /// 变更历史表只留最新的 `keep_rows` 行，返回删了几行
    pub(crate) async fn trim_history(&mut self, keep_rows: u64) -> Result<u64> {
        let sql = &self.sql.trim_history;
        // 配置里的数字很小，转不过去就用最大值（等于不删）
        let keep_rows = i64::try_from(keep_rows).unwrap_or(i64::MAX);
        let done = match &mut self.conn {
            #[cfg(sqlx_pg)]
            Conn::Postgres(conn) => conn.trim_history(sql, keep_rows).await,
            #[cfg(sqlx_sqlite)]
            Conn::Sqlite(conn) => conn.trim_history(sql, keep_rows).await,
            #[cfg(sqlx_mysql)]
            Conn::MySql(conn) => conn.trim_history(sql, keep_rows).await,
        };
        done.wrap_err(|| err!(RepoErr::DeleteFailed, "live config trim history"))
    }

    /// 等下一个变更信号。只有 PG 有推送；SQLite、MySQL 一直等不到，只靠轮询
    pub(crate) async fn recv(&mut self) -> Result<Signal> {
        match &mut self.conn {
            #[cfg(sqlx_pg)]
            Conn::Postgres(conn) => conn.recv().await,
            #[allow(unreachable_patterns)]
            _ => std::future::pending().await,
        }
    }

    /// 关掉小池
    pub(crate) async fn close(&mut self) {
        match &mut self.conn {
            #[cfg(sqlx_pg)]
            Conn::Postgres(conn) => conn.close().await,
            #[cfg(sqlx_sqlite)]
            Conn::Sqlite(conn) => conn.close().await,
            #[cfg(sqlx_mysql)]
            Conn::MySql(conn) => conn.close().await,
        }
    }
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
