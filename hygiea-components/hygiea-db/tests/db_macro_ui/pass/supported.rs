// db! 支持的写法都能编译：语句起点、ActiveModel / Model 的方法、写了 op 的 into_* / 分页器 / 连接上直接执行。
// 函数不调用，只看类型检查过不过
#![allow(dead_code)]

use hygiea_db::db;
use hygiea_db::seaorm::{active_models, transaction};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, IntoActiveModel,
    ModelTrait, PaginatorTrait, QueryFilter, QuerySelect, Set,
};

mod cake {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "cake")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub name: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

async fn select(db: &impl ConnectionTrait) -> hygiea_core::Result<()> {
    let _: Option<cake::Model> = db!(cake::Entity::find_by_id(1).one(db))?;
    let _: Vec<cake::Model> = db!(cake::Entity::find().filter(cake::Column::Name.eq("a")).all(db))?;
    let _: u64 = db!(cake::Entity::find().count(db))?;
    let select = cake::Entity::find().lock_exclusive();
    let _ = db!(select.one(db), "lock cake")?;
    Ok(())
}

async fn write(db: &impl ConnectionTrait, models: &[cake::Model]) -> hygiea_core::Result<()> {
    let am = cake::ActiveModel { name: Set("a".into()), ..Default::default() };
    db!(cake::Entity::insert(am.clone()).exec(db))?;
    db!(cake::Entity::insert_many(active_models(models)).on_conflict_do_nothing().exec_without_returning(db))?;
    db!(cake::Entity::update_many().filter(cake::Column::Id.eq(1)).exec(db))?;
    db!(cake::Entity::delete_by_id(1).exec(db))?;
    db!(cake::Entity::delete_many().filter(cake::Column::Id.eq(1)).exec(db))?;
    Ok(())
}

async fn methods(db: &impl ConnectionTrait, model: cake::Model) -> hygiea_core::Result<()> {
    let am = model.clone().into_active_model();
    db!(am.clone().insert(db))?;
    db!(am.clone().update(db))?;
    db!(am.clone().save(db))?;
    db!(am.delete(db), "remove cake")?;
    db!(model.delete(db))?;
    Ok(())
}

async fn without_table(db: &impl ConnectionTrait) -> hygiea_core::Result<()> {
    let _: Vec<i32> = db!(
        cake::Entity::find().select_only().column(cake::Column::Id).into_tuple().all(db),
        "cake ids"
    )?;
    let _ = db!(cake::Entity::find().into_json().all(db), "cake json")?;
    let _ = db!(cake::Entity::find().paginate(db, 10).fetch_page(0), "cake page")?;
    db!(db.execute_unprepared("SELECT 1"), "select one")?;
    Ok(())
}

async fn in_transaction(pool: &DatabaseConnection) -> hygiea_core::Result<()> {
    transaction(pool, "lock timeout", async |txn| {
        db!(txn.execute_unprepared("SET LOCAL lock_timeout = '3s'"), "set lock timeout")?;
        db!(cake::Entity::find().all(txn))?;
        Ok(())
    })
    .await
}

fn main() {}
