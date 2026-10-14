//! 数据库配置表（动态配置），feature `live-config`
//!
//! 配置存在库里，改了表，运行中的程序自动拿到新值。各连接池组件（sqlx、SeaORM 的 PG、MySQL、SQLite）`uses` 带 2 时，
//! 按组件名把一个源 [`DbLiveConfig`] 放进 Resources。几个组件都开就是几个源（比如应用自己的库一个、平台公共配置库一个），
//! 各管各的 namespace，按组件名取。
//! 源连 `[db.connect]` 的库（连接参数、search_path 一样），有业务池（`uses` 带 1）时 `[db.live_config.connect]` 里写了的项
//! 换成写的（换个 schema、用户）；整个库都不一样的另起一个 `uses = 2` 的组件。走自己的独立小池（固定 1 条连接），不占业务池。
//! 第一次绑定时才连库、建表，不用的话什么都不做。
//!
//! ```ignore
//! /// 每个字段是一个 namespace（字段名就是 namespace 名，可以 serde rename），再下一层字段是 key
//! #[derive(Serialize, Deserialize, Default)]
//! pub struct RuntimeConfig {
//!     pub markji_sync: MarkjiSync,
//!     pub moji_cache: MojiCache,
//! }
//!
//! let source = resources.require_named::<DbLiveConfig>("main")?;
//! let runtime = source.bind_all(RuntimeConfig::default()).await?;            // LiveConfig<RuntimeConfig>
//! let sync = source.bind("markji_sync", MarkjiSync::default()).await?;       // 只绑一个 namespace
//!
//! let cfg = runtime.get();   // Arc<RuntimeConfig>，当前最新的快照；用的时候不持有锁
//! ```
//!
//! 配置在连接池配置的 `live_config` 段（[`LiveConfigConfig`]），比如 `[db.live_config]`，`uses` 带 2 时才能写，不写用默认值。
//!
//!
//! # 原理
//!
//! 详细的流程图见仓库里的 `docs/design/live-config.md`，这里是要点：
//!
//! - 三张表：`{p}config`（`(namespace, key)` → JSON 值）、`{p}config_revision`（每个 namespace 的版本号）、
//!   `{p}config_history`（每次变更的新旧值），后两张由触发器写。前缀 `{p}` 默认是应用名加 `_`
//!   （`[registry] app_name`，比如 `dict_jp` → `dict_jp_config`），`table_prefix` 可以改、可以写 `""` 不加前缀；
//!   表建在连接 search_path 的第一个 schema 里。几个应用共用一套表，就把它们的前缀（和库、schema）配成一样
//! - 第一次绑定时检查表：一个都没有就建，都在且列一致就用，有一个对不上就报 [`RepoErr::LiveConfigSchemaMismatch`](crate::RepoErr::LiveConfigSchemaMismatch)，
//!   建不了报 [`RepoErr::LiveConfigCreateFailed`](crate::RepoErr::LiveConfigCreateFailed)（日志里有 DDL）。框架不做迁移
//! - 绑定：默认值里表没有的 key 补进表（有的不动），读表、覆盖到默认值上转成 `T`。启动时转不了报
//!   [`RepoErr::LiveConfigDecodeFailed`](crate::RepoErr::LiveConfigDecodeFailed)，运行中转不了保留旧值
//! - 变更：PG 靠触发器 NOTIFY 推送（channel 是 `<schema>.{p}config`，不同 schema 里同名的表互不打扰），另每 `poll_interval_secs`（默认 60 秒）兜底；SQLite、MySQL 没有推送，每 `poll_interval_secs`（默认 1 秒）
//!   轮询版本号。只接受更大的版本号，变了只重新读那个 namespace
//! - 一个源一个后台任务、一个独立小池（1 条连接），不占业务池；断线由 sqlx 重连，PG 的 listener 坏了在小池上重建
//!
//! 文件：和驱动无关的是 `source.rs` 绑定入口和后台任务的启动，`handle.rs` 对外的句柄，`worker.rs` 后台任务，
//! `schema.rs` 表结构和检查规则；SQL 在 `sql/<数据库>/live_config.rs`；执行在 `sqlx/`（一种数据库一个文件，`mod.rs` 的 `Store` 分发），
//! 现在只有 sqlx 的实现（SeaORM 底层也是 sqlx）；接别的框架见 `sqlx/mod.rs` 的说明
//!
//! # 用的时候要注意
//!
//! - struct 的字段**改名**：旧 key 留在表里（打 warn），新 key 按代码默认值补进表，线上改过的值不会跟过去。
//!   要保留就手动 `UPDATE {p}config SET key = '新名' WHERE namespace = .. AND key = '旧名'`（触发器记 D + I）。
//!   `#[serde(alias)]` 只管读，补默认值时还是按新名插入
//! - 字段**改类型**：表里还是旧类型的值，转不成新类型时启动报 [`RepoErr::LiveConfigDecodeFailed`](crate::RepoErr::LiveConfigDecodeFailed)。
//!   先把表里的值改成新类型，或者换个 key 名
//! - **删 key**：等于恢复成代码当前的默认值，下次绑定时补回表里。不想要默认值就改值，不要删行
//! - 要表达"没有值"（比如不限）别用 `Option`，用单独的开关字段或者约定的值（比如 0）
//! - PG **只读从库**（hot standby）上不能 `LISTEN`，也建不了表：只在连主库的组件上开
//! - MySQL 默认开着 binlog，没有 `SUPER` 的用户建不了触发器（报 1419）：DBA 打开 `log_bin_trust_function_creators`，
//!   或者用有权限的账号先建好表，之后普通用户就能用：
//!   - DDL 在 `sql/mysql/live_config.sql`：`{p}` 换成表名前缀，按 `-- @@` 逐条执行（`mysql` 命令行要先改 `DELIMITER`），
//!     在组件 `[db.connect]` 的那个库里执行。建表失败时 error 日志里有换好前缀的完整 DDL，直接复制更省事
//!   - 业务用户对三张表要有 `SELECT`、`INSERT`、`UPDATE`、`DELETE`（绑定时补默认值、清理历史要删行）。
//!     触发器的定义者是建它的账号，历史表的 `changed_by` 记的是 `USER()`（实际改的账号），不受影响
//!   - DDL 会隐式提交，普通用户建到触发器才报 1419 时，库里留下有表、没触发器的半成品，下次启动报
//!     [`RepoErr::LiveConfigSchemaMismatch`](crate::RepoErr::LiveConfigSchemaMismatch)：用有权限的账号只执行剩下的三条
//!     `CREATE TRIGGER`，或者删掉三张表再整份执行
//! - SQLite 的 `:memory:` 库（不共享缓存）用不了：小池另开一条连接，连到的是另一个空库
//! - 组件关闭后 `get()` 还是最后一次的值，`changed()` 返回 [`RepoErr::LiveConfigClosed`](crate::RepoErr::LiveConfigClosed)

