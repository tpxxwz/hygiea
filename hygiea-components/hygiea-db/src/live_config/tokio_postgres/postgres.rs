//! 动态配置在 PostgreSQL 上的 tokio-postgres 实现：一条专用连接，自己驱动 tokio-postgres 的 `Connection`
//! （`poll_message`）收 NOTIFY。SQL 在 `sql/postgres/live_config.*`

use serde_json::{Map, Value};
use tokio::sync::mpsc;
use tokio_postgres::{AsyncMessage, Client, Config, NoTls, Notification};

use hygiea_core::{Result, ResultExt, err};

use super::super::Backend;
use super::super::schema::{State, Tables, broken, check_err, created, state};
use super::super::worker::{Row, Signal};
use crate::RepoErr;
use crate::sql::Columns;
use crate::sql::postgres::DDL_LOCK;
use crate::sql::postgres::live_config::{OBJECTS, parse_payload, seed_param};
use crate::tokio_postgres::internal::{begin, columns};

/// 一条专用连接。`client` 是 `None`：还没连，或者报错后丢掉了，[`open`](Self::open) 重连
pub(super) struct PgConn {
    config: Config,
    client: Option<Client>,
    /// 驱动连接的后台任务转过来的通知；连接断了任务结束，这里收到 `None`
    notifications: Option<mpsc::UnboundedReceiver<Notification>>,
}

impl PgConn {
    /// 主池有 `application_name`（应用名）时，这条连接用 `<应用名>-live-config`，在 `pg_stat_activity` 里认得出来
    pub(super) fn new(mut config: Config) -> Self {
        if let Some(name) = config
            .get_application_name()
            .map(|name| format!("{name}-live-config"))
        {
            config.application_name(name);
        }
        Self {
            config,
            client: None,
            notifications: None,
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.client.is_some()
    }

    /// 连上、起后台任务收通知、LISTEN；`ensure_tables` 时先检查表和函数、触发器
    pub(super) async fn open(&mut self, tables: &Tables, ensure_tables: bool) -> Result<()> {
        let setup_err = || err!(RepoErr::ExecFailed, "live config setup");
        let (client, mut connection) = self.config.connect(NoTls).await.wrap_err(setup_err)?;
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            // 连接上的异步消息只能这样取（`connection.await` 会把通知丢掉）。client 被丢掉、连接断了都会结束
            while let Some(message) = std::future::poll_fn(|cx| connection.poll_message(cx)).await {
                match message {
                    Ok(AsyncMessage::Notification(notification)) => {
                        if tx.send(notification).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("live config: connection closed: {e}");
                        break;
                    }
                }
            }
        });
        if ensure_tables && let Err(e) = ensure_postgres(&client, tables).await {
            // 这条连接只给动态配置用，出错就整条丢掉，不用回滚
            return Err(e);
        }
        // channel 名是框架拼的（前缀校验过），拼进 SQL 安全
        client
            .batch_execute(&format!("LISTEN {}", tables.channel()))
            .await
            .wrap_err(|| err!(RepoErr::ExecFailed, "live config listen"))?;
        self.client = Some(client);
        self.notifications = Some(rx);
        Ok(())
    }

    /// 丢掉连接（后台任务跟着结束），下次 [`open`](Self::open) 重连
    pub(super) fn reset(&mut self) -> bool {
        self.notifications = None;
        self.client.take().is_some()
    }

    /// 没连上时报错，worker 会先重建再用
    fn client(&self) -> std::result::Result<&Client, ConnClosed> {
        self.client.as_ref().ok_or(ConnClosed)
    }

    pub(super) async fn load(
        &self,
        sql: &str,
        namespace: &str,
    ) -> std::result::Result<Vec<Row>, StoreErr> {
        let rows = self.client()?.query(sql, &[&namespace]).await?;
        Ok(rows
            .iter()
            .map(|row| Ok((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?)))
            .collect::<std::result::Result<_, tokio_postgres::Error>>()?)
    }

    pub(super) async fn revisions(
        &self,
        sql: &str,
    ) -> std::result::Result<Vec<(String, i64)>, StoreErr> {
        let rows = self.client()?.query(sql, &[]).await?;
        Ok(rows
            .iter()
            .map(|row| Ok((row.try_get(0)?, row.try_get(1)?)))
            .collect::<std::result::Result<_, tokio_postgres::Error>>()?)
    }

    /// 一条语句补完：参数是 `{namespace: {key: value}}` 的 JSON 文本，在库里展开
    pub(super) async fn seed(
        &self,
        sql: &str,
        defaults: &[(String, Map<String, Value>)],
    ) -> std::result::Result<(), StoreErr> {
        self.client()?
            .execute(sql, &[&seed_param(defaults)])
            .await?;
        Ok(())
    }

    pub(super) async fn trim_history(
        &self,
        sql: &str,
        keep_rows: i64,
    ) -> std::result::Result<u64, StoreErr> {
        Ok(self.client()?.execute(sql, &[&keep_rows]).await?)
    }

    /// 等下一个通知；还没连上时一直等。可以取消：只是在 channel 上等
    pub(super) async fn recv(&mut self) -> Result<Signal> {
        let Some(notifications) = self.notifications.as_mut() else {
            return std::future::pending().await;
        };
        loop {
            let Some(notification) = notifications.recv().await else {
                // 连接断了：报错，worker 调 reset 丢掉、重连，重连后对一遍版本号补上断线期间的变更
                return Err(err!(RepoErr::ExecFailed, "live config receive"));
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

    /// 丢掉连接，后台任务跟着结束
    pub(super) fn close(&mut self) {
        self.reset();
    }
}

/// 还没连上（或者报错后丢掉了）
#[derive(Debug)]
pub(super) struct ConnClosed;

impl std::fmt::Display for ConnClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("live config connection is not open")
    }
}

