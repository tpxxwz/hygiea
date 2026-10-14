//! 锁的 tokio-postgres 实现，一种数据库一个文件（现在只有 PG），这里只做声明和导出。规则在上层的 `lock/`，SQL 在 `sql/<数据库>/lock.rs`

mod postgres;

pub(crate) use postgres::TokioPgLock;