mod handle;
mod schema;
mod source;
// 执行：每个驱动一个目录，worker 经 store.rs 的 Store 分发过去
#[cfg(sqlx_live_config)]
mod sqlx;
mod store;
mod worker;

use serde::Deserialize;

#[cfg(live_config_mysql)]
use crate::config::MySqlConnectOverride;
#[cfg(live_config_pg)]
use crate::config::PgConnectOverride;
#[cfg(live_config_sqlite)]
use crate::config::SqliteConnectOverride;

pub use handle::{LiveConfig, LiveRef, LiveView};

/// 动态配置支持的后端，按它分 PG / SQLite / MySQL。variant 跟着 feature 走：没开 `sqlite` 就没有 `Sqlite`，
/// `match` 时漏了已开的后端编译不过
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Backend {
    #[cfg(live_config_pg)]
    Postgres,
    #[cfg(live_config_sqlite)]
    Sqlite,
    #[cfg(live_config_mysql)]
    MySql,
}
use source::LiveConfigSource;
pub(crate) use store::Store;

// ---- config -----------------------------------------------------------------

/// 连接池配置里的 `live_config` 段，`uses` 带 2 时才能写。`C` 是单独连库的覆盖项：PG 是 `PgConnectOverride`、SQLite 是 `SqliteConnectOverride`、MySQL 是 `MySqlConnectOverride`
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LiveConfigConfig<C> {
    /// 对一遍版本号的间隔（秒）。PostgreSQL 上是兜底（变更靠 NOTIFY 推过来），不写是 60；
    /// SQLite、MySQL 上是主要的检查方式（改了表最多等这么久生效），不写是 1
    pub poll_interval_secs: Option<u64>,
    /// 变更历史表（`{p}config_history`）最多留最新的多少行，更早的删掉。
    /// 和 `history_trim_interval_mins` 要么都配、要么都不配（只配一个启动报错），都不配不删
    pub history_keep_rows: Option<u64>,
    /// 隔多少分钟清一次变更历史表。后台任务启动时先清一次；多个实例都会清，结果一样
    pub history_trim_interval_mins: Option<u64>,
    /// 表名前缀：不写用应用名（`[registry] app_name`）加 `_`，写 `""` 不加前缀，写 `foo` 或 `foo_` 都是 `foo_`。
    /// 去掉末尾的 `_` 后得是小写下划线 `[a-z][a-z0-9_]*`
    pub table_prefix: Option<String>,
    /// 单独连的库：`[db.live_config.connect]`，只写和 `[db.connect]` 不一样的项，不写的沿用。
    /// 只有 `uses` 带 1（有业务池）时能写，没有业务池时直接写在 `[db.connect]` 里
    pub connect: C,
}

