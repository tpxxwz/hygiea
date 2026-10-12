//! SeaORM 专用的部分，feature `seaorm`：
//! - [`transaction`] 等事务函数（`transaction.rs`）
//! - [`find_page`]：取一页再查总数，返回 [`Page`](crate::Page)（`page.rs`）
//! - [`active_models`]：一组 Model 转成 ActiveModel，给 `insert_many` 用（`model.rs`）
//! - [`db!`](crate::db) 和 [`DbResultExt::db`]：执行 SeaORM 的语句，失败时把 `DbErr` 转成 `RepoErr`（`db.rs`）
//! - [`HyUtcDateTime`](crate::HyUtcDateTime) 的 trait 实现（`datetime.rs`）
//! - 连接池组件，一种数据库一个文件（`pool/`），再导出到 crate 根

mod datetime;
mod db;
mod model;
mod page;
pub(crate) mod pool;
mod transaction;

#[doc(hidden)]
pub use db::__private;
pub use db::DbResultExt;
pub use model::active_models;
pub use page::find_page;
pub use transaction::{transaction, transaction_snapshot, transaction_with_config};
