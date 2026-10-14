//! feature 组合的 cfg 别名：源码里写 `#[cfg(sqlx_lock_pg)]`，不用每处重复 `all(feature = "lock", any(..), ..)`。
//! 规则只写在这里，一行一个别名。两层：
//! - 按驱动（`sqlx_*`、`toasty_*`、`tokio_pg*`）：这个驱动对这个数据库 / 功能的实现要不要编译，给各驱动目录用
//! - 按数据库（`live_config_*`、`lock_*`）：任意一个驱动有实现就算，给和驱动无关的规则、SQL 用

fn main() {
    let on = |feature: &str| {
        let var = format!("CARGO_FEATURE_{}", feature.to_uppercase().replace('-', "_"));
        std::env::var_os(var).is_some()
    };
    // 驱动能连哪个数据库：sqlx、SeaORM（底下是 sqlx）都算 sqlx
    let sqlx_pg = (on("sqlx") || on("seaorm")) && on("postgres");
    let sqlx_sqlite = on("sqlx") && on("sqlite");
    let sqlx_mysql = on("sqlx") && on("mysql");
    let toasty_pg = on("toasty") && on("postgres");
    let tokio_pg = on("tokio-postgres") && on("postgres");
    let live_config = on("live-config");
    let lock = on("lock");
    let lock_row = on("lock-row");

    let aliases = [
        ("sqlx_pg", sqlx_pg),
        ("sqlx_sqlite", sqlx_sqlite),
        ("sqlx_mysql", sqlx_mysql),
        ("toasty_pg", toasty_pg),
        ("tokio_pg", tokio_pg),
        // 按驱动的功能实现
        (
            "sqlx_live_config",
            live_config && (sqlx_pg || sqlx_sqlite || sqlx_mysql),
        ),
        ("sqlx_lock_pg", lock && sqlx_pg),
        ("sqlx_lock_mysql", lock && sqlx_mysql),
        ("sqlx_lock_row", lock_row && sqlx_pg),
        ("toasty_live_config", live_config && toasty_pg),
        ("toasty_lock_pg", lock && toasty_pg),
        ("toasty_lock_row", lock_row && toasty_pg),
        ("tokio_pg_live_config", live_config && tokio_pg),
        ("tokio_pg_lock", lock && tokio_pg),
        ("tokio_pg_lock_row", lock_row && tokio_pg),
        // 按数据库：任意驱动有实现
        (
            "live_config_pg",
            live_config && (sqlx_pg || toasty_pg || tokio_pg),
        ),
        ("live_config_sqlite", live_config && sqlx_sqlite),
        ("live_config_mysql", live_config && sqlx_mysql),
        (
            "live_config_any",
            live_config && (sqlx_pg || toasty_pg || tokio_pg || sqlx_sqlite || sqlx_mysql),
        ),
        ("lock_pg", lock && (sqlx_pg || toasty_pg || tokio_pg)),
        ("lock_mysql", lock && sqlx_mysql),
        (
            "lock_row_pg",
            lock_row && (sqlx_pg || toasty_pg || tokio_pg),
        ),
        (
            "lock_any",
            lock && (sqlx_pg || toasty_pg || tokio_pg || sqlx_mysql),
        ),
    ];
    for (name, enabled) in aliases {
        println!("cargo::rustc-check-cfg=cfg({name})");
        if enabled {
            println!("cargo::rustc-cfg={name}");
        }
    }
}
