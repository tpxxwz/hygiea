//! hygiea 的数据库组件：连接池，放进 Resources 给其他组件和业务用。
//!
//! 用 feature 选框架和数据库，两个维度分开开，同时打开的组合才会编译：
//!
//! | feature | 组件 | 连接池（资源类型） |
//! |---|---|---|
//! | `sqlx` + `postgres` | [`SqlxPgComponent`] | [`SqlxPgPool`] |
//! | `sqlx` + `sqlite` | [`SqlxSqliteComponent`] | [`SqlxSqlitePool`] |
//! | `seaorm` + `postgres` | [`SeaOrmPgComponent`] | [`SeaOrmPgPool`] |
//! | `mysql` | 预留，还没有实现 | |
//!
//! 和框架无关、开 `sqlx` 或 `seaorm` 就有的：
//! - UTC 时间列类型 [`HyUtcDateTime`]，两个框架同时开也是同一个类型
//! - 分页的请求和结果 [`PageRequest`]、[`Page`]
//! - 数据库错误 [`RepoErr`]，按操作分：0x 事务、1x 执行语句（10 兜底，11-14 增删改查）、9x 参数
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
//! ## 连接池和 Resources
//!
//! 连接池按组件名放进 Resources，同一个组件可以用不同名字注册多次（主库、从库）。
//! 依赖它的组件声明 `ResourceId::named::<SqlxPgPool>("primary")`，Registry 会把连接池排在前面启动。
//!
//! ```toml
//! hygiea-db = { version = "..", features = ["sqlx", "postgres"] }
//! ```

#[cfg(any(feature = "sqlx", feature = "seaorm"))]
mod datetime;
#[cfg(any(feature = "sqlx", feature = "seaorm"))]
mod error;
#[cfg(any(feature = "sqlx", feature = "seaorm"))]
mod page;
#[cfg(feature = "seaorm")]
pub mod seaorm;
#[cfg(feature = "sqlx")]
pub mod sqlx;

#[cfg(all(feature = "sqlx", feature = "postgres"))]
pub use self::sqlx::pool::postgres::*;
#[cfg(all(feature = "sqlx", feature = "sqlite"))]
pub use self::sqlx::pool::sqlite::*;
#[cfg(any(feature = "sqlx", feature = "seaorm"))]
pub use datetime::HyUtcDateTime;
#[cfg(any(feature = "sqlx", feature = "seaorm"))]
pub use error::RepoErr;
#[cfg(any(feature = "sqlx", feature = "seaorm"))]
pub use page::{Page, PageRequest};
#[cfg(all(feature = "seaorm", feature = "postgres"))]
pub use seaorm::pool::postgres::*;
