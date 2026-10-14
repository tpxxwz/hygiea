//! 分布式锁：PG 连接池组件 `uses` 带 4 时放进 Resources 的 `DbLock` 实现 `DistributedLock`。这里不经过 Registry，
//! 用 `DbResources::from_config` 直接建两份（`uses = 5`：业务池 + 锁，锁和业务池共用连接），模拟两个实例抢同一把锁。
//!
//! 1. 注册锁类型（`#[derive(hy_lock_type)]`）和字符串 key 的模板（`#[derive(hy_lock_key)]`），
//!    编号 = 项目前缀（Cargo.toml 的 `err_code_project_prefix`，这里是 001）+ 模块前缀 + code
//! 2. 锁类型 + id：`try_lock_i32`，PG 上是原生的 `pg_try_advisory_xact_lock(int, int)`
//! 3. 任意 i64：`try_lock_i64`，不用注册
//! 4. 字符串 key：`lock_key!` 生成。本示例开了 `db-lock-row`，PG 上是在锁表 `example_lock` 里插入不提交的行（启动时建表）；
//!    没开 `db-lock-row` 时 `try_lock` 返回 `LockErr::Unsupported`；不想建表的用第 6 步，自己显式 `LockKeyI64::xxh3_64`
//! 5. 阻塞版：拿不到就等，最多等 timeout；guard 没 release 就 drop 时在后台释放
//! 6. 哈希降级：`LockKeyI64::xxh3_64(&lock_key!(..))?` 交给 `try_lock_i64`，不用锁表；有损（不同 key 可能撞到同一个数，
//!    多锁不会少锁），和锁表的那把锁不互斥
//!
//! 用 Docker 起一个临时的 postgres（hygiea-test 的容器），跑完自动删掉：
//!
//! ```bash
//! cargo run -p hygiea-examples --example db_lock
//! ```

use std::time::Duration;

use hygiea::Result;
use hygiea::app::RegistryConfig;
use hygiea::db::{DbLock, DbResources, DbUses, PgConnectConfig, SqlxPgConfig, SqlxPgPool};
use hygiea::lock::{DistributedLock, LockKeyI64, hy_lock_key, hy_lock_type, lock_key};
use hygiea_test::container::{ContainerSpec, RunningContainer};

// ---- 注册：锁类型、字符串 key 共用一套编号，同模块里 code 不能重复 ----

/// 锁类型：`try_lock_i32` 的第一个参数，第二个参数是运行时的 id
#[derive(hy_lock_type)]
#[lock_module_prefix = "01"]
enum OrderLockType {
    #[lock_type(code = "0001")]
    Pay, // 001 01 0001 = 1010001
}

/// 字符串 key 的模板：`lock_key!` 的写法和 err! 一样，模板和参数对不上编译报错
#[derive(hy_lock_key)]
#[lock_module_prefix = "01"]
enum OrderLockKey {
    #[lock_key(code = "0002", tpl = "daily_settle")]
    DailySettle,
    #[lock_key(code = "0003", tpl = "export:{{ user }}")]
    Export,
}

const PASSWORD: &str = "hygiea-example";
const PORT: u16 = 5432;

