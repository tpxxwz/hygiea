//! hygiea 的数据库组件：连接池、动态配置、分布式锁，放进 Resources 给其他组件和业务用。
//!
//! 用 feature 选框架、数据库和功能，三个维度分开开、只增加功能，同时打开的组合才会编译
//! （组合条件集中在 `build.rs` 的 cfg 别名里）：
//!
//! | feature | 组件 | 业务池（资源类型） |
//! |---|---|---|
//! | `sqlx` + `postgres` | [`SqlxPgComponent`] | [`SqlxPgPool`] |
//! | `sqlx` + `sqlite` | [`SqlxSqliteComponent`] | [`SqlxSqlitePool`] |
//! | `seaorm` + `postgres` | [`SeaOrmPgComponent`] | [`SeaOrmPgPool`] |
//! | `sqlx` + `mysql` | `SqlxMySqlComponent` | `SqlxMySqlPool` |
//! | `seaorm` + `mysql` | `SeaOrmMySqlComponent` | `SeaOrmMySqlPool` |
//! | `seaorm` + `sqlite` | `SeaOrmSqliteComponent` | `SeaOrmSqlitePool` |
//!
//! 功能 `live-config`、`lock` / `lock-row` 的 feature 不绑定驱动，实现按驱动各写一份。现在只有 sqlx 的实现
//! （sqlx、SeaORM 的连接池共用）。加别的框架、数据库照 research/db-drivers 分支的写法。
//! MySQL 要 8.0 以上：锁用 `GET_LOCK`，动态配置只靠轮询（没有推送），见 [`lock`]、[`live_config`] 模块文档。
//!
//! 组件提供什么由配置的 `uses` 决定（[`DbUses`]，按位加：1 业务池、2 动态配置、4 锁，不写是 1），
//! 6 种组件都是同一个泛型组件 [`DbComponent`] 的别名。配置分段：`[db.connect]` 连哪个库（[`PgConnectConfig`] / [`SqliteConnectConfig`] / `MySqlConnectConfig`），`[db.pool]` 池的参数
//! （[`PoolConfig`]，各数据库、框架共用），SeaORM 另有 `[db.log]`，这两段只给业务池用，`uses` 带 1 时才能写。PG 连接的 `application_name` 用 `[registry] app_name`
//! （建连接池时从 `RegistryConfig` 拿到），不能在 `connect.params` 里自己写。
//!
//! 开了 `live-config`、`uses` 带 2 时组件按组件名放一个 `DbLiveConfig`：数据库配置表，改了表程序自动拿到新值，
//! 配置在 `[db.live_config]`，见 [`live_config`] 模块文档。开了 `lock`、`uses` 带 4 时（PG、MySQL）组件匿名放一个
//! `DbLock`，整个应用只能有一个，配置在 `[db.lock]`，见 [`lock`] 模块文档。
//!
//! 和框架无关、开 `sqlx` 或 `seaorm` 就有的：
//! - UTC 时间列类型 [`HyUtcDateTime`]，两个框架同时开也是同一个类型
//! - 分页的请求和结果 [`PageRequest`]、[`Page`]
//! - 数据库错误 [`RepoErr`]，按操作分：0x 事务、1x 执行语句（10 兜底，11-14 增删改查）、2x 动态配置、9x 参数
//!
//! 各框架专用的在 [`sqlx`](mod@sqlx) / [`seaorm`] 模块里，比如事务函数 `sqlx::transaction`、`seaorm::transaction`；
//! 连接池组件也在里面，再导出到 crate 根。
//!
//! ## SeaORM 的用法
//!
//! 增删改查照 SeaORM 写，hygiea 不再封装一层，只负责把 `DbErr` 转成 [`RepoErr`]、开事务、分页：
//!
//! ```ignore
//! use hygiea::db::{PageRequest, SeaOrmPgPool, db};
//! use hygiea::db::seaorm::{DbResultExt, active_models, find_page, transaction};
//!
//! // 拿连接池：组件启动后从 Resources 取（比如初始化 AppState 时）
//! let pool = resources.require::<SeaOrmPgPool>()?;
//!
//! // repository：参数收 &impl ConnectionTrait，连接池、事务都能传
//! async fn find_user(db: &impl ConnectionTrait, id: i64) -> Result<Option<user::Model>> {
//!     db!(user::Entity::find_by_id(id).one(db)) // 失败是 QueryFailed，op "public.user at src/repo.rs:12"
//! }
//! async fn save_users(db: &impl ConnectionTrait, users: &[user::Model]) -> Result<()> {
//!     db!(user::Entity::insert_many(active_models(users))
//!         .on_conflict_do_nothing()
//!         .exec_without_returning(db))?;
//!     Ok(())
//! }
//!
//! // service：要一起成功的放进事务，闭包返回 Ok 提交、Err 回滚
//! transaction(&*pool, "replace users", async |txn| {
//!     db!(user::Entity::delete_many().exec(txn))?;
//!     save_users(txn, &users).await
//! })
//! .await?;
//!
//! // 分页：取一页再查总数
//! let select = user::Entity::find().order_by_asc(user::Column::Id);
//! let page = find_page(&*pool, select, PageRequest::new(0, 20)?).await?;
//!
//! // 拿不到表名的（into_tuple / into_model 之后、原生 SQL、分页器）必须写 op，不写编译报错
//! let ids: Vec<i64> = db!(
//!     user::Entity::find().select_only().column(user::Column::Id).into_tuple().all(&*pool),
//!     "user ids"
//! )?;
//! // db! 包不住的（load_many、cursor_by、流里的每一行）接 .db()：ExecFailed，op 是调用位置
//! let fruits = cakes.load_many(fruit::Entity, &*pool).await.db()?;
//! ```
//!
//! - [`db!`](crate::db) 认哪些写法、op 的格式见它的文档
//! - 事务：[`seaorm::transaction`] 用数据库默认的隔离级别，[`seaorm::transaction_snapshot`] 是一致性只读，
//!   [`seaorm::transaction_with_config`] 自己指定。在事务闭包给的 `txn` 上再开事务是 savepoint（嵌套），
//!   要用同一个事务就把 `txn` 往下传。行锁（`lock_exclusive()` 等）要在事务里、用 `txn` 执行才有用
//! - 排查：设 `RUST_LIB_BACKTRACE=1` 后错误的 `{:?}` 末尾带调用栈
//!
//! ## 组件和 Resources
//!
//! 业务池、`DbLiveConfig` 按组件名放进 Resources，`DbLock` 匿名放。同一个组件可以用不同名字注册多次（主库、从库、
//! 只给锁用的协调库、只给动态配置用的公共配置库），推荐都放在应用配置的 `[db.<名字>]` 下，注册时名字和字段名写成一样：
//!
//! ```ignore
//! struct AppConfig { registry: RegistryConfig, db: Dbs }
//! struct Dbs { main: SeaOrmPgConfig, coord: SqlxMySqlConfig }    // [db.main]、[db.coord]
//!
//! Registry::new()
//!     .add_named::<SeaOrmPgComponent>("main", c.db.main)          // uses = 3：业务池 + 动态配置
//!     .add_named::<SqlxMySqlComponent>("coord", c.db.coord);      // uses = 4：只给锁用
//!
//! let pool = resources.require_named::<SeaOrmPgPool>("main")?;
//! let sync = resources.require_named::<DbLiveConfig>("main")?.bind("markji_sync", MarkjiSync::default()).await?;
//! let lock = resources.require::<DbLock>()?;                      // 里面是哪种数据库调用方不用管
//! ```
//!
//! 依赖它的组件声明 `ResourceId::named::<SqlxPgPool>("primary")`，Registry 会把连接池排在前面启动。
//! 不经过 Registry（测试、脚本）用 [`DbResources::from_config`] 直接建，用完 `close`。
//!
//! ```toml
//! hygiea-db = { version = "..", features = ["sqlx", "postgres"] }
//! ```

