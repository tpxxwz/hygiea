//! 分页查询：取一页再查总数，返回 [`Page`]

use hygiea_core::{Result, ResultExt, err};
use sea_orm::{ConnectionTrait, EntityTrait, PaginatorTrait, Select};

use super::db::table;
use crate::{Page, PageRequest, RepoErr};

/// 执行调用方拼好的查询，取一页，再查总数，见 [`Page`]。查询要带排序，否则同一页两次结果可能不一样；
/// 两条 SQL 要读同一个快照时在 [`transaction_snapshot`](crate::seaorm::transaction_snapshot) 里调。
/// 失败是 [`RepoErr::QueryFailed`]，`op` 带页号、页大小和失败的是哪一条，比如 `find_page user page 3 size 20, fetch items`
///
/// ```ignore
/// let select = user::Entity::find()
///     .filter(user::Column::GroupId.eq(group_id))
///     .order_by_asc(user::Column::Id);
/// let page = seaorm::find_page(&db, select, PageRequest::new(page, page_size)?).await?;
/// ```
pub async fn find_page<E>(
    db: &impl ConnectionTrait,
    select: Select<E>,
    request: PageRequest,
) -> Result<Page<E::Model>>
where
    E: EntityTrait,
    E::Model: Sync,
{
    let op = |step: &str| {
        let (page, size) = (request.page(), request.page_size());
        format!("find_page {} page {page} size {size}, {step}", table::<E>())
    };
    // PageRequest 构造时校验过页大小不是 0，paginate 收到 0 会 panic
    let paginator = select.paginate(db, u64::from(request.page_size()));
    let items = paginator
        .fetch_page(u64::from(request.page()))
        .await
        .wrap_err(|| err!(RepoErr::QueryFailed, op("fetch items")))?;
    let total_items = paginator
        .num_items()
        .await
        .wrap_err(|| err!(RepoErr::QueryFailed, op("count items")))?;
    Ok(Page::new(items, request, total_items))
}
