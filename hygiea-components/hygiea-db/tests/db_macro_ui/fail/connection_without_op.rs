// 连接上直接执行拿不到表名，不写 op 编译报错
use hygiea_db::db;
use sea_orm::ConnectionTrait;

async fn raw(db: &impl ConnectionTrait) -> hygiea_core::Result<()> {
    db!(db.execute_unprepared("SELECT 1"))?;
    Ok(())
}

fn main() {}