#[tokio::main]
async fn main() -> Result<()> {
    let server = ContainerSpec::new("postgres", "18")
        .env("POSTGRES_PASSWORD", PASSWORD)
        .port(PORT)
        .wait_log_times("database system is ready to accept connections", 2)
        .start()
        .await;
    // 两个实例，各自的锁；a 的业务池用来查锁表
    let (pool_a, a) = instance(&server).await?;
    let (_, b) = instance(&server).await?;

    // ---- 锁类型 + id ----
    let guard = a.try_lock_i32(OrderLockType::Pay, 42u32).await?;
    println!("a 拿锁 Pay/42：{}", guard.is_some());
    println!(
        "b 拿锁 Pay/42：{}",
        b.try_lock_i32(OrderLockType::Pay, 42u32).await?.is_some()
    );
    println!(
        "b 拿锁 Pay/43：{}",
        b.try_lock_i32(OrderLockType::Pay, 43u32).await?.is_some()
    );
    println!(
        "a 再拿 Pay/42（不可重入）：{}",
        a.try_lock_i32(OrderLockType::Pay, 42u32).await?.is_some()
    );
    if let Some(guard) = guard {
        guard.release().await?;
    }
    println!(
        "a 释放后 b 拿锁 Pay/42：{}",
        b.try_lock_i32(OrderLockType::Pay, 42u32).await?.is_some()
    );

    // ---- 任意 i64 ----
    let held = a.try_lock_i64(1_000_000_007).await?;
    println!("\na 拿锁 i64：{:?}", held.as_ref().map(|g| g.key()));
    println!(
        "b 拿锁 i64：{}",
        b.try_lock_i64(1_000_000_007).await?.is_some()
    );
    // 没 release 就 drop：后台回滚事务释放锁，稍等一下 b 就能拿到
    drop(held);
    tokio::time::sleep(Duration::from_millis(100)).await;
    println!(
        "a 的 guard drop 后 b 拿锁 i64：{}",
        b.try_lock_i64(1_000_000_007).await?.is_some()
    );

    // ---- 字符串 key ----
    let settle = a.try_lock(lock_key!(OrderLockKey::DailySettle)).await?;
    println!("\na 拿锁：{:?}", settle.as_ref().map(|g| g.key()));
    println!(
        "b 拿锁 daily_settle：{}",
        b.try_lock(lock_key!(OrderLockKey::DailySettle))
            .await?
            .is_some()
    );
    let export = b.try_lock(lock_key!(OrderLockKey::Export, "alice")).await?;
    println!("b 拿锁：{:?}", export.as_ref().map(|g| g.key()));

    // 持有中的行没提交，别的会话查不到；释放时回滚，行跟着撤掉，表一直是空的
    let rows: Vec<String> = sqlx::query_scalar("SELECT key FROM example_lock ORDER BY key")
        .fetch_all(&*pool_a)
        .await
        .map_err(|e| hygiea::err!(hygiea::BaseErr::SysErr).with_source(e))?;
    println!("锁表 example_lock 里看得到的行（持有中也是空的）：{rows:?}");

    // ---- 阻塞版：a 半秒后释放，b 最多等 3 秒 ----
    let start = tokio::time::Instant::now();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Some(settle) = settle {
            let _ = settle.release().await;
        }
    });
    let waited = b
        .lock(lock_key!(OrderLockKey::DailySettle), Duration::from_secs(3))
        .await?;
    println!(
        "\nb 等锁 daily_settle：{}，等了 {:?}",
        waited.is_some(),
        start.elapsed()
    );
    // 一直拿不到：超时返回 None
    let timeout = a
        .lock(
            lock_key!(OrderLockKey::DailySettle),
            Duration::from_millis(300),
        )
        .await?;
    println!("a 等锁 daily_settle 300ms：{}", timeout.is_some());

    // ---- 哈希降级：字符串 key 哈希成 i64，走 advisory lock ----
    let key = LockKeyI64::xxh3_64(&lock_key!(OrderLockKey::Export, "bob"))?;
    let hashed = a.try_lock_i64(key).await?;
    println!(
        "
a 拿锁（哈希成 i64）：{:?}",
        hashed.as_ref().map(|g| g.key())
    );
    println!("b 拿同一个哈希：{}", b.try_lock_i64(key).await?.is_some());
    // 和锁表里同一个 key 的那把锁是两样东西，互不互斥：同一个 key 不要两种方式混用
    println!(
        "b 用锁表拿 export:bob：{}（和哈希的那把不互斥）",
        b.try_lock(lock_key!(OrderLockKey::Export, "bob"))
            .await?
            .is_some()
    );
    Ok(())
}

/// 一个实例的业务池和锁。应用名 example：锁表是 example_lock
async fn instance(server: &RunningContainer) -> Result<(SqlxPgPool, DbLock)> {
    let config = SqlxPgConfig {
        uses: DbUses::try_from(DbUses::POOL | DbUses::LOCK)?,
        connect: PgConnectConfig {
            host: server.host().to_string(),
            port: server.port(PORT),
            password: PASSWORD.to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    let registry_config = RegistryConfig {
        app_name: "example".to_string(),
        ..Default::default()
    };
    let db = DbResources::from_config(&config, &registry_config).await?;
    match (db.pool, db.lock) {
        (Some(pool), Some(lock)) => Ok((pool, lock)),
        // uses 带了 1、4，from_config 成功时两样都有
        _ => Err(hygiea::err!(hygiea::BaseErr::SysErr)),
    }
}
