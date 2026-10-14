//! feature 组合的 cfg 别名：源码里写 `#[cfg(sqlx_lock_pg)]`，不用每处重复 `all(feature = "lock", any(..), ..)`。
//! 规则只写在这里，一行一个别名。两层：
//! - 按驱动（`sqlx_*`）：这个驱动对这个数据库 / 功能的实现要不要编译，给各驱动目录用
//! - 按数据库（`live_config_*`、`lock_*`）：任意一个驱动有实现就算，给和驱动无关的规则、SQL 用
//!
//! 现在只有 sqlx 一个驱动，两层看起来重复；加别的驱动时在按数据库的别名里加上它（见 research/db-drivers 分支）

fn main() {
    let on = |feature: &str| {
        let var = format!("CARGO_FEATURE_{}", feature.to_uppercase().replace('-', "_"));
        std::env::var_os(var).is_some()
    };
    // 驱动能连哪个数据库：sqlx、SeaORM（底下是 sqlx）都算 sqlx
    let sqlx_pg = (on("sqlx") || on("seaorm")) && on("postgres");
    let sqlx_sqlite = on("sqlx") && on("sqlite");
    let live_config = on("live-config");
    let lock = on("lock");
    let lock_row = on("lock-row");

    let aliases = [
        ("sqlx_pg", sqlx_pg),
        ("sqlx_sqlite", sqlx_sqlite),
        // 按驱动的功能实现
        ("sqlx_live_config", live_config && (sqlx_pg || sqlx_sqlite)),
        ("sqlx_lock_pg", lock && sqlx_pg),
        ("sqlx_lock_row", lock_row && sqlx_pg),
        // 按数据库：任意驱动有实现
        ("live_config_pg", live_config && sqlx_pg),
        ("live_config_sqlite", live_config && sqlx_sqlite),
        ("live_config_any", live_config && (sqlx_pg || sqlx_sqlite)),
        ("lock_pg", lock && sqlx_pg),
        ("lock_row_pg", lock_row && sqlx_pg),
        ("lock_any", lock && sqlx_pg),
    ];
    for (name, enabled) in aliases {
        println!("cargo::rustc-check-cfg=cfg({name})");
        if enabled {
            println!("cargo::rustc-cfg={name}");
        }
    }
}
