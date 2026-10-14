//! 动态配置在 PostgreSQL 上的 sqlx 实现：小池上建 listener，推送 + 轮询。SQL 在 `sql/postgres/live_config.*`

use serde_json::{Map, Value};
use sqlx::postgres::{PgConnectOptions, PgListener, PgPool};
use sqlx::{AssertSqlSafe, Connection, PgConnection};

use hygiea_core::{Result, ResultExt, err};

use super::super::Backend;
use super::super::schema::{State, Tables, broken, check_err, created, state};
use super::super::worker::{Row, Signal};
use super::fixed;
use crate::sql::Columns;
use crate::sql::postgres::live_config::{OBJECTS, parse_payload, seed_param};
use crate::sql::postgres::{DDL_LOCK, begin_sql};
use crate::{PoolConfig, RepoErr};

/// listener 占着小池里唯一那条连接：监听、加载、轮询都走它，断了 sqlx 自动重连、重新 LISTEN。
/// `listener` 是 `None`：还没建，或者报错后丢掉了，[`open`](Self::open) 在同一个小池上重建
pub(super) struct PgConn {
    pool: PgPool,
    listener: Option<Box<PgListener>>,
}

impl PgConn {
    /// `options` 是主池的连接参数。主池有 `application_name`（应用名）时，小池用 `<应用名>-live-config`，
    /// 带到小池的每条连接上（包括 sqlx 断线重连时新开的），在 `pg_stat_activity` 里认得出来
    pub(super) fn new(options: PgConnectOptions, main: &PoolConfig) -> Self {
        let options = match options
            .get_application_name()
            .map(|name| format!("{name}-live-config"))
        {
            Some(name) => options.application_name(&name),
            None => options,
        };
        Self {
            pool: fixed(main).connect_lazy_with(options),
            listener: None,
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.listener.is_some()
    }

    /// 建 listener、LISTEN；`ensure_tables` 时先检查表和函数、触发器
    pub(super) async fn open(&mut self, tables: &Tables, ensure_tables: bool) -> Result<()> {
        let setup_err = || err!(RepoErr::ExecFailed, "live config setup");
        let mut opened = PgListener::connect_with(&self.pool)
            .await
            .wrap_err(setup_err)?;
        if ensure_tables {
            let conn = sqlx::Acquire::acquire(&mut opened)
                .await
                .wrap_err(setup_err)?;
            if let Err(e) = ensure_postgres(&mut *conn, tables).await {
                // begin_with 中途报错时 sqlx 不回滚，连接会带着失败的事务回到小池，补一条 ROLLBACK
                // （不在事务里时 PG 只给警告；这条也失败说明连接坏了，归还时会被关掉）
                let _ = sqlx::raw_sql("ROLLBACK").execute(conn).await;
                return Err(e);
            }
        }
        opened
            .listen(&tables.channel())
            .await
            .wrap_err(|| err!(RepoErr::ExecFailed, "live config listen"))?;
        self.listener = Some(Box::new(opened));
        Ok(())
    }

    /// 丢掉 listener，下次 [`open`](Self::open) 重建：sqlx 只对部分 IO 错误自己重连，别的错误会一直留着坏连接
    pub(super) fn reset(&mut self) -> bool {
        self.listener.take().is_some()
    }

    /// 小池最多 1 条连接、被 listener 占着，查询都走它（走 `&pool` 拿不到连接，等到 acquire 超时）。
    /// 没建（还没 open，或者报错后丢掉了）时报错，worker 会先重建再用
    fn listener(&mut self) -> sqlx::Result<&mut PgListener> {
        self.listener.as_deref_mut().ok_or(sqlx::Error::PoolClosed)
    }

    pub(super) async fn load(&mut self, sql: &str, namespace: &str) -> sqlx::Result<Vec<Row>> {
        sqlx::query_as(AssertSqlSafe(sql))
            .bind(namespace)
            .fetch_all(self.listener()?)
            .await
    }

    pub(super) async fn revisions(&mut self, sql: &str) -> sqlx::Result<Vec<(String, i64)>> {
        sqlx::query_as(AssertSqlSafe(sql))
            .fetch_all(self.listener()?)
            .await
    }

    /// 一条语句补完：参数是 `{namespace: {key: value}}`，在库里展开
    pub(super) async fn seed(
        &mut self,
        sql: &str,
        defaults: &[(String, Map<String, Value>)],
    ) -> sqlx::Result<()> {
        sqlx::query(AssertSqlSafe(sql))
            .bind(seed_param(defaults))
            .execute(self.listener()?)
            .await
            .map(|_| ())
    }

    pub(super) async fn trim_history(&mut self, sql: &str, keep_rows: i64) -> sqlx::Result<u64> {
        sqlx::query(AssertSqlSafe(sql))
            .bind(keep_rows)
            .execute(self.listener()?)
            .await
            .map(|r| r.rows_affected())
    }

    /// 等下一个通知；还没建好 listener 时一直等。
    /// 可以取消：sqlx 收消息时读完整条才动缓冲区（`PgStream::recv_unchecked`）
    pub(super) async fn recv(&mut self) -> Result<Signal> {
        let Some(listener) = self.listener.as_mut() else {
            return std::future::pending().await;
        };
        loop {
            let received = listener
                .try_recv()
                .await
                .wrap_err(|| err!(RepoErr::ExecFailed, "live config receive"))?;
            let Some(notification) = received else {
                return Ok(Signal::Reconnected);
            };
            match parse_payload(notification.payload()) {
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

    /// 放掉 listener，等借出去的连接还回来后关掉小池
    pub(super) async fn close(&mut self) {
        self.listener.take();
        self.pool.close().await;
    }
}

// ---- 检查、建表 -------------------------------------------------------------------

async fn postgres_columns(conn: &mut PgConnection, tables: &Tables) -> sqlx::Result<Columns> {
    crate::sqlx::internal::postgres::columns(conn, tables.table_names().to_vec()).await
}

async fn ensure_postgres(conn: &mut PgConnection, tables: &Tables) -> Result<()> {
    let backend = Backend::Postgres;
    let mut expected = tables.functions();
    expected.extend(tables.triggers(backend));
    let check =
        |columns: &Columns, objects: &[String]| state(tables, backend, columns, &expected, objects);

    let columns = postgres_columns(conn, tables).await.wrap_err(check_err)?;
    let objects = postgres_objects(conn, tables).await.wrap_err(check_err)?;
    match check(&columns, &objects) {
        State::Ready => return Ok(()),
        State::Broken(problems) => return broken(&problems, tables, backend),
        State::Absent => {}
    }

    // 等建表锁不被 DBA 设的超时打断，见 sql::postgres::begin_sql
    let mut tx = conn
        .begin_with(AssertSqlSafe(begin_sql(0)))
        .await
        .wrap_err(check_err)?;
    sqlx::query(DDL_LOCK)
        .bind(hygiea_core::builtin::LOCK_TYPE)
        .bind(hygiea_core::builtin::Lock::LiveConfigDdl as i32)
        .execute(&mut *tx)
        .await
        .wrap_err(check_err)?;
    let columns = postgres_columns(&mut tx, tables)
        .await
        .wrap_err(check_err)?;
    let objects = postgres_objects(&mut tx, tables)
        .await
        .wrap_err(check_err)?;
    match check(&columns, &objects) {
        State::Ready => return Ok(()),
        State::Broken(problems) => return broken(&problems, tables, backend),
        State::Absent => {}
    }
    let result = async {
        sqlx::raw_sql(AssertSqlSafe(tables.ddl(backend)))
            .execute(&mut *tx)
            .await?;
        tx.commit().await
    }
    .await;
    created(result, tables, backend)
}

/// 库里已有的同名函数、触发器（触发器挂在哪张表上都算）
async fn postgres_objects(conn: &mut PgConnection, tables: &Tables) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(OBJECTS)
        .bind(tables.functions())
        .bind(tables.triggers(Backend::Postgres))
        .fetch_all(&mut *conn)
        .await
}
