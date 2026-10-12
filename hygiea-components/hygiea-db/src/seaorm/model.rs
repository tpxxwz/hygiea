//! Model 和 ActiveModel 之间的转换

use sea_orm::{ActiveModelTrait, IntoActiveModel};

/// 一组借来的 Model 逐个克隆转成 ActiveModel，直接交给 `insert_many`（它收 `IntoIterator`）
///
/// ```ignore
/// db!(user::Entity::insert_many(active_models(&users)).exec_without_returning(db))?;
/// ```
pub fn active_models<M, A>(models: &[M]) -> impl Iterator<Item = A> + use<'_, M, A>
where
    M: Clone + IntoActiveModel<A>,
    A: ActiveModelTrait,
{
    models
        .iter()
        .cloned()
        .map(IntoActiveModel::into_active_model)
}
