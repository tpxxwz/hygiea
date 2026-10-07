//! 数据库里的 UTC 时间列类型 [`HyUtcDateTime`]，开 `sqlx` 或 `seaorm` 就有，和数据库无关
//!
//! sqlx 和 SeaORM 只认 time 的 `OffsetDateTime` 和 `PrimitiveDateTime`，不认 [`UtcDateTime`]，
//! 孤儿规则又不允许直接给它实现框架的 trait，所以包一层。同一个类型按 feature 实现两边的 trait
//! （实现在 `sqlx/mod.rs`、`seaorm/mod.rs`），两个框架同时开也是同一个类型。读写时和 `OffsetDateTime` 互转，支持哪些数据库、对应什么列类型
//! 都跟着 `OffsetDateTime` 走（PostgreSQL 是 `timestamptz`）。
//!
//! ```ignore
//! // sqlx
//! let at: HyUtcDateTime = sqlx::query_scalar("SELECT create_at FROM record")
//!     .fetch_one(&pool)
//!     .await?;
//! sqlx::query("INSERT INTO record (create_at) VALUES ($1)")
//!     .bind(HyUtcDateTime(now_utc()))
//!     .execute(&pool)
//!     .await?;
//!
//! // SeaORM
//! #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
//! #[sea_orm(table_name = "record")]
//! pub struct Model {
//!     #[sea_orm(primary_key)]
//!     pub id: i64,
//!     pub create_at: HyUtcDateTime,
//!     pub delete_at: Option<HyUtcDateTime>,
//! }
//! ```

use hygiea_core::datetime::{UtcDateTime, rfc3339_utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::ops::{Deref, DerefMut};

/// 数据库里的 UTC 时间，列类型同 `OffsetDateTime`。`.0` 或 `From` 取出 [`UtcDateTime`]；
/// 实现了 `Deref`，`UtcDateTime` 的方法和 hygiea 的扩展方法可以直接调，返回的是 `UtcDateTime`
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HyUtcDateTime(pub UtcDateTime);

impl From<UtcDateTime> for HyUtcDateTime {
    fn from(value: UtcDateTime) -> Self {
        Self(value)
    }
}

impl From<HyUtcDateTime> for UtcDateTime {
    fn from(value: HyUtcDateTime) -> Self {
        value.0
    }
}

impl Deref for HyUtcDateTime {
    type Target = UtcDateTime;

    fn deref(&self) -> &UtcDateTime {
        &self.0
    }
}

impl DerefMut for HyUtcDateTime {
    fn deref_mut(&mut self) -> &mut UtcDateTime {
        &mut self.0
    }
}

/// 序列化成带 `Z` 的 RFC 3339，读的时候带任意 offset 都换算成 UTC，同 [`rfc3339_utc`]
impl Serialize for HyUtcDateTime {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        rfc3339_utc::serialize(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for HyUtcDateTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        rfc3339_utc::deserialize(deserializer).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hygiea_core::datetime::HygieaDateTimeExt;

    fn utc(s: &str) -> UtcDateTime {
        UtcDateTime::parse_ext_rfc3339(s).unwrap()
    }

    #[test]
    fn deref_calls_utc_date_time_methods() {
        use hygiea_core::datetime::HygieaUtcDateTimeExt;

        let mut at = HyUtcDateTime(utc("2026-10-05T12:57:24.719Z"));
        assert_eq!(at.year(), 2026);
        assert_eq!(at.start_of_day(), utc("2026-10-05T00:00:00Z"));
        *at = utc("2026-10-06T00:00:00Z");
        assert_eq!(at.0, utc("2026-10-06T00:00:00Z"));
    }

    #[test]
    fn serde_uses_rfc3339_utc() {
        let at = HyUtcDateTime(utc("2026-10-05T12:57:24.719Z"));
        let json = serde_json::to_string(&at).unwrap();
        assert_eq!(json, r#""2026-10-05T12:57:24.719Z""#);
        let back: HyUtcDateTime =
            serde_json::from_str(r#""2026-10-05T20:57:24.719+08:00""#).unwrap();
        assert_eq!(back, at);
    }
}
