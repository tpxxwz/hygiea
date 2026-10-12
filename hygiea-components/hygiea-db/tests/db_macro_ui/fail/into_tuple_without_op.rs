// into_tuple 之后拿不到表名，不写 op 编译报错
use hygiea_db::db;
use sea_orm::{ConnectionTrait, EntityTrait, QuerySelect};

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

async fn ids(db: &impl ConnectionTrait) -> hygiea_core::Result<Vec<i32>> {
    db!(cake::Entity::find().select_only().column(cake::Column::Id).into_tuple().all(db))
}

fn main() {}
