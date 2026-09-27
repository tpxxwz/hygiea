//! hygiea 的 Redis 组件：连接池，放进 Resources 给其他组件和业务共用。
//!
//! | feature | 组件 | 连接池（资源类型） |
//! |---|---|---|
//! | `fred` | [`RedisComponent`] | [`FredRedisPool`] |
//!
//! 连接池按组件名放进 Resources；缓存、限流等上层能力依赖它
//! （`ResourceId::named::<FredRedisPool>("cache")`），共用同一个连接池。
//!
//! ```toml
//! hygiea-redis = { version = "..", features = ["fred"] }
//! ```

#[cfg(feature = "fred")]
mod fred_pool;

#[cfg(feature = "fred")]
pub use fred_pool::*;