impl std::error::Error for ConnClosed {}

/// 查询的错误：没连上，或者 PG 报错
#[derive(Debug)]
pub(super) enum StoreErr {
    Closed(ConnClosed),
    Pg(tokio_postgres::Error),
}

impl std::fmt::Display for StoreErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed(e) => e.fmt(f),
            Self::Pg(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for StoreErr {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Closed(e) => Some(e),
            Self::Pg(e) => Some(e),
        }
    }
}

impl From<ConnClosed> for StoreErr {
    fn from(e: ConnClosed) -> Self {
        Self::Closed(e)
    }
}

impl From<tokio_postgres::Error> for StoreErr {
    fn from(e: tokio_postgres::Error) -> Self {
        Self::Pg(e)
    }
}

// ---- 检查、建表 -------------------------------------------------------------------

/// 检查表和函数、触发器，没有就在事务里建（规则见 `schema.rs`）
async fn ensure_postgres(client: &Client, tables: &Tables) -> Result<()> {
    let backend = Backend::Postgres;
    let mut expected = tables.functions();
    expected.extend(tables.triggers(backend));
    let check =
        |columns: &Columns, objects: &[String]| state(tables, backend, columns, &expected, objects);

    let found = columns(client, tables.table_names().to_vec())
        .await
        .wrap_err(check_err)?;
    let objects = existing_objects(client, tables).await.wrap_err(check_err)?;
    match check(&found, &objects) {
        State::Ready => return Ok(()),
        State::Broken(problems) => return broken(&problems, tables, backend),
        State::Absent => {}
    }

    // 等建表锁不被 DBA 设的超时打断，见 sql::postgres::begin_sql
    begin(client, 0).await.wrap_err(check_err)?;
    client
        .execute(
            DDL_LOCK,
            &[
                &hygiea_core::builtin::LOCK_TYPE,
                &(hygiea_core::builtin::Lock::LiveConfigDdl as i32),
            ],
        )
        .await
        .wrap_err(check_err)?;
    let found = columns(client, tables.table_names().to_vec())
        .await
        .wrap_err(check_err)?;
    let objects = existing_objects(client, tables).await.wrap_err(check_err)?;
    match check(&found, &objects) {
        State::Ready => {
            client.batch_execute("ROLLBACK").await.wrap_err(check_err)?;
            return Ok(());
        }
        State::Broken(problems) => return broken(&problems, tables, backend),
        State::Absent => {}
    }
    let result = client
        .batch_execute(&format!("{}\nCOMMIT;", tables.ddl(backend)))
        .await;
    created(result, tables, backend)
}

/// 库里已有的同名函数、触发器
async fn existing_objects(
    client: &Client,
    tables: &Tables,
) -> std::result::Result<Vec<String>, tokio_postgres::Error> {
    let rows = client
        .query(
            OBJECTS,
            &[&tables.functions(), &tables.triggers(Backend::Postgres)],
        )
        .await?;
    rows.iter().map(|row| row.try_get(0)).collect()
}
