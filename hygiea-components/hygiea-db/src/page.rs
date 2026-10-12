//! 分页的请求 [`PageRequest`] 和结果 [`Page`]，开 `sqlx` 或 `seaorm` 就有，和框架、数据库无关
//!
//! 按 LIMIT / OFFSET 分页，页号从 0 开始，和 SeaORM 的 `Paginator::fetch_page` 一致。
//! 页号、页大小都是 `u32`。[`PageRequest::new`] 校验：页大小是 0 报 [`BaseErr::NotPositive`]，
//! OFFSET（页号 × 页大小，要绑成 `i64`）超过 `i64::MAX` 报 [`RepoErr::PageOutOfRange`]；反序列化走同一个校验，
//! 构造出来的 [`PageRequest`] 算 LIMIT / OFFSET 不会溢出。两边都 `Serialize` / `Deserialize`。
//!
//! 一页数据和总数是两条 SQL，中间有写入时总数可能和数据对不上，要一致就在快照事务里查
//! （SeaORM 是 `seaorm::transaction_snapshot`）。查询要带 `ORDER BY`，否则同一页两次结果可能不一样。
//!
//! ```ignore
//! // SeaORM
//! let select = user::Entity::find().order_by_asc(user::Column::Id);
//! let page = seaorm::find_page(&db, select, request).await?;
//!
//! // sqlx：LIMIT / OFFSET 绑 i64
//! let items: Vec<User> = sqlx::query_as("SELECT * FROM users ORDER BY id LIMIT $1 OFFSET $2")
//!     .bind(request.limit())
//!     .bind(request.offset())
//!     .fetch_all(&pool)
//!     .await?;
//! let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users").fetch_one(&pool).await?;
//! // COUNT 不会是负数
//! let page = Page::new(items, request, total.unsigned_abs());
//! ```

use hygiea_core::{BaseErr, HyErr, Result, ensure, err};
use serde::{Deserialize, Serialize};

use crate::RepoErr;

/// 要第几页（从 0 开始）、一页几条。字段私有，只能经 [`new`](Self::new) 或反序列化构造，保证 OFFSET 放得进 `i64`
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawPageRequest")]
pub struct PageRequest {
    page: u32,
    page_size: u32,
}

impl PageRequest {
    /// 页大小是 0 报 [`BaseErr::NotPositive`]，页号 × 页大小超过 `i64::MAX` 报 [`RepoErr::PageOutOfRange`]
    pub fn new(page: u32, page_size: u32) -> Result<Self> {
        ensure!(page_size > 0, BaseErr::NotPositive, {
            "param": "page_size",
            "op": "PageRequest::new",
        });
        // u32 × u32 放得进 u64，不会溢出
        let offset = u64::from(page) * u64::from(page_size);
        if i64::try_from(offset).is_err() {
            return Err(err!(RepoErr::PageOutOfRange, {
                "page": page,
                "page_size": page_size,
            }));
        }
        Ok(Self { page, page_size })
    }

    pub fn page(&self) -> u32 {
        self.page
    }

    pub fn page_size(&self) -> u32 {
        self.page_size
    }

    /// LIMIT 的值，即页大小
    pub fn limit(&self) -> i64 {
        i64::from(self.page_size)
    }

    /// OFFSET 的值：页号 × 页大小，[`new`](Self::new) 里校验过不会溢出
    pub fn offset(&self) -> i64 {
        i64::from(self.page) * self.limit()
    }
}

/// 反序列化先读成这个，再经 [`PageRequest::new`] 校验
#[derive(Deserialize)]
struct RawPageRequest {
    page: u32,
    page_size: u32,
}

impl TryFrom<RawPageRequest> for PageRequest {
    type Error = HyErr;

    fn try_from(raw: RawPageRequest) -> Result<Self> {
        Self::new(raw.page, raw.page_size)
    }
}

/// 一页的数据和总数。`total_pages` 按 `total_items` 和页大小算，没有数据时是 0；页号超出时 `items` 是空的
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub page: u32,
    pub page_size: u32,
    pub total_items: u64,
    pub total_pages: u64,
}

impl<T> Page<T> {
    pub fn new(items: Vec<T>, request: PageRequest, total_items: u64) -> Self {
        Self {
            items,
            page: request.page,
            page_size: request.page_size,
            total_items,
            total_pages: total_items.div_ceil(u64::from(request.page_size)),
        }
    }

    /// 转换每条数据，分页信息不变，比如实体转成 VO
    pub fn map<U>(self, f: impl FnMut(T) -> U) -> Page<U> {
        Page {
            items: self.items.into_iter().map(f).collect(),
            page: self.page,
            page_size: self.page_size,
            total_items: self.total_items,
            total_pages: self.total_pages,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(page: u32, page_size: u32) -> PageRequest {
        PageRequest::new(page, page_size).unwrap()
    }

    #[test]
    fn limit_and_offset() {
        assert_eq!((request(0, 20).limit(), request(0, 20).offset()), (20, 0));
        assert_eq!(request(3, 20).offset(), 60);
    }

    #[test]
    fn new_rejects_zero_page_size() {
        let e = PageRequest::new(0, 0).unwrap_err();
        assert!(e.is(BaseErr::NotPositive), "{e:#}");
        assert!(
            format!("{e}").contains("page_size must be greater than 0: PageRequest::new"),
            "{e}"
        );
    }

    /// 页号 × 页大小刚好不超过 i64::MAX 能构造，多一点就报错
    #[test]
    fn new_rejects_offset_over_i64() {
        let max = request(u32::MAX, 1 << 31);
        assert_eq!(max.offset(), i64::from(u32::MAX) << 31);
        let e = PageRequest::new(u32::MAX, (1 << 31) + 1).unwrap_err();
        assert!(e.is(RepoErr::PageOutOfRange), "{e:#}");
        assert!(
            format!("{e}").contains("page 4294967295, page_size 2147483649"),
            "{e}"
        );
    }

    #[test]
    fn total_pages_rounds_up() {
        assert_eq!(Page::<i32>::new(vec![], request(0, 10), 0).total_pages, 0);
        assert_eq!(Page::<i32>::new(vec![], request(0, 10), 10).total_pages, 1);
        assert_eq!(Page::<i32>::new(vec![], request(0, 10), 11).total_pages, 2);
    }

    #[test]
    fn map_keeps_page_info() {
        let page = Page::new(vec![1, 2], request(1, 2), 5).map(|n| n.to_string());
        assert_eq!(
            page,
            Page::new(vec!["1".into(), "2".into()], request(1, 2), 5)
        );
        assert_eq!(page.total_pages, 3);
    }

    #[test]
    fn serde_rejects_zero_page_size() {
        let parsed: PageRequest = serde_json::from_str(r#"{"page":1,"page_size":20}"#).unwrap();
        assert_eq!(parsed, request(1, 20));
        let e = serde_json::from_str::<PageRequest>(r#"{"page":1,"page_size":0}"#).unwrap_err();
        assert!(
            e.to_string().contains("page_size must be greater than 0"),
            "{e}"
        );
        let e =
            serde_json::from_str::<PageRequest>(r#"{"page":4294967295,"page_size":4294967295}"#)
                .unwrap_err();
        assert!(e.to_string().contains("page out of range"), "{e}");

        let json = serde_json::to_string(&Page::new(vec![7], request(0, 1), 3)).unwrap();
        assert_eq!(
            json,
            r#"{"items":[7],"page":0,"page_size":1,"total_items":3,"total_pages":3}"#
        );
    }
}
