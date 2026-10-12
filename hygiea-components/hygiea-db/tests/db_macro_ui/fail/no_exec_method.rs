// 最后一步不是执行方法（只拼了语句）编译报错。宏在展开时就报错，下面的 import 用不上
#![allow(unused_imports)]

use hygiea_db::db;
use sea_orm::EntityTrait;

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

async fn find() -> hygiea_core::Result<()> {
    db!(cake::Entity::find());
    Ok(())
}

fn main() {}
