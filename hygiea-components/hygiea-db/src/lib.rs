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
//! 开 `sqlx` 或 `seaorm` 就有 UTC 时间列类型 [`HyUtcDateTime`]，和数据库无关，两个框架同时开也是同一个类型。
//!
//! 连接池按组件名放进 Resources，同一个组件可以用不同名字注册多次（主库、从库）。
//! 依赖它的组件声明 `ResourceId::named::<SqlxPgPool>("primary")`，Registry 会把连接池排在前面启动。
//!
//! ```toml
//! hygiea-db = { version = "..", features = ["sqlx", "postgres"] }
//! ```

#[cfg(any(feature = "sqlx", feature = "seaorm"))]
mod datetime;
#[cfg(all(feature = "seaorm", feature = "postgres"))]
mod seaorm_postgres;
#[cfg(all(feature = "sqlx", feature = "postgres"))]
mod sqlx_postgres;
#[cfg(all(feature = "sqlx", feature = "sqlite"))]
mod sqlx_sqlite;

#[cfg(any(feature = "sqlx", feature = "seaorm"))]
pub use datetime::HyUtcDateTime;
#[cfg(all(feature = "seaorm", feature = "postgres"))]
pub use seaorm_postgres::*;
#[cfg(all(feature = "sqlx", feature = "postgres"))]
pub use sqlx_postgres::*;
#[cfg(all(feature = "sqlx", feature = "sqlite"))]
pub use sqlx_sqlite::*;