/// PG 连接池（sqlx、SeaORM）的 `live_config` 段
#[cfg(live_config_pg)]
pub type PgLiveConfigConfig = LiveConfigConfig<PgConnectOverride>;

/// SQLite 连接池的 `live_config` 段
#[cfg(live_config_sqlite)]
pub type SqliteLiveConfigConfig = LiveConfigConfig<SqliteConnectOverride>;

/// MySQL 连接池（sqlx、SeaORM）的 `live_config` 段
#[cfg(live_config_mysql)]
pub type MySqlLiveConfigConfig = LiveConfigConfig<MySqlConnectOverride>;

// ---- 资源 ---------------------------------------------------------------------

/// 一个动态配置源：`uses` 带 2（见 [`DbUses`](crate::DbUses)）的连接池组件按组件名放进 Resources。
/// 几个组件都开就是几个源，各管各的 namespace。一个源一个后台任务、一个独立小池（1 条连接），第一次绑定时才连库、建表。
/// `Clone` 后共用同一个后台任务
#[derive(Clone, Debug)]
pub struct DbLiveConfig {
    source: LiveConfigSource,
}

impl DbLiveConfig {
    /// 只校验配置、不连库。`store` 拿表名建出各数据库的 [`Store`]
    pub(crate) fn new<C>(
        store: impl FnOnce(schema::Tables) -> hygiea_core::Result<Store>,
        config: &LiveConfigConfig<C>,
        registry_config: &hygiea_core::app::RegistryConfig,
    ) -> hygiea_core::Result<Self> {
        let prefix = crate::sql::table_prefix(
            config.table_prefix.as_deref(),
            &registry_config.app_name,
            "live config",
        )?;
        let source = LiveConfigSource::new(store(schema::Tables::new(prefix))?, config)?;
        Ok(Self { source })
    }

    /// 把一个 namespace 绑定到 `T`（第一层字段是 key），见 [模块文档](self)。
    /// `defaults` 是表里没有的 key 用的值，第一次绑定时补进表里
    pub async fn bind<T>(&self, namespace: &str, defaults: T) -> hygiea_core::Result<LiveConfig<T>>
    where
        T: serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
    {
        self.source.bind(namespace, defaults).await
    }

    /// 把多个 namespace 绑定到 `T`（第一层字段是 namespace，第二层是 key），见 [模块文档](self)
    pub async fn bind_all<T>(&self, defaults: T) -> hygiea_core::Result<LiveConfig<T>>
    where
        T: serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
    {
        self.source.bind_all(defaults).await
    }

    /// 组件关闭时调用：后台任务退出、小池关掉，之后绑定报 [`RepoErr::LiveConfigClosed`](crate::RepoErr::LiveConfigClosed)
    pub(crate) async fn close(&self) {
        self.source.close().await;
    }
}