#[cfg(any(sqlx_pg, sqlx_sqlite, sqlx_mysql))]
mod component;
#[cfg(any(feature = "sqlx", feature = "seaorm"))]
mod config;
#[cfg(any(feature = "sqlx", feature = "seaorm"))]
mod datetime;
#[cfg(any(feature = "sqlx", feature = "seaorm"))]
mod error;
#[cfg(live_config_any)]
pub mod live_config;
#[cfg(lock_any)]
pub mod lock;
#[cfg(any(feature = "sqlx", feature = "seaorm"))]
mod page;
#[cfg(feature = "seaorm")]
pub mod seaorm;
#[cfg(any(live_config_any, lock_any))]
mod sql;
#[cfg(any(feature = "sqlx", sqlx_live_config, sqlx_lock_pg))]
pub mod sqlx;

#[cfg(all(feature = "sqlx", feature = "mysql"))]
pub use self::sqlx::pool::mysql::*;
#[cfg(all(feature = "sqlx", feature = "postgres"))]
pub use self::sqlx::pool::postgres::*;
#[cfg(all(feature = "sqlx", feature = "sqlite"))]
pub use self::sqlx::pool::sqlite::*;
#[cfg(any(sqlx_pg, sqlx_sqlite, sqlx_mysql))]
pub use component::{DbComponent, DbConfig, DbResources, DbUses};
#[cfg(all(any(feature = "sqlx", feature = "seaorm"), feature = "mysql"))]
pub use config::MySqlConnectConfig;
#[cfg(any(live_config_mysql, lock_mysql))]
pub use config::MySqlConnectOverride;
#[cfg(all(any(feature = "sqlx", feature = "seaorm"), feature = "postgres"))]
pub use config::PgConnectConfig;
#[cfg(any(live_config_pg, lock_pg))]
pub use config::PgConnectOverride;
#[cfg(any(feature = "sqlx", feature = "seaorm"))]
pub use config::PoolConfig;
#[cfg(sqlx_sqlite)]
pub use config::SqliteConnectConfig;
#[cfg(live_config_sqlite)]
pub use config::SqliteConnectOverride;
#[cfg(any(feature = "sqlx", feature = "seaorm"))]
pub use datetime::HyUtcDateTime;
#[cfg(any(feature = "sqlx", feature = "seaorm"))]
pub use error::RepoErr;
#[cfg(live_config_mysql)]
pub use live_config::MySqlLiveConfigConfig;
#[cfg(live_config_pg)]
pub use live_config::PgLiveConfigConfig;
#[cfg(live_config_sqlite)]
pub use live_config::SqliteLiveConfigConfig;
#[cfg(live_config_any)]
pub use live_config::{DbLiveConfig, LiveConfig, LiveConfigConfig, LiveRef, LiveView};
#[cfg(lock_any)]
pub use lock::DbLock;
#[cfg(lock_mysql)]
pub use lock::MySqlLockConfig;
#[cfg(lock_pg)]
pub use lock::PgLockConfig;
#[cfg(any(feature = "sqlx", feature = "seaorm"))]
pub use page::{Page, PageRequest};
#[cfg(feature = "seaorm")]
pub use seaorm::pool::SeaOrmLogConfig;
#[cfg(all(feature = "seaorm", feature = "mysql"))]
pub use seaorm::pool::mysql::*;
#[cfg(all(feature = "seaorm", feature = "postgres"))]
pub use seaorm::pool::postgres::*;
#[cfg(all(feature = "seaorm", feature = "sqlite"))]
pub use seaorm::pool::sqlite::*;
