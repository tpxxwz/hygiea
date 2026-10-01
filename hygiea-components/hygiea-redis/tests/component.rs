//! `RedisComponent` 按组件名声明提供的 `FredRedisPool`；不启动，不连 Redis
//! （经 Registry 启动并连上 Redis 的测试在 `tests/redis.rs` 的 `#[container]` 层）
//!
//! 运行：`cargo test -p hygiea-redis --features fred --test component`

#![cfg(feature = "fred")]

use hygiea_core::app::{Component, Name, ResourceId};
use hygiea_redis::{FredRedisPool, RedisComponent, RedisConfig};

/// 具名构造：provides 只有组件名下的 FredRedisPool
#[test]
fn component_provides_named_pool() {
    let name = Name::from("cache");
    let component = RedisComponent::build(name.clone(), RedisConfig::default());
    assert_eq!(
        component.provides(),
        vec![ResourceId::named::<FredRedisPool>(name)]
    );
}
